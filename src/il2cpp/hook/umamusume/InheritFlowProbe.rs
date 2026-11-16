use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicBool, AtomicI64, AtomicUsize};

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed,
            ProbeSuite,
            TrainingCuttProbe::{class_for_label, report_due, bit, CutProbe, REPORT_INTERVAL_SECS},
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
// inheritance visit into five legs, the prepare, the cut, the tail, the press and the after, and reports each one's
// runs, mean and worst wall time, plus how long the leg that is open right now has been open. A stall then shows as
// one leg with an open time that keeps growing, which is the number the stall census could not give.
//
// The last two legs were added for the player session of 2026-10-28 (`run log/hachimi (2).log`), where six of eleven
// Inspiration visits crossed `OnEndCutt` and stayed on the screen 2 to 4 s and two crossed neither and ended in a
// soft reset after 7 to 10 s. Everything this module armed stopped at `OnCompleteFadeOut`: the button the game offers
// after the cut, the click that answers it, and the result plates behind the click had no door in any build, so a
// visit that sat waiting for a click looked the same in the log as one that had nothing left to do.
//
// Names come from the live dump, re-read against the client this fork plays on: `Gallop.SingleModeSuccessionEventViewController`
// (`InitializeView/0` returning a coroutine at `run log/introspect-run69.log:24597`, `BeginView/0` at `:24598`,
// `FinalizeView/0` returning a coroutine at `:24602`, `PrepareCutIn/0` at `:24603`, `CreateCharacterModel/0` at
// `:24604`, `PlayCutIn/0` at `:24606`, `OnPauseCutt/0` at `:24607`, `OnEndCutt/1` taking the timeline controller at
// `:24609`, `OnCompleteFadeOut/0` at `:24610`, `CheckResult/0` at `:24611`, `PlayResultCoroutine/0` at `:24612`,
// `DestroyCutin/0` at `:24613`, `DestroyCharacter/0` at `:24614`, `SetupSuccessionButton/0` at `:24615`, the request
// the cut end sends as `<OnEndCutt>g__OnSendSuccess|28_0/1 -> void(int)` at `:24618`, and the three listeners the
// button is wired to as `<SetupSuccessionButton>b__34_0/0`, `b__34_1/1` and `b__34_2/1` at `:24619` to `:24621`),
// `Gallop.SingleModeSuccessionCutInHelper::OnEndCutIn/0`, and `Gallop.SingleModeChangeViewManager::
// ChangeViewSuccessionCutAndStory/1` taking the entry info. All are instance methods.
//
// The last nine of those are for what the session of 2026-10-30 could not say. Two Inspiration visits on view 1501
// both parked their two timelines at 3.267 s of 3.240 with `OnEndCutt` at zero crossings, one of them after a click
// the player made themselves 2.521 s after the button went up. Run 69 parked the same way and is the first run that
// counted the click's own side: `ButtonListener()` and `ButtonFlagListenerA(flag)` crossed after the player clicked
// while `OnEndCutt`, `OnCompleteFadeOut`, `CheckResult` and `PlayResultCoroutine` stayed at zero crossings. The door
// that carries the inheritance request is now resolved on this client's shape, `OnSendSuccess(cutt)`; the shape the
// 2026-10-30 dumps gave it never installed here.
//
// Nothing here changes an argument or a return value, and nothing presses a button or ends a cut. The whole module
// is behind `debug_mode`, so a player who has that off gets no hook at all.

const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const ONE_INFO: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS];
// The shape of the two button listeners that take a flag, `<SetupSuccessionButton>b__34_1/1 -> void(bool)` and
// `b__34_2/1 -> void(bool)` at `run log/introspect-run62.log:24368` and `:24369`.
const ONE_FLAG: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN];
// The inheritance request's own shape on this client: `<OnEndCutt>g__OnSendSuccess|28_0/1 -> void(int)` at
// `run log/introspect-run69.log:24618`, the cut index the cut end hands its own request. The client whose dump is
// `run log/introspect-run62.log` spelled that same method `/0 -> void()` at `:24366`, and a door resolved against the
// older shape reports "no matching overload" on this one, which is the whole reason runs 62 to 69 never counted it.
const ONE_INT: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_I4];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
// The return shape of a coroutine door: `CLASS` matches every reference type, which is what an `IEnumerator`
// return is. `TrainingCuttProbe.rs:65` resolves its coroutine doors the same way.
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;

