use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed,
            TrainingCuttProbe::{class_for_label, peak_seconds, report_due, CutProbe, REPORT_INTERVAL_SECS},
        },
        types::*,
    },
};

// Observe only hooks over the popups and the skill learning screen that sit between career screens.
//
// This fork has measured a training cut to the millisecond and has never counted a dialog. Item 84 put the
// remaining between-cut time in the player's own decision time, and item 99 says the one thing on that path
// nobody has measured is the acknowledgement wall: how many times a career run stops on a one-button popup.
// The licensed read of Overseer's release binary says the same flow from the other side, and says it is
// speedable through the game's own buttons, matched by button shape because the caption is translated. This
// file counts the doors so the next decision is priced rather than guessed.
//
// What is deliberately not here: any acknowledgement. Nothing in this module presses a button, closes a
// dialog, or answers a tap. Every wrapper hands the call to the game untouched, and the whole module is
// behind `debug_mode`, so a player who has that off gets no hook at all.
//
// The dump on this client is truncated at 500 classes (A8), so absence of a class here proves nothing about
// the client. Two of these doors are known from this fork's own working code instead: `DialogManager.rs`
// resolves `PushDialog/1` and `PushSystemDialog/2` on `Gallop.DialogManager` today, and both of its wrappers
// return the dialog object rather than nothing, which is the shape the wrappers below declare (A3: a static
// target gets no `this`). A door that does not resolve is reported by name and stays inert (A4).

const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const ONE_INFO: &[Il2CppTypeEnum] = &[CLASS];
const INFO_AND_FLAG: &[Il2CppTypeEnum] = &[CLASS, BOOL];

// The popup doors. A career run that raises no dialog at all reads zero here, and that is worth knowing
// before anything is built to acknowledge one.
static PUSH_DIALOG: CutProbe = CutProbe::counted("DialogManager::PushDialog(data)");
static PUSH_SYSTEM_DIALOG: CutProbe = CutProbe::counted("DialogManager::PushSystemDialog(data, outsideClick)");
// Their event skip is suppressed while a dialog is open, so this is the state a skip would have to respect.
// Peaked rather than counted-only: a peak of 1.0 says a dialog was open at some point in the run.
static IS_SHOW_DIALOG: CutProbe = CutProbe::peaked("DialogManager::get_IsShowDialog()");

// The skill learning screen Overseer speeds and this fork has never touched. The delay door is a duration a
// getter reports, which is the shape this fork's write layer exists for, and this is the first number for it.
static ON_CLICK_SKILL: CutProbe = CutProbe::counted("SingleModeMainViewController::OnClickSkillLearning()");
static SKILL_BACK_DELAY: CutProbe = CutProbe::peaked("SingleModeSkillLearningViewController::GetBackButtonAnimationDelayTime()");

// The career main screen's own view lifecycle, so a popup count can be read against how long the player was
// actually on the screen it popped into. Dump: `BeginView/0 -> void()` at 24031 and `PlayOutView/0 ->
// class<System.Collections.IEnumerator>()` at 24032, both under `Gallop.SingleModeMainViewController`.
static BEGIN_VIEW: CutProbe = CutProbe::counted("SingleModeMainViewController::BeginView()");
static PLAY_OUT_VIEW: CutProbe = CutProbe::counted("SingleModeMainViewController::PlayOutView()");

static PROBES: [&CutProbe; 7] = [
    &PUSH_DIALOG,
    &PUSH_SYSTEM_DIALOG,
    &IS_SHOW_DIALOG,
    &ON_CLICK_SKILL,
    &SKILL_BACK_DELAY,
    &BEGIN_VIEW,
    &PLAY_OUT_VIEW,
];

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

// One career-screen visit: opened by `BeginView`, closed by `PlayOutView`.
static VIEW_OPENED_MS: AtomicI64 = AtomicI64::new(-1);
static VISITS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static VISIT_WALL_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static VISIT_WALL_MS_WORST: AtomicI64 = AtomicI64::new(0);

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

// A visit has a wall only if it was opened and not closed twice. An unopened close contributes nothing,
// which is the rule every other run clock in this fork already follows.
fn visit_milliseconds(opened: i64, now: i64) -> Option<i64> {
    if opened < 0 || now < opened {
        return None;
    }

    Some(now - opened)
}

fn open_visit(now: i64) {
    VIEW_OPENED_MS.store(now, atomic::Ordering::Relaxed);
}

