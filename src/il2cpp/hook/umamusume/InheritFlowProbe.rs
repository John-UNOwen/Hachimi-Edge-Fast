use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicBool, AtomicI64, AtomicUsize};

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed,
            ProbeSuite,
            TrainingCuttProbe::{class_for_label, report_due, CutProbe, REPORT_INTERVAL_SECS},
        },
        types::*,
    },
};

// Observe only doors on the inheritance (succession) cut-in: the view that plays it, the helper that ends it, and
// the view change that enters it.
//
// The stall this was built for is the inheritance screen, view 1501, which ran for minutes with no progress in
// earlier runs. The existing census watches the training cut-in and the story cut-in. Neither names this view's
// own doors, so a stall on 1501 could not be said to be inside any one of its steps. This module splits the
// inheritance visit into three legs, the prepare, the cut and the tail, and reports each one's runs, mean and worst
// wall time, plus how long the leg that is open right now has been open. A stall then shows as one leg with an open
// time that keeps growing, which is the number the stall census could not give.
//
// Names come from the live dump: `Gallop.SingleModeSuccessionEventViewController` (`PrepareCutIn/0`, `PlayCutIn/0`,
// `OnPauseCutt/0`, `OnEndCutt/1` taking the timeline controller, `OnCompleteFadeOut/0`, `DestroyCutin/0`),
// `Gallop.SingleModeSuccessionCutInHelper::OnEndCutIn/0`, and `Gallop.SingleModeChangeViewManager::
// ChangeViewSuccessionCutAndStory/1` taking the entry info. All are instance methods.
//
// Nothing here changes an argument or a return value, and nothing presses a button or ends a cut. The whole module
// is behind `debug_mode`, so a player who has that off gets no hook at all.

const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const ONE_INFO: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;

static PREPARE_CUT_IN: CutProbe = CutProbe::counted("SingleModeSuccessionEventViewController::PrepareCutIn()");
static PLAY_CUT_IN: CutProbe = CutProbe::counted("SingleModeSuccessionEventViewController::PlayCutIn()");
static PAUSE_CUTT: CutProbe = CutProbe::counted("SingleModeSuccessionEventViewController::OnPauseCutt()");
static END_CUTT: CutProbe = CutProbe::counted("SingleModeSuccessionEventViewController::OnEndCutt(controller)");
static FADE_OUT_DONE: CutProbe = CutProbe::counted("SingleModeSuccessionEventViewController::OnCompleteFadeOut()");
static DESTROY_CUT_IN: CutProbe = CutProbe::counted("SingleModeSuccessionEventViewController::DestroyCutin()");
static HELPER_END_CUT_IN: CutProbe = CutProbe::counted("SingleModeSuccessionCutInHelper::OnEndCutIn()");
static CHANGE_TO_SUCCESSION_CUT: CutProbe =
    CutProbe::counted("SingleModeChangeViewManager::ChangeViewSuccessionCutAndStory(entry)");

pub(crate) static PROBES: [&CutProbe; 8] = [
    &PREPARE_CUT_IN,
    &PLAY_CUT_IN,
    &PAUSE_CUTT,
    &END_CUTT,
    &FADE_OUT_DONE,
    &DESTROY_CUT_IN,
    &HELPER_END_CUT_IN,
    &CHANGE_TO_SUCCESSION_CUT,
];

// The three legs of one inheritance visit. Each opens on one door and closes on the next.
//   prepare: PrepareCutIn -> PlayCutIn
//   cut:     PlayCutIn -> OnEndCutt
//   tail:    OnEndCutt -> OnCompleteFadeOut
static PREPARE_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static CUT_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static TAIL_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static PREPARE_LEG: Leg = Leg::new();
static CUT_LEG: Leg = Leg::new();
static TAIL_LEG: Leg = Leg::new();

static ARMED: AtomicBool = AtomicBool::new(false);
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

// A leg's wall time, or nothing when the leg never opened or the clock went back.
pub(crate) fn leg_us(open_us: i64, now_us: i64) -> Option<i64> {
    if open_us < 0 || now_us < open_us {
        return None;
    }

    Some(now_us - open_us)
}

// How many runs a leg has closed, their total and their worst wall time, all in microseconds.
pub(crate) struct Leg {
    runs: AtomicUsize,
    total_us: AtomicI64,
    worst_us: AtomicI64,
}

impl Leg {
    pub(crate) const fn new() -> Self {
        Self { runs: AtomicUsize::new(0), total_us: AtomicI64::new(0), worst_us: AtomicI64::new(0) }
    }