// Every door this probe holds is one a session crosses once per Succession visit: run 73 played 11 visits and its
// busiest door, `DestroyCharacter`, crossed 15 times. The shared `PROBE_DETAIL_LIMIT` of 8 cut the per-crossing lines
// off at the eighth visit, which is exactly where that session's three stuck visits sat (C85), so these doors print
// their crossings for a whole session's worth of visits before the chunk counts take over.
const DOOR_TRACE_LIMIT: usize = 40;

static PREPARE_CUT_IN: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::PrepareCutIn()", DOOR_TRACE_LIMIT);
static PLAY_CUT_IN: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::PlayCutIn()", DOOR_TRACE_LIMIT);
static PAUSE_CUTT: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::OnPauseCutt()", DOOR_TRACE_LIMIT);
static END_CUTT: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::OnEndCutt(controller)", DOOR_TRACE_LIMIT);
static FADE_OUT_DONE: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::OnCompleteFadeOut()", DOOR_TRACE_LIMIT);
static DESTROY_CUT_IN: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::DestroyCutin()", DOOR_TRACE_LIMIT);
static HELPER_END_CUT_IN: CutProbe = CutProbe::traced("SingleModeSuccessionCutInHelper::OnEndCutIn()", DOOR_TRACE_LIMIT);
static CHANGE_TO_SUCCESSION_CUT: CutProbe =
    CutProbe::traced("SingleModeChangeViewManager::ChangeViewSuccessionCutAndStory(entry)", DOOR_TRACE_LIMIT);

// The succession button doors. Their hooks belong to `SingleModeSuccessionEventViewController`, which arms them in
// every build, and that module calls the two note functions below when it crosses them. They are counted here
// because the legs of one visit are one bookkeeping and this module has the report. Both names are in the view's
// own block in the dump, `run log/introspect-run69.log:24608` and `:24615`.
static SETUP_BUTTON: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::SetupSuccessionButton()", DOOR_TRACE_LIMIT);
static ON_CLICK: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::OnClickSuccessionButton()", DOOR_TRACE_LIMIT);
// The two doors that play the result plates after the click, `CheckResult/0 -> void()` and
// `PlayResultCoroutine/0 -> class<System.Collections.IEnumerator>()` at `run log/introspect-run69.log:24611` and
// `:24612`. Run 67 crossed `CheckResult()` twice on a visit this fork's own click closed; `PlayResultCoroutine()` has
// never crossed in any run, so no log yet says whether the plates play through it at all.
static CHECK_RESULT: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::CheckResult()", DOOR_TRACE_LIMIT);
static PLAY_RESULT: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::PlayResultCoroutine()", DOOR_TRACE_LIMIT);

// The view's own lifecycle, so a visit that never got to its cut and one that cannot leave it are told apart.
// `InitializeView/0` and `FinalizeView/0` are coroutines (`run log/introspect-run69.log:24597`, `:24602`),
// `BeginView/0` is at `:24598`, `CreateCharacterModel/0` at `:24604` and `DestroyCharacter/0` at `:24614`.
static INITIALIZE_VIEW: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::InitializeView()", DOOR_TRACE_LIMIT);
static BEGIN_VIEW: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::BeginView()", DOOR_TRACE_LIMIT);
static FINALIZE_VIEW: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::FinalizeView()", DOOR_TRACE_LIMIT);
static CREATE_CHARACTER_MODEL: CutProbe =
    CutProbe::traced("SingleModeSuccessionEventViewController::CreateCharacterModel()", DOOR_TRACE_LIMIT);