fn close_visit(now: i64) {
    let opened = VIEW_OPENED_MS.swap(-1, atomic::Ordering::Relaxed);
    let Some(ms) = visit_milliseconds(opened, now) else {
        return;
    };

    VISITS_CLOSED.fetch_add(1, atomic::Ordering::Relaxed);
    VISIT_WALL_MS_TOTAL.fetch_add(ms, atomic::Ordering::Relaxed);
    VISIT_WALL_MS_WORST.fetch_max(ms, atomic::Ordering::Relaxed);
}

type PushDialogFn = extern "C" fn(data: *mut Il2CppObject) -> *mut Il2CppObject;
// This fork calls this door itself, in `DialogManager::single_button_message`, so the address is known good
// on this client. Static, one class argument, and it hands back the dialog (A3).
def_detour! {
    DialogManager_PushDialog(data: *mut Il2CppObject) -> *mut Il2CppObject {
            PUSH_DIALOG.count();

        get_orig_fn!(DialogManager_PushDialog, PushDialogFn)(data)
    }
    bail {
                get_orig_fn!(DialogManager_PushDialog, PushDialogFn)(data)
    }
}

type PushSystemDialogFn = extern "C" fn(data: *mut Il2CppObject, is_enable_outside_click: bool) -> *mut Il2CppObject;
def_detour! {
    DialogManager_PushSystemDialog(data: *mut Il2CppObject, is_enable_outside_click: bool) -> *mut Il2CppObject {
            PUSH_SYSTEM_DIALOG.count();

        get_orig_fn!(DialogManager_PushSystemDialog, PushSystemDialogFn)(data, is_enable_outside_click)
    }
    bail {
                get_orig_fn!(DialogManager_PushSystemDialog, PushSystemDialogFn)(data, is_enable_outside_click)
    }
}

type IsShowDialogFn = extern "C" fn() -> bool;
// A state read a UI layer polls, so this is the hot shape: one increment, one max, no formatting (section 6).
def_detour! {
    DialogManager_GetIsShowDialog() -> bool {
            let value = get_orig_fn!(DialogManager_GetIsShowDialog, IsShowDialogFn)();
        IS_SHOW_DIALOG.sample(if value { 1.0 } else { 0.0 });

        value
    }
}

type SkillBackDelayFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
// Dump 24295: `GetBackButtonAnimationDelayTime/0 -> float()`, an instance getter on the skill learning view
// controller. Counted and peaked only: this fork has not decided to write it, and a getter with a matching
// setter is off limits to a scale lever until a setter is built beside it.
def_detour! {
    SingleModeSkillLearningViewController_GetBackButtonAnimationDelayTime(this: *mut Il2CppObject) -> f32 {
            let value = get_orig_fn!(SingleModeSkillLearningViewController_GetBackButtonAnimationDelayTime, SkillBackDelayFn)(this);
        SKILL_BACK_DELAY.sample(value);

        value
    }
}

type MainViewVoidFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    SingleModeMainViewController_OnClickSkillLearning(this: *mut Il2CppObject) {
            ON_CLICK_SKILL.count();

        get_orig_fn!(SingleModeMainViewController_OnClickSkillLearning, MainViewVoidFn)(this);
    }
    bail {
                get_orig_fn!(SingleModeMainViewController_OnClickSkillLearning, MainViewVoidFn)(this)
    }
}

def_detour! {
    SingleModeMainViewController_BeginView(this: *mut Il2CppObject) {
            BEGIN_VIEW.count();
        open_visit(elapsed_ms());

        get_orig_fn!(SingleModeMainViewController_BeginView, MainViewVoidFn)(this);
    }
    bail {
                get_orig_fn!(SingleModeMainViewController_BeginView, MainViewVoidFn)(this)
    }
}

type PlayOutViewFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
// `PlayOutView/0 -> class<System.Collections.IEnumerator>()`: the coroutine factory the screen plays out
// with. The iterator is handed back untouched; only the wall is measured (A4).
def_detour! {
    SingleModeMainViewController_PlayOutView(this: *mut Il2CppObject) -> *mut Il2CppObject {
            PLAY_OUT_VIEW.count();
        close_visit(elapsed_ms());

        get_orig_fn!(SingleModeMainViewController_PlayOutView, PlayOutViewFn)(this)
    }
    bail {
                get_orig_fn!(SingleModeMainViewController_PlayOutView, PlayOutViewFn)(this)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());

    let dialog_manager = class_for_label(umamusume, "Gallop.DialogManager");
    let skill_view = class_for_label(umamusume, "Gallop.SingleModeSkillLearningViewController");
    let main_view = class_for_label(umamusume, "Gallop.SingleModeMainViewController");

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

    macro_rules! static_probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_static_method(class, $method, $params, $ret) };

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

    static_probe!(dialog_manager, DialogManager_PushDialog, "PushDialog", ONE_INFO, CLASS, "DialogManager::PushDialog(data)");
    static_probe!(dialog_manager, DialogManager_PushSystemDialog, "PushSystemDialog", INFO_AND_FLAG, CLASS, "DialogManager::PushSystemDialog(data, outsideClick)");
    probe!(skill_view, SingleModeSkillLearningViewController_GetBackButtonAnimationDelayTime, "GetBackButtonAnimationDelayTime", NO_PARAMS, R4, "SingleModeSkillLearningViewController::GetBackButtonAnimationDelayTime()");
    probe!(main_view, SingleModeMainViewController_OnClickSkillLearning, "OnClickSkillLearning", NO_PARAMS, VOID, "SingleModeMainViewController::OnClickSkillLearning()");
    probe!(main_view, SingleModeMainViewController_BeginView, "BeginView", NO_PARAMS, VOID, "SingleModeMainViewController::BeginView()");
    probe!(main_view, SingleModeMainViewController_PlayOutView, "PlayOutView", NO_PARAMS, CLASS, "SingleModeMainViewController::PlayOutView()");

    // `get_IsShowDialog` is not in the truncated dump (A8), so its static half is not stated anywhere here.
    // It is asked for the way the two doors beside it on the same class are known: `DialogManager.rs`
    // resolves `PushDialog/1` and `PushSystemDialog/2` as statics today, so a static read is the shape the
    // class answers on this client. If it is not a static the resolver refuses it and the door is named as
    // missing rather than armed with a wrapper that would misread its arguments (A3).
    static_probe!(dialog_manager, DialogManager_GetIsShowDialog, "get_IsShowDialog", NO_PARAMS, BOOL, "DialogManager::get_IsShowDialog()");

    info!("Dialog probe: {installed} doors installed, {} of them carry a counted kind in the totals line, one career-screen visit clock opened by BeginView and closed by PlayOutView", PROBES.len());

    if !missing.is_empty() {
        info!("Dialog probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the other probe reports.
pub fn report_if_due() {
    if START.get().is_none() {
        return;
    }

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();
    let now_sec = elapsed_ms() / 1000;
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

        if calls == 0 {
            continue;
        }

        if probe.is_peaked() {
            let _ = write!(line, " {}={} peak {:.3}", probe.label(), calls, peak_seconds(probe.peak_bits()));
        }
        else {
            let _ = write!(line, " {}={}", probe.label(), calls);
        }
    }

    let visits = VISITS_CLOSED.load(atomic::Ordering::Relaxed);
    let total_ms = VISIT_WALL_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let worst_ms = VISIT_WALL_MS_WORST.load(atomic::Ordering::Relaxed);
    let open_for = visit_milliseconds(VIEW_OPENED_MS.load(atomic::Ordering::Relaxed), elapsed_ms()).unwrap_or(0);
    let mean_ms = if visits == 0 { 0 } else { total_ms / (visits as i64) };

    info!(
        "Dialog probe totals at {now_sec} s:{line} | career screen visits {visits} mean {mean_ms} ms worst {worst_ms} ms, open now {open_for} ms, dialogs pushed {}",
        PUSH_DIALOG.calls() + PUSH_SYSTEM_DIALOG.calls()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visit_has_a_wall_only_when_it_was_open() {
        // An unopened close and a close that precedes its open are both counted doors reaching the clock out
        // of order, and neither may add a wall.
        assert_eq!(visit_milliseconds(-1, 500), None);
        assert_eq!(visit_milliseconds(600, 500), None);
        assert_eq!(visit_milliseconds(500, 500), Some(0));
        assert_eq!(visit_milliseconds(500, 12_402), Some(11_902));
    }

    #[test]
    fn no_census_door_is_measured_under_two_names() {
        let mut labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();
        let total = labels.len();

        labels.sort_unstable();
        labels.dedup();

        assert_eq!(labels.len(), total);
        assert_eq!(total, 7);
    }

    #[test]
    fn the_dialog_doors_are_the_choke_point_and_not_a_button() {
        // Nothing in this module presses anything. The doors counted are the ones that raise a dialog and the
        // state read a skip would have to respect, never the game's own button handlers.
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();

        assert!(labels.contains(&"DialogManager::PushDialog(data)"));
        assert!(labels.contains(&"DialogManager::get_IsShowDialog()"));
        assert!(!labels.iter().any(|label| label.contains("OnClickCenterButton") || label.contains("OnClickRightButton")));
    }
}