    pub(crate) fn record(&self, us: i64) {
        self.runs.fetch_add(1, atomic::Ordering::Relaxed);
        self.total_us.fetch_add(us, atomic::Ordering::Relaxed);
        self.worst_us.fetch_max(us, atomic::Ordering::Relaxed);
    }

    fn line(&self, name: &str) -> String {
        match self.runs.load(atomic::Ordering::Relaxed) {
            0 => format!("{name} 0 runs"),
            n => format!(
                "{name} {n} runs mean {:.1} ms worst {} ms",
                self.total_us.load(atomic::Ordering::Relaxed) as f64 / n as f64 / 1_000.0,
                self.worst_us.load(atomic::Ordering::Relaxed) / 1_000
            ),
        }
    }
}

// Opens a leg on the door that starts it. A leg opened twice keeps the later open, because the earlier one was
// never closed by its own door and is not a visit the game finished.
fn open_leg(open: &AtomicI64, now: i64) {
    open.store(now, atomic::Ordering::Relaxed);
}

// Closes a leg on the door that ends it. A close with no open is a door reached out of order and adds nothing.
fn close_leg(open: &AtomicI64, leg: &Leg, now: i64) {
    if let Some(us) = leg_us(open.swap(-1, atomic::Ordering::Relaxed), now) {
        leg.record(us);
    }
}

type InheritVoidFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    SuccessionEvent_PrepareCutIn(this: *mut Il2CppObject) {
            PREPARE_CUT_IN.count();
        open_leg(&PREPARE_OPEN_US, ProbeSuite::now_us());

        get_orig_fn!(SuccessionEvent_PrepareCutIn, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_PrepareCutIn, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_PlayCutIn(this: *mut Il2CppObject) {
            PLAY_CUT_IN.count();
        let now = ProbeSuite::now_us();
        close_leg(&PREPARE_OPEN_US, &PREPARE_LEG, now);
        open_leg(&CUT_OPEN_US, now);

        get_orig_fn!(SuccessionEvent_PlayCutIn, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_PlayCutIn, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_OnPauseCutt(this: *mut Il2CppObject) {
            PAUSE_CUTT.count();

        get_orig_fn!(SuccessionEvent_OnPauseCutt, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_OnPauseCutt, InheritVoidFn)(this)
    }
}

type OnEndCuttFn = extern "C" fn(this: *mut Il2CppObject, controller: *mut Il2CppObject);
def_detour! {
    SuccessionEvent_OnEndCutt(this: *mut Il2CppObject, controller: *mut Il2CppObject) {
            END_CUTT.count();
        let now = ProbeSuite::now_us();
        close_leg(&CUT_OPEN_US, &CUT_LEG, now);
        open_leg(&TAIL_OPEN_US, now);

        get_orig_fn!(SuccessionEvent_OnEndCutt, OnEndCuttFn)(this, controller);
    }
    bail {
                get_orig_fn!(SuccessionEvent_OnEndCutt, OnEndCuttFn)(this, controller)
    }
}

def_detour! {
    SuccessionEvent_OnCompleteFadeOut(this: *mut Il2CppObject) {
            FADE_OUT_DONE.count();
        close_leg(&TAIL_OPEN_US, &TAIL_LEG, ProbeSuite::now_us());

        get_orig_fn!(SuccessionEvent_OnCompleteFadeOut, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_OnCompleteFadeOut, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_DestroyCutin(this: *mut Il2CppObject) {
            DESTROY_CUT_IN.count();

        get_orig_fn!(SuccessionEvent_DestroyCutin, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_DestroyCutin, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionCutInHelper_OnEndCutIn(this: *mut Il2CppObject) {
            HELPER_END_CUT_IN.count();

        get_orig_fn!(SuccessionCutInHelper_OnEndCutIn, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionCutInHelper_OnEndCutIn, InheritVoidFn)(this)
    }
}

type ChangeToSuccessionCutFn = extern "C" fn(this: *mut Il2CppObject, entry: *mut Il2CppObject);
def_detour! {
    ChangeViewManager_ChangeViewSuccessionCutAndStory(this: *mut Il2CppObject, entry: *mut Il2CppObject) {
            CHANGE_TO_SUCCESSION_CUT.count();

        get_orig_fn!(ChangeViewManager_ChangeViewSuccessionCutAndStory, ChangeToSuccessionCutFn)(this, entry);
    }
    bail {
                get_orig_fn!(ChangeViewManager_ChangeViewSuccessionCutAndStory, ChangeToSuccessionCutFn)(this, entry)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let succession = class_for_label(umamusume, "Gallop.SingleModeSuccessionEventViewController");
    let helper = class_for_label(umamusume, "Gallop.SingleModeSuccessionCutInHelper");
    let change_view = class_for_label(umamusume, "Gallop.SingleModeChangeViewManager");

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;

    macro_rules! probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        installed += 1;
                    }
                    else {
                        missing.push($label);
                    }
                },
                None => missing.push($label),
            }
        };
    }

    probe!(succession, SuccessionEvent_PrepareCutIn, "PrepareCutIn", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::PrepareCutIn()");
    probe!(succession, SuccessionEvent_PlayCutIn, "PlayCutIn", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::PlayCutIn()");
    probe!(succession, SuccessionEvent_OnPauseCutt, "OnPauseCutt", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::OnPauseCutt()");
    probe!(succession, SuccessionEvent_OnEndCutt, "OnEndCutt", ONE_INFO, VOID, "SingleModeSuccessionEventViewController::OnEndCutt(controller)");
    probe!(succession, SuccessionEvent_OnCompleteFadeOut, "OnCompleteFadeOut", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::OnCompleteFadeOut()");
    probe!(succession, SuccessionEvent_DestroyCutin, "DestroyCutin", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::DestroyCutin()");
    probe!(helper, SuccessionCutInHelper_OnEndCutIn, "OnEndCutIn", NO_PARAMS, VOID, "SingleModeSuccessionCutInHelper::OnEndCutIn()");
    probe!(
        change_view,
        ChangeViewManager_ChangeViewSuccessionCutAndStory,
        "ChangeViewSuccessionCutAndStory",
        ONE_INFO,
        VOID,
        "SingleModeChangeViewManager::ChangeViewSuccessionCutAndStory(entry)"
    );

    ARMED.store(true, atomic::Ordering::Release);
    info!("Inherit probe: {installed} of {} doors installed, the visit is split into prepare, cut and tail legs", PROBES.len());

    if !missing.is_empty() {
        info!("Inherit probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the other probe reports.
pub fn report_if_due() {
    if !ARMED.load(atomic::Ordering::Acquire) {
        return;
    }

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();
    let now = ProbeSuite::now_us();
    let now_sec = now / 1_000_000;
    let last_sec = LAST_REPORT_SEC.load(atomic::Ordering::Relaxed);
    let last_totals = LAST_TOTALS.load(atomic::Ordering::Relaxed);

    if !report_due(now_sec, last_sec, totals, last_totals, REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_TOTALS.store(totals, atomic::Ordering::Relaxed);

    let mut line = String::new();

    for probe in PROBES.iter() {
        let calls = probe.calls();

        if calls > 0 {
            let _ = write!(line, " {}={}", probe.label(), calls);
        }
    }

    let open_ms = |open: &AtomicI64| leg_us(open.load(atomic::Ordering::Relaxed), now).unwrap_or(0) / 1_000;

    info!(
        "Inherit probe totals at {now_sec} s:{line} | {} | {} | {} | open now: prepare {} ms, cut {} ms, tail {} ms",
        PREPARE_LEG.line("prepare"),
        CUT_LEG.line("cut"),
        TAIL_LEG.line("tail"),
        open_ms(&PREPARE_OPEN_US),
        open_ms(&CUT_OPEN_US),
        open_ms(&TAIL_OPEN_US),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leg_has_a_wall_only_when_it_was_opened_before_it_closed() {
        assert_eq!(leg_us(-1, 5_000), None, "a leg that never opened has no wall");
        assert_eq!(leg_us(9_000, 5_000), None, "a close before its open adds nothing");
        assert_eq!(leg_us(5_000, 5_000), Some(0));
        assert_eq!(leg_us(1_000_000, 1_250_000), Some(250_000));
    }

    #[test]
    fn a_leg_keeps_its_runs_their_mean_and_their_worst() {
        let leg = Leg::new();

        leg.record(100_000);
        leg.record(300_000);

        assert_eq!(leg.line("cut"), "cut 2 runs mean 200.0 ms worst 300 ms");
    }

    #[test]
    fn a_leg_with_no_runs_says_so() {
        assert_eq!(Leg::new().line("tail"), "tail 0 runs");
    }

    #[test]
    fn every_inherit_door_is_named_by_the_view_or_helper_that_owns_it() {
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();

        assert_eq!(labels.len(), 8);
        assert!(labels.iter().all(|label| {
            label.starts_with("SingleModeSuccessionEventViewController::")
                || label.starts_with("SingleModeSuccessionCutInHelper::")
                || label.starts_with("SingleModeChangeViewManager::")
        }));
    }
}