static DESTROY_CHARACTER: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::DestroyCharacter()", DOOR_TRACE_LIMIT);
// The request the cut end sends, `<OnEndCutt>g__OnSendSuccess|28_0/1 -> void(int)` at `run log/introspect-run69.log:24618`,
// counted with the cut index it is handed because this screen runs two cut-ins at once. The label keeps the compiler's
// name out of it so the totals line stays one field per door. Every door this probe counts after the cut waits on
// whatever this one sends, and no door of ours has ever seen it cross: the dump of 2026-10-30 declared the method with
// no argument, this client declares the int, and the door was resolved against the older shape.
static SEND_SUCCESS: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::OnSendSuccess(cutt)", DOOR_TRACE_LIMIT);
// The listeners `SetupSuccessionButton` wires onto the button at `:24367` to `:24369`. A press this fork makes goes
// through the view's own `OnClickSuccessionButton`, and whether the game's own listeners run behind it is the open
// question about the auto click that these three are the only way to answer.
static BUTTON_LISTENER: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::ButtonListener()", DOOR_TRACE_LIMIT);
static BUTTON_FLAG_LISTENER_A: CutProbe =
    CutProbe::traced("SingleModeSuccessionEventViewController::ButtonFlagListenerA(flag)", DOOR_TRACE_LIMIT);
static BUTTON_FLAG_LISTENER_B: CutProbe =
    CutProbe::traced("SingleModeSuccessionEventViewController::ButtonFlagListenerB(flag)", DOOR_TRACE_LIMIT);

pub(crate) static PROBES: [&CutProbe; 21] = [
    &INITIALIZE_VIEW,
    &BEGIN_VIEW,
    &PREPARE_CUT_IN,
    &PLAY_CUT_IN,
    &PAUSE_CUTT,
    &END_CUTT,
    &SEND_SUCCESS,
    &CREATE_CHARACTER_MODEL,
    &FADE_OUT_DONE,
    &SETUP_BUTTON,
    &BUTTON_LISTENER,
    &BUTTON_FLAG_LISTENER_A,
    &BUTTON_FLAG_LISTENER_B,
    &ON_CLICK,
    &CHECK_RESULT,
    &PLAY_RESULT,
    &FINALIZE_VIEW,
    &DESTROY_CHARACTER,
    &DESTROY_CUT_IN,
    &HELPER_END_CUT_IN,
    &CHANGE_TO_SUCCESSION_CUT,
];

// The legs of one inheritance visit. Each opens on one door and closes on the next.
//   prepare: PrepareCutIn -> PlayCutIn
//   cut:     PlayCutIn -> OnEndCutt
//   tail:    OnEndCutt -> OnCompleteFadeOut
//   press:   SetupSuccessionButton -> OnClickSuccessionButton, the time the offered button waits for a click
//   after:   OnClickSuccessionButton -> DestroyCutin, the teardown the click starts
static PREPARE_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static CUT_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static TAIL_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static PRESS_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static AFTER_OPEN_US: AtomicI64 = AtomicI64::new(-1);
static PREPARE_LEG: Leg = Leg::new();
static CUT_LEG: Leg = Leg::new();
static TAIL_LEG: Leg = Leg::new();
static PRESS_LEG: Leg = Leg::new();
static AFTER_LEG: Leg = Leg::new();

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

// The succession button doors are hooked by `SingleModeSuccessionEventViewController`, which arms them in every
// build, and that module calls these two when it crosses them. The offer opens the press leg and the click closes
// it and opens the after leg, which the `DestroyCutin` door below closes. The click count covers a click the player
// made and the click this fork's auto click made alike; the lines the module that presses names which was which.
// Nothing is recorded when this module's own doors were never armed, so a player without `debug_mode` spends one
// load of one atomic on these two calls.
pub(crate) fn note_succession_button_setup() {
    if !ARMED.load(atomic::Ordering::Acquire) {
        return;
    }

    SETUP_BUTTON.count();
    open_leg(&PRESS_OPEN_US, ProbeSuite::now_us());
}

pub(crate) fn note_succession_button_click() {
    if !ARMED.load(atomic::Ordering::Acquire) {
        return;
    }

    ON_CLICK.count();

    let now = ProbeSuite::now_us();
    close_leg(&PRESS_OPEN_US, &PRESS_LEG, now);
    open_leg(&AFTER_OPEN_US, now);
}

/// How many cut runs this probe has closed, or `None` while its doors are not armed. The auto click asks this whether
/// the screen it is about to click has finished its cut, because the session of 2026-10-30 showed the game crossing
/// `SetupSuccessionButton` 34 ms before `PrepareCutIn`: a button that exists is not that answer, and a closed cut leg
/// is. A build without `debug_mode` arms no door here, and that build's auto click watches the cut end itself.
pub(crate) fn cut_runs_closed() -> Option<usize> {
    if !ARMED.load(atomic::Ordering::Acquire) {
        return None;
    }

    Some(CUT_LEG.runs.load(atomic::Ordering::Relaxed))
}

type InheritVoidFn = extern "C" fn(this: *mut Il2CppObject);
type InheritVoidFlagFn = extern "C" fn(this: *mut Il2CppObject, flag: bool);
type InheritVoidIntFn = extern "C" fn(this: *mut Il2CppObject, cutt: i32);
type CoroutineReturnFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
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
        close_leg(&AFTER_OPEN_US, &AFTER_LEG, ProbeSuite::now_us());

        get_orig_fn!(SuccessionEvent_DestroyCutin, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_DestroyCutin, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_CheckResult(this: *mut Il2CppObject) {
            CHECK_RESULT.count();

        get_orig_fn!(SuccessionEvent_CheckResult, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_CheckResult, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_PlayResultCoroutine(this: *mut Il2CppObject) -> *mut Il2CppObject {
            PLAY_RESULT.count();

        get_orig_fn!(SuccessionEvent_PlayResultCoroutine, CoroutineReturnFn)(this)
    }
    bail {
                get_orig_fn!(SuccessionEvent_PlayResultCoroutine, CoroutineReturnFn)(this)
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

// The view's lifecycle. The two coroutines are entered once per visit and the game drives what they return, so these
// record the step the game took and hand back the pointer it was given, unchanged.
def_detour! {
    SuccessionEvent_InitializeView(this: *mut Il2CppObject) -> *mut Il2CppObject {
            INITIALIZE_VIEW.count();

        get_orig_fn!(SuccessionEvent_InitializeView, CoroutineReturnFn)(this)
    }
    bail {
                get_orig_fn!(SuccessionEvent_InitializeView, CoroutineReturnFn)(this)
    }
}

def_detour! {
    SuccessionEvent_BeginView(this: *mut Il2CppObject) {
            BEGIN_VIEW.count();

        get_orig_fn!(SuccessionEvent_BeginView, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_BeginView, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_FinalizeView(this: *mut Il2CppObject) -> *mut Il2CppObject {
            FINALIZE_VIEW.count();

        get_orig_fn!(SuccessionEvent_FinalizeView, CoroutineReturnFn)(this)
    }
    bail {
                get_orig_fn!(SuccessionEvent_FinalizeView, CoroutineReturnFn)(this)
    }
}

// The cut's characters: this is the step that asks for the models the cut animates, so a visit whose cut never
// closed can be told by whether the models were ever asked for.
def_detour! {
    SuccessionEvent_CreateCharacterModel(this: *mut Il2CppObject) {
            CREATE_CHARACTER_MODEL.count();

        get_orig_fn!(SuccessionEvent_CreateCharacterModel, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_CreateCharacterModel, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_DestroyCharacter(this: *mut Il2CppObject) {
            DESTROY_CHARACTER.count();

        get_orig_fn!(SuccessionEvent_DestroyCharacter, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_DestroyCharacter, InheritVoidFn)(this)
    }
}

// The request the cut end sends, `<OnEndCutt>g__OnSendSuccess|28_0`. Counted on entry, before the game's own body
// asks anything of the server, so a visit that reached the cut end without sending is told from one that sent and
// then waited. The int it takes is the cut index the end action was handed, and the first crossings print it: this
// screen runs two cut-ins, and run 69 parked both.
def_detour! {
    SuccessionEvent_OnSendSuccess(this: *mut Il2CppObject, cutt: i32) {
             SEND_SUCCESS.observe(&[cutt as f64]);

        get_orig_fn!(SuccessionEvent_OnSendSuccess, InheritVoidIntFn)(this, cutt);
    }
    bail {
                get_orig_fn!(SuccessionEvent_OnSendSuccess, InheritVoidIntFn)(this, cutt)
    }
}

// The three listeners `SetupSuccessionButton` wires onto the button. A press this fork makes goes through the view's
// own handler, and these are the only doors that can say whether what the game registered behind that handler ran.
def_detour! {
    SuccessionEvent_ButtonListener(this: *mut Il2CppObject) {
            BUTTON_LISTENER.count();

        get_orig_fn!(SuccessionEvent_ButtonListener, InheritVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_ButtonListener, InheritVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_ButtonFlagListenerA(this: *mut Il2CppObject, flag: bool) {
            BUTTON_FLAG_LISTENER_A.observe(&[bit(flag)]);

        get_orig_fn!(SuccessionEvent_ButtonFlagListenerA, InheritVoidFlagFn)(this, flag);
    }
    bail {
                get_orig_fn!(SuccessionEvent_ButtonFlagListenerA, InheritVoidFlagFn)(this, flag)
    }
}

def_detour! {
    SuccessionEvent_ButtonFlagListenerB(this: *mut Il2CppObject, flag: bool) {
            BUTTON_FLAG_LISTENER_B.observe(&[bit(flag)]);

        get_orig_fn!(SuccessionEvent_ButtonFlagListenerB, InheritVoidFlagFn)(this, flag);
    }
    bail {
                get_orig_fn!(SuccessionEvent_ButtonFlagListenerB, InheritVoidFlagFn)(this, flag)
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
    probe!(succession, SuccessionEvent_CheckResult, "CheckResult", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::CheckResult()");
    probe!(succession, SuccessionEvent_PlayResultCoroutine, "PlayResultCoroutine", NO_PARAMS, CLASS, "SingleModeSuccessionEventViewController::PlayResultCoroutine()");
    // The lifecycle doors. The two coroutine doors resolve on the coroutine return shape, the same `CLASS` match the
    // result coroutine above uses.
    probe!(succession, SuccessionEvent_InitializeView, "InitializeView", NO_PARAMS, CLASS, "SingleModeSuccessionEventViewController::InitializeView()");
    probe!(succession, SuccessionEvent_BeginView, "BeginView", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::BeginView()");
    probe!(succession, SuccessionEvent_FinalizeView, "FinalizeView", NO_PARAMS, CLASS, "SingleModeSuccessionEventViewController::FinalizeView()");
    probe!(succession, SuccessionEvent_CreateCharacterModel, "CreateCharacterModel", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::CreateCharacterModel()");
    probe!(succession, SuccessionEvent_DestroyCharacter, "DestroyCharacter", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::DestroyCharacter()");
    // The request inside the cut end, under the compiler's own name for it, resolved on the shape this client gives it:
    // `/1 -> void(int)` at `run log/introspect-run69.log:24618`. Asked as `NO_PARAMS` it answered "no class or no
    // matching overload" in every run from 62 to 69, because the client those dumps came from declared no argument.
    probe!(succession, SuccessionEvent_OnSendSuccess, "<OnEndCutt>g__OnSendSuccess|28_0", ONE_INT, VOID, "SingleModeSuccessionEventViewController::OnSendSuccess(cutt)");
    // The three listeners the button was wired to, under their compiler names. All three cross on a visit that reaches
    // the button: run 69 counted `ButtonListener()` and `ButtonFlagListenerA(flag)` after the player's own click. A
    // method the compiler hoisted off the display class and made static is rejected by the instance matcher (`A3`),
    // lands in `missing` by name, and is said there rather than called through a wrapper that would misread its
    // arguments.
    probe!(succession, SuccessionEvent_ButtonListener, "<SetupSuccessionButton>b__34_0", NO_PARAMS, VOID, "SingleModeSuccessionEventViewController::ButtonListener()");
    probe!(succession, SuccessionEvent_ButtonFlagListenerA, "<SetupSuccessionButton>b__34_1", ONE_FLAG, VOID, "SingleModeSuccessionEventViewController::ButtonFlagListenerA(flag)");
    probe!(succession, SuccessionEvent_ButtonFlagListenerB, "<SetupSuccessionButton>b__34_2", ONE_FLAG, VOID, "SingleModeSuccessionEventViewController::ButtonFlagListenerB(flag)");
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
    info!(
        "Inherit probe: {installed} of 19 doors installed, the two succession button doors counted by the auto click module, the visit split into prepare, cut, tail, press and after legs, the cut's own request, its characters and the button's listeners counted apart"
    );

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
        "Inherit probe totals at {now_sec} s:{line} | {} | {} | {} | {} | {} | open now: prepare {} ms, cut {} ms, tail {} ms, press {} ms, after {} ms",
        PREPARE_LEG.line("prepare"),
        CUT_LEG.line("cut"),
        TAIL_LEG.line("tail"),
        PRESS_LEG.line("press"),
        AFTER_LEG.line("after"),
        open_ms(&PREPARE_OPEN_US),
        open_ms(&CUT_OPEN_US),
        open_ms(&TAIL_OPEN_US),
        open_ms(&PRESS_OPEN_US),
        open_ms(&AFTER_OPEN_US),
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

        assert_eq!(labels.len(), 21);
        assert!(labels.iter().all(|label| {
            label.starts_with("SingleModeSuccessionEventViewController::")
                || label.starts_with("SingleModeSuccessionCutInHelper::")
                || label.starts_with("SingleModeChangeViewManager::")
        }));

        // The four doors the 2026-10-28 player log could not see. A run that reaches the button has to show them.
        let wanted = [
            "SingleModeSuccessionEventViewController::SetupSuccessionButton()",
            "SingleModeSuccessionEventViewController::OnClickSuccessionButton()",
            "SingleModeSuccessionEventViewController::CheckResult()",
            "SingleModeSuccessionEventViewController::PlayResultCoroutine()",
        ];

        for label in wanted {
            assert!(labels.contains(&label), "{label} is a door this probe counts");
        }

        // The nine the session of 2026-10-30 could not see either: a visit whose cut never closed had no door for
        // the request the cut end sends, no door for the models that cut animates, no door at the view's own entry
        // or exit, and no door on the three listeners the button was wired to.
        let owed = [
            "SingleModeSuccessionEventViewController::OnSendSuccess(cutt)",
            "SingleModeSuccessionEventViewController::InitializeView()",
            "SingleModeSuccessionEventViewController::BeginView()",
            "SingleModeSuccessionEventViewController::FinalizeView()",
            "SingleModeSuccessionEventViewController::CreateCharacterModel()",
            "SingleModeSuccessionEventViewController::DestroyCharacter()",
            "SingleModeSuccessionEventViewController::ButtonListener()",
            "SingleModeSuccessionEventViewController::ButtonFlagListenerA(flag)",
            "SingleModeSuccessionEventViewController::ButtonFlagListenerB(flag)",
        ];

        for label in owed {
            assert!(labels.contains(&label), "{label} is a door this probe counts");
        }
    }

    // The auto click needs an answer to "is this screen's cut over" that a button cannot give, and it needs to know
    // that a build without `debug_mode` has no doors here at all. Those are the two facts this holds open; the run
    // line that checks the numbers is owed to item 111 with the rest of the press.
    #[test]
    fn an_unarmed_probe_gives_the_press_no_answer_about_the_cut() {
        assert_eq!(cut_runs_closed(), None, "with no doors armed this probe cannot say whether a cut closed");
    }

    #[test]
    fn the_press_and_after_legs_hold_a_visit_from_the_offer_to_the_teardown() {
        let now = 1_000_000;

        open_leg(&PRESS_OPEN_US, now);
        close_leg(&PRESS_OPEN_US, &PRESS_LEG, now + 300_000);
        open_leg(&AFTER_OPEN_US, now + 300_000);
        close_leg(&AFTER_OPEN_US, &AFTER_LEG, now + 800_000);

        assert_eq!(PRESS_LEG.line("press"), "press 1 runs mean 300.0 ms worst 300 ms");
        assert_eq!(AFTER_LEG.line("after"), "after 1 runs mean 500.0 ms worst 500 ms");
        assert_eq!(leg_us(PRESS_OPEN_US.load(atomic::Ordering::Relaxed), now + 900_000), None, "a closed leg reports no open time");
    }
}
