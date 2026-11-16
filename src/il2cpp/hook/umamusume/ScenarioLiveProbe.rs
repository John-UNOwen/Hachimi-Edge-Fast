use std::ffi::CStr;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicUsize};

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed, ProbeSuite, SceneManager, StoryFrameProbe, StoryTimelineController,
            TrainingCuttProbe::{class_for_label, flag_bit, peak_seconds, report_due, CutProbe, REPORT_INTERVAL_SECS},
        },
        types::*,
    },
};

// Observe only hooks over the doors the career Grand Live plays through, and one line per visit that holds.
//
// The player called this the grand concert, and the game calls it the Grand Live: view 1621
// `SingleModeScenarioLiveTop` (`SceneDefine.rs:43`), the screen a career run steps onto from the training
// screen and back. A 2026-10-30 session on another PC spent 44 s on it and ended that stay with the
// player's own soft reset (`run log/player20261030-otherpc.log:6655` names the view and `:6713` the reset),
// and an earlier stay in the same session stood 59.5 s. Through both of those stays this fork's own cut-in
// census printed `updatespeed +0` and `SkipRuntime(time) +0` across a whole 20 s report: not one of this
// fork's levers was reached, because the lever that fires on a cut-in tick gets no tick there. When the
// screen did come back, the game crossed its own skip door eight times in 1.3 s
// (`SingleModeTrainingCutInHelper::SkipRuntime()`, a door this fork only counts), which is the "requires
// clicking to progress" the player described.
//
// The build with these probes ran on that machine for 61 min from 2026-10-30T07:52Z (`run log/
// player20261030c-otherpc.log`, a copy): 11 stays on view 1621 totalling 468 s, the longest 85 s, two of
// them past the 60 s the stall watchdog waits for, and the last one still standing when the log was copied
// 35 s in. No soft reset appears anywhere in it. The concert's own `Skip()` was crossed zero times in the
// whole session and `get_CanSkipLive` 19 times, about one per live and mostly within two seconds of the
// screen opening, so the game decides once whether a live may be skipped and then stops asking while the
// screen holds. Every story door of ours goes quiet on this screen: `IsStoryEndFrameOrGrandLiveWaitFrameSkipped`
// stood still through all 11 stays and only ran between them, which retires the reading that a frozen skip
// check here is the stall. On this screen it is the state of the screen.
//
// So this file counts the doors the concert owns and nobody has measured, and prints what they say while a
// career live screen holds. Nothing here presses anything: no skip is asked for, no setting is written, and
// every wrapper hands the call to the game untouched. The module is behind `debug_mode`, so a player who
// has that off gets no hook at all, exactly like the other probe modules.
//
// The names come from this client's own widened dump: `Gallop.SingleModeScenarioLiveCutControl::Skip/0 ->
// void()` and `Gallop.SingleModeScenarioLiveTopModel::get_CanSkipLive/0 -> bool()` at
// `run log/introspect-run62.log:23124` and `:23127`, and the five `Gallop.StoryViewController` doors at
// `:25437`, `:25589`, `:25590`, `:25591` and `:25592`, inside the class block that starts at `:25427`. That
// class is the one that took the first run's five attempts at it: the three high speed doors were written
// against `Gallop.StoryView`, whose own block ends at `:25403`, and a widened line names its owner only as
// far as the dump knows it, so `SortingOrder::field Fade` at `:25404` is
// `Gallop.StoryViewController.SortingOrder`. `PhaseGrandLiveAnimation` is printed with no owner at all
// (`:23125`), and the run found no class under either spelling this file tried first, so that door is
// looked for by name over the image (`phase_animation_class`) and the log names the class it landed on.
// None of these classes is hooked anywhere else in this tree for any of these methods, and the dump prints
// every one of them as an instance method with no `static`, so every wrapper below takes a `this` (A3). The
// dump is truncated at 500 classes (A8), so a door that does not resolve is named as missing and stays
// inert rather than armed on a guess (A4).

const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];

// The two career screens the Grand Live is played on: 1620 is the live selection top and 1621 the live
// itself (`SceneDefine.rs:42` and `:43`). Each gets its own visit clock: a run that steps from the
// selection list onto the live is two screens with two waits, and a line that mixed their counts would
// say a wait started on a screen it never stood on.
const VISIT_VIEWS: [i32; 2] = [1620, 1621];
// A visit gets its first line this long after the screen opened, then one every `VISIT_EVERY_US`, and no
// more than `VISIT_LINE_LIMIT` of them. A player who gives up and soft resets at 45 s still gets four
// lines about what the doors were doing while the screen held, which is the number this fork has never
// had for this screen. The shape is the one the Inspiration watch line uses, so two stalls read alike.
const VISIT_FIRST_US: i64 = 5_000_000;
const VISIT_EVERY_US: i64 = 10_000_000;
const VISIT_LINE_LIMIT: usize = 6;
// A view id no `ViewId` has, so "no live screen is open" is a value the game cannot answer by itself.
const VIEW_NONE: i32 = i32::MIN;

const DOOR_COUNT: usize = 8;
// The order the doors are declared in, spelled out because the per visit marks below are indexed by it
// and a line that mixed the two up would print a skip count beside the wrong words.
const DOOR_SKIP: usize = 0;
const DOOR_CAN_SKIP: usize = 1;
const DOOR_PHASE: usize = 2;
const DOOR_SUPPRESS: usize = 3;
const DOOR_PAUSE: usize = 4;
const DOOR_RESUME: usize = 5;
const DOOR_PAUSE_ALL: usize = 6;
const DOOR_RESUME_ALL: usize = 7;
// The two state getters, indexed the way the per visit answer counters below are.
const FLAG_COUNT: usize = 2;
const FLAG_CAN_SKIP: usize = 0;
const FLAG_SUPPRESS: usize = 1;
const STORY_DOOR_COUNT: usize = 3;

// The concert's own skip control, the game's permission read, and the phase coroutine.
static CUT_CONTROL_SKIP: CutProbe = CutProbe::counted("SingleModeScenarioLiveCutControl::Skip()");
static CAN_SKIP_LIVE: CutProbe = CutProbe::peaked("SingleModeScenarioLiveTopModel::get_CanSkipLive()");
static PHASE_TRANSITION: CutProbe = CutProbe::counted("PhaseGrandLiveAnimation::TransitionGrandlivePhase()");
// The story controller's own Grand Live levers: the flag that says its high speed should not be applied to
// the live, and the pair that pauses and resumes its auto high speed around it. This fork writes the story
// high speed type all run (32 writes in the 61 min session the other PC ran with these probes armed) and
// has never read what the game thinks about applying it to a concert.
static SUPPRESS_LIVE_HIGH_SPEED: CutProbe = CutProbe::peaked("StoryViewController::get_SkipApplyGrandLiveHighSpeedType()");
static PAUSE_LIVE_AUTO_HIGH_SPEED: CutProbe = CutProbe::counted("StoryViewController::PauseGrandLiveAutoHighSpeedSettings()");
static RESUME_LIVE_AUTO_HIGH_SPEED: CutProbe = CutProbe::counted("StoryViewController::ResumeGrandLiveAutoHighSpeedSettings()");
// The same class's pair for all of its auto high speed rather than the live's. A concert that will not move
// because the game parked its auto high speed for everything and never brought it back reads differently
// from one where only the live's own pair stands still, and the first run could not tell them apart.
static PAUSE_ALL_AUTO_HIGH_SPEED: CutProbe = CutProbe::counted("StoryViewController::PauseAutoHighSpeedSettings()");
static RESUME_ALL_AUTO_HIGH_SPEED: CutProbe = CutProbe::counted("StoryViewController::ResumeAutoHighSpeedSettings()");

pub(crate) static PROBES: [&CutProbe; DOOR_COUNT] = [
    &CUT_CONTROL_SKIP,
    &CAN_SKIP_LIVE,
    &PHASE_TRANSITION,
    &SUPPRESS_LIVE_HIGH_SPEED,
    &PAUSE_LIVE_AUTO_HIGH_SPEED,
    &RESUME_LIVE_AUTO_HIGH_SPEED,
    &PAUSE_ALL_AUTO_HIGH_SPEED,
    &RESUME_ALL_AUTO_HIGH_SPEED,
];

// The words the visit line uses for them. A method signature does not fit in a sentence a person reads
// while the game is holding; the totals line prints the census names.
const LINE_SKIP: &str = "live skip door";
const LINE_CAN_SKIP: &str = "can skip live";
const LINE_PHASE: &str = "grand live phase transition";
const LINE_SUPPRESS: &str = "grand live high speed suppress flag";
const LINE_PAUSE: &str = "live auto high speed paused";
const LINE_RESUME: &str = "live auto high speed resumed";
const LINE_PAUSE_ALL: &str = "all auto high speed paused";
const LINE_RESUME_ALL: &str = "all auto high speed resumed";
const LINE_WAIT: &str = "story wait frames";
const LINE_WAITING: &str = "story waiting frames";
const LINE_SKIP_CHECK: &str = "story or grand live wait skip check";

static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);
// The visit the game is standing on, when it was first asked for, and how many lines of it have printed.
// Read from the `ChangeView` atomic the way the frame clock reads it, so a line costs no game call.
static VISIT_VIEW: AtomicI32 = AtomicI32::new(VIEW_NONE);
static VISIT_OPENED_US: AtomicI64 = AtomicI64::new(-1);
static VISITS_SEEN: AtomicUsize = AtomicUsize::new(0);
static VISIT_LINES: AtomicUsize = AtomicUsize::new(0);
// What each door had been crossed by when this visit's last line printed, so a line can say what moved
// since the line before it. The counts only ever go up; the deltas are the part that answers whether
// anything is still running on a screen that will not move.
static LINE_MARKS: [AtomicUsize; DOOR_COUNT] = [const { AtomicUsize::new(0) }; DOOR_COUNT];
static STORY_MARKS: [AtomicUsize; STORY_DOOR_COUNT] = [const { AtomicUsize::new(0) }; STORY_DOOR_COUNT];
// What each state getter answered while the visit in progress was open, a false answer at index 0 and a
// true one at index 1, reset when a new screen opens. The census peak is the highest value the door ever
// handed in the run, which cannot tell a wait where the game said the live may be skipped from a wait where
// it said so two seconds before the screen opened and then stopped asking, and the run of 2026-10-30 07:52Z
// is exactly that question.
static FLAG_ANSWERS: [[AtomicUsize; 2]; FLAG_COUNT] = [const { [AtomicUsize::new(0), AtomicUsize::new(0)] }; FLAG_COUNT];

fn record_answer(flag: usize, value: bool) {
    FLAG_ANSWERS[flag][usize::from(value)].fetch_add(1, atomic::Ordering::Relaxed);
}

fn reset_answers() {
    for row in FLAG_ANSWERS.iter() {
        for answer in row.iter() {
            answer.store(0, atomic::Ordering::Relaxed);
        }
    }
}

fn elapsed_us() -> i64 {
    ProbeSuite::now_us()
}

// Is this a career live screen worth a visit clock.
fn live_view(view: i32) -> bool {
    VISIT_VIEWS.contains(&view)
}

// The class the dump spells `PhaseGrandLiveAnimation` with no owner (`run log/introspect-run62.log:23125`).
// The two spellings this file first tried both came back empty on the client of the 2026-10-30 run, and a
// nested type is its own type definition in the image and its own entry in the class array, so the plain
// lookups are tried first and the walk is the fallback rather than the rule: one pass over the image's
// classes at init, under `debug_mode` only, next to the 0.15 s the hook arming already takes. It returns
// the name it found along with the class, because a door armed on a class nobody can name is a door nobody
// can read again later, and because the next lever needs that name the way this one did.
fn phase_animation_class(image: *const Il2CppImage) -> (Option<*mut Il2CppClass>, Option<String>) {
    const NAME: &CStr = c"PhaseGrandLiveAnimation";

    for label in ["PhaseGrandLiveAnimation", "Gallop.PhaseGrandLiveAnimation"] {
        if let Some(class) = class_for_label(image, label) {
            return (Some(class), None);
        }
    }

    let count = crate::il2cpp::api::il2cpp_image_get_class_count(image);

    for index in 0..count {
        let class: *mut Il2CppClass = crate::il2cpp::api::il2cpp_image_get_class(image, index).cast_mut();

        if class.is_null() {
            continue;
        }

        let name = unsafe { CStr::from_ptr((*class).name) };

        if name != NAME {
            continue;
        }

        let name = name.to_string_lossy().into_owned();
        let namespace = unsafe { CStr::from_ptr((*class).namespaze) }.to_string_lossy().into_owned();
        let found = if namespace.is_empty() { name } else { format!("{namespace}.{name}") };

        return (Some(class), Some(found));
    }

    (None, None)
}

// A line is due once the visit has been open long enough for the next one and the visit has not used up
// its lines. A visit that never reaches the first mark prints nothing at all, which is the right answer
// for a live the game plays in three seconds.
fn visit_line_due(opened_us: i64, now_us: i64, shown: usize) -> bool {
    if opened_us < 0 || shown >= VISIT_LINE_LIMIT {
        return false;
    }

    let due_us = if shown == 0 {
        VISIT_FIRST_US
    }
    else {
        VISIT_FIRST_US + (shown as i64) * VISIT_EVERY_US
    };

    now_us - opened_us >= due_us
}

// Crossings since the last line. A census that restarted under a re-init can hand a count below the
// mark it is read against, and a wrapped subtraction would print a huge number where the truth is none.
fn since_mark(calls: usize, mark: usize) -> usize {
    calls.saturating_sub(mark)
}

// How long ago a door was last crossed, or -1 for a door that never was or a suite whose clock never
// started. Both read as "no stamp" on the line, which is a fact about the run rather than a duration.
fn read_age_ms(stamp_us: i64, now_us: i64) -> i64 {
    if stamp_us < 0 || now_us < stamp_us {
        return -1;
    }

    (now_us - stamp_us) / 1_000
}

// Everything one visit line prints, already read off the doors, so the line a run reads is decided by a
// test and not by a format string buried in a report path.
struct VisitParts<'a> {
    view: i32,
    line_no: usize,
    stood_us: i64,
    // Census name, times crossed, crossings since the last line, for the six counted doors.
    counted: [(&'a str, usize, usize); 6],
    // Census name, times it answered false and true during this visit, times read, reads since the last
    // line, ms since the last read, for the two state getters. A read count that climbs while the frame
    // skip check stands still is the game still asking whether it may skip, and the answers beside it say
    // which way it said: the first run read `get_CanSkipLive` 19 times in 61 min and never told a run which
    // of those 19 said yes, because the census peak is a run wide.
    flags: [(&'a str, usize, usize, usize, usize, i64); 2],
    // Census name, times polled, polls since the last line, and the last answer it gave. The frame skip
    // check answers true and false per call rather than one number worth printing, so it carries None.
    story: [(&'a str, usize, usize, Option<i64>); 3],
    story_mode: bool,
    story_writes: usize,
    // True on the first line of a visit, where every delta is measured from the tick the screen opened.
    from_open: bool,
}

fn visit_line(parts: &VisitParts<'_>) -> String {
    let mut line = format!(
        "Concert visit {}: view {} stood {} s,",
        parts.line_no,
        parts.view,
        parts.stood_us / 1_000_000
    );

    for &(label, calls, delta) in parts.counted.iter() {
        let _ = write!(line, " {label} {calls} (+{delta})");
    }

    for &(label, answered_no, answered_yes, reads, delta, age) in parts.flags.iter() {
        if reads == 0 {
            let _ = write!(line, " {label} never read by the game");
            continue;
        }

        if age < 0 {
            let _ = write!(line, " {label} answered {answered_no} no {answered_yes} yes in this visit, read {reads} times (+{delta}) with no stamp yet");
        }
        else {
            let _ = write!(line, " {label} answered {answered_no} no {answered_yes} yes in this visit, read {reads} times (+{delta}), last read {age} ms ago");
        }
    }

    for &(label, polls, delta, answer) in parts.story.iter() {
        match answer {
            Some(frames) => {
                let _ = write!(line, " | {label} {frames} frames, {polls} polls (+{delta})");
            },
            None => {
                let _ = write!(line, " | {label} {polls} polls (+{delta})");
            },
        }
    }

    let _ = write!(
        line,
        " | story high speed mode {} writes {} | deltas {}",
        u8::from(parts.story_mode),
        parts.story_writes,
        if parts.from_open { "from when the screen opened" } else { "from the last line" }
    );

    line
}

type ScenarioLiveVoidFn = extern "C" fn(this: *mut Il2CppObject);
// The concert's own skip. Counted and never called from this module: whether a skip asked for on this
// screen does anything is the next decision, and it needs the count first.
def_detour! {
    ScenarioLiveCutControl_Skip(this: *mut Il2CppObject) {
            CUT_CONTROL_SKIP.count();

        get_orig_fn!(ScenarioLiveCutControl_Skip, ScenarioLiveVoidFn)(this);
    }
    bail {
                get_orig_fn!(ScenarioLiveCutControl_Skip, ScenarioLiveVoidFn)(this)
    }
}

type ScenarioLiveBoolFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
// A getter with a matching setter (`set_SkipApplyGrandLiveHighSpeedType/1` at
// `run log/introspect-run62.log:25438`), so it is sampled and never scaled: this fork has no setter
// beside it, which is the rule `resolve_getter` enforces for a lever and the same one DialogProbe's
// `GetBackButtonAnimationDelayTime` door follows.
def_detour! {
    ScenarioLiveTopModel_GetCanSkipLive(this: *mut Il2CppObject) answer -> bool {
            let value = get_orig_fn!(ScenarioLiveTopModel_GetCanSkipLive, ScenarioLiveBoolFn)(this);
        // Published before the counter: this is the game's own answer about whether a live may be
        // skipped, and a trip in the probe half is not the mod's decision to make.
        answer.publish(value);
        CAN_SKIP_LIVE.sample(flag_bit(value));
        record_answer(FLAG_CAN_SKIP, value);

        value
    }
}

type PhaseTransitionFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
// `TransitionGrandlivePhase/0 -> class<System.Collections.IEnumerator>()`: the coroutine factory the
// live steps its phases through, not its `MoveNext`. The iterator is handed back untouched and only the
// crossing is counted, which is the shape DialogProbe's `PlayOutView` door uses (A4).
def_detour! {
    PhaseGrandLiveAnimation_TransitionGrandlivePhase(this: *mut Il2CppObject) -> *mut Il2CppObject {
            PHASE_TRANSITION.count();

        get_orig_fn!(PhaseGrandLiveAnimation_TransitionGrandlivePhase, PhaseTransitionFn)(this)
    }
    bail {
                get_orig_fn!(PhaseGrandLiveAnimation_TransitionGrandlivePhase, PhaseTransitionFn)(this)
    }
}

// The live's own high speed lever: a getter with a matching setter
// (`set_SkipApplyGrandLiveHighSpeedType/1` at `run log/introspect-run62.log:25438`), so it is sampled and
// never scaled. This fork has no setter beside it, which is the rule `resolve_getter` enforces for a lever
// and the same one DialogProbe's `GetBackButtonAnimationDelayTime` door follows.
def_detour! {
    StoryController_GetSkipApplyGrandLiveHighSpeedType(this: *mut Il2CppObject) answer -> bool {
            let value = get_orig_fn!(StoryController_GetSkipApplyGrandLiveHighSpeedType, ScenarioLiveBoolFn)(this);
        answer.publish(value);
        SUPPRESS_LIVE_HIGH_SPEED.sample(flag_bit(value));
        record_answer(FLAG_SUPPRESS, value);

        value
    }
}

def_detour! {
    StoryController_PauseGrandLiveAutoHighSpeedSettings(this: *mut Il2CppObject) {
            PAUSE_LIVE_AUTO_HIGH_SPEED.count();

        get_orig_fn!(StoryController_PauseGrandLiveAutoHighSpeedSettings, ScenarioLiveVoidFn)(this);
    }
    bail {
                get_orig_fn!(StoryController_PauseGrandLiveAutoHighSpeedSettings, ScenarioLiveVoidFn)(this)
    }
}

def_detour! {
    StoryController_ResumeGrandLiveAutoHighSpeedSettings(this: *mut Il2CppObject) {
            RESUME_LIVE_AUTO_HIGH_SPEED.count();

        get_orig_fn!(StoryController_ResumeGrandLiveAutoHighSpeedSettings, ScenarioLiveVoidFn)(this);
    }
    bail {
                get_orig_fn!(StoryController_ResumeGrandLiveAutoHighSpeedSettings, ScenarioLiveVoidFn)(this)
    }
}

// The class's pair for all of its auto high speed (`:25591` and `:25592`), counted beside the live's so a
// run can say which of the two the game parked. A pause with no resume after it is the game's own decision
// to stop applying high speed, and this fork's story high speed writes keep landing while it stands.
def_detour! {
    StoryController_PauseAutoHighSpeedSettings(this: *mut Il2CppObject) {
            PAUSE_ALL_AUTO_HIGH_SPEED.count();

        get_orig_fn!(StoryController_PauseAutoHighSpeedSettings, ScenarioLiveVoidFn)(this);
    }
    bail {
                get_orig_fn!(StoryController_PauseAutoHighSpeedSettings, ScenarioLiveVoidFn)(this)
    }
}

def_detour! {
    StoryController_ResumeAutoHighSpeedSettings(this: *mut Il2CppObject) {
            RESUME_ALL_AUTO_HIGH_SPEED.count();

        get_orig_fn!(StoryController_ResumeAutoHighSpeedSettings, ScenarioLiveVoidFn)(this);
    }
    bail {
                get_orig_fn!(StoryController_ResumeAutoHighSpeedSettings, ScenarioLiveVoidFn)(this)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let cut_control = class_for_label(umamusume, "Gallop.SingleModeScenarioLiveCutControl");
    let top_model = class_for_label(umamusume, "Gallop.SingleModeScenarioLiveTopModel");
    let (phase_animation, phase_found) = phase_animation_class(umamusume);
    // The class that owns the concert's high speed levers is the controller, not the view: the dump prints
    // those five methods inside the block that starts at `run log/introspect-run62.log:25427`,
    // `=== Gallop.StoryViewController ===`, and the widened `Gallop.StoryView::` lines that end at `:25403`
    // belong to a different class. A first run written against the view armed none of them.
    let story_controller = class_for_label(umamusume, "Gallop.StoryViewController");

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

    // The labels named here are the census names, so a door that did not resolve can be looked up in
    // the dump by the same string the totals line prints.
    probe!(cut_control, ScenarioLiveCutControl_Skip, "Skip", NO_PARAMS, VOID, "SingleModeScenarioLiveCutControl::Skip()");
    probe!(top_model, ScenarioLiveTopModel_GetCanSkipLive, "get_CanSkipLive", NO_PARAMS, BOOL, "SingleModeScenarioLiveTopModel::get_CanSkipLive()");
    probe!(phase_animation, PhaseGrandLiveAnimation_TransitionGrandlivePhase, "TransitionGrandlivePhase", NO_PARAMS, CLASS, "PhaseGrandLiveAnimation::TransitionGrandlivePhase()");
    probe!(story_controller, StoryController_GetSkipApplyGrandLiveHighSpeedType, "get_SkipApplyGrandLiveHighSpeedType", NO_PARAMS, BOOL, "StoryViewController::get_SkipApplyGrandLiveHighSpeedType()");
    probe!(story_controller, StoryController_PauseGrandLiveAutoHighSpeedSettings, "PauseGrandLiveAutoHighSpeedSettings", NO_PARAMS, VOID, "StoryViewController::PauseGrandLiveAutoHighSpeedSettings()");
    probe!(story_controller, StoryController_ResumeGrandLiveAutoHighSpeedSettings, "ResumeGrandLiveAutoHighSpeedSettings", NO_PARAMS, VOID, "StoryViewController::ResumeGrandLiveAutoHighSpeedSettings()");
    probe!(story_controller, StoryController_PauseAutoHighSpeedSettings, "PauseAutoHighSpeedSettings", NO_PARAMS, VOID, "StoryViewController::PauseAutoHighSpeedSettings()");
    probe!(story_controller, StoryController_ResumeAutoHighSpeedSettings, "ResumeAutoHighSpeedSettings", NO_PARAMS, VOID, "StoryViewController::ResumeAutoHighSpeedSettings()");

    info!(
        "Concert probe: {installed} of {DOOR_COUNT} doors installed on the career Grand Live, watch lines on views {VISIT_VIEWS:?} first {} s after a screen opens and every {} s after that, up to {VISIT_LINE_LIMIT} per visit, nothing pressed and nothing written",
        VISIT_FIRST_US / 1_000_000,
        VISIT_EVERY_US / 1_000_000
    );

    // The class the walk answered for, spelled out because the dump never printed it and the next door has
    // to be written against it rather than guessed at again.
    if let Some(found) = phase_found.as_deref() {
        info!("Concert probe: the phase animation door armed on {found}, the class the dump spells as PhaseGrandLiveAnimation with no owner");
    }

    if !missing.is_empty() {
        info!("Concert probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// The visit line for the screen the game is standing on, printed on the quiet path every tick it is due.
// The clock is the suite's, so a visit line and a stall watchdog line for the same stay are measured on
// one ruler and can be read against each other.
fn report_visit(now_us: i64) {
    if now_us < 0 {
        return;
    }

    let view = SceneManager::requested_view_id();

    if !live_view(view) {
        // Closing a visit is a two store job, and it only has to happen once: at the 200 fps target this
        // runs every frame, and on a screen that is not a live it would otherwise write the same two
        // values 200 times a second (AGENTS section 6).
        if VISIT_VIEW.load(atomic::Ordering::Relaxed) != VIEW_NONE {
            VISIT_VIEW.store(VIEW_NONE, atomic::Ordering::Relaxed);
            VISIT_OPENED_US.store(-1, atomic::Ordering::Relaxed);
        }

        return;
    }

    if VISIT_VIEW.load(atomic::Ordering::Relaxed) != view {
        VISIT_VIEW.store(view, atomic::Ordering::Relaxed);
        VISIT_OPENED_US.store(now_us, atomic::Ordering::Relaxed);
        VISITS_SEEN.fetch_add(1, atomic::Ordering::Relaxed);
        VISIT_LINES.store(0, atomic::Ordering::Relaxed);

        for mark in LINE_MARKS.iter() {
            mark.store(0, atomic::Ordering::Relaxed);
        }

        for mark in STORY_MARKS.iter() {
            mark.store(0, atomic::Ordering::Relaxed);
        }

        reset_answers();

        return;
    }

    let opened_us = VISIT_OPENED_US.load(atomic::Ordering::Relaxed);
    let shown = VISIT_LINES.load(atomic::Ordering::Relaxed);

    if !visit_line_due(opened_us, now_us, shown) {
        return;
    }

    VISIT_LINES.store(shown + 1, atomic::Ordering::Relaxed);

    let wait = StoryFrameProbe::story_wait_frame_count();
    let waiting = StoryFrameProbe::story_waiting_frame_count();
    let check = StoryFrameProbe::story_or_live_wait_skipped_calls();
    let can_skip_reads = CAN_SKIP_LIVE.calls();
    let suppress_reads = SUPPRESS_LIVE_HIGH_SPEED.calls();
    let parts = VisitParts {
        view,
        line_no: shown + 1,
        stood_us: now_us - opened_us,
        counted: [
            (LINE_SKIP, CUT_CONTROL_SKIP.calls(), since_mark(CUT_CONTROL_SKIP.calls(), LINE_MARKS[DOOR_SKIP].load(atomic::Ordering::Relaxed))),
            (LINE_PHASE, PHASE_TRANSITION.calls(), since_mark(PHASE_TRANSITION.calls(), LINE_MARKS[DOOR_PHASE].load(atomic::Ordering::Relaxed))),
            (LINE_PAUSE, PAUSE_LIVE_AUTO_HIGH_SPEED.calls(), since_mark(PAUSE_LIVE_AUTO_HIGH_SPEED.calls(), LINE_MARKS[DOOR_PAUSE].load(atomic::Ordering::Relaxed))),
            (LINE_RESUME, RESUME_LIVE_AUTO_HIGH_SPEED.calls(), since_mark(RESUME_LIVE_AUTO_HIGH_SPEED.calls(), LINE_MARKS[DOOR_RESUME].load(atomic::Ordering::Relaxed))),
            (LINE_PAUSE_ALL, PAUSE_ALL_AUTO_HIGH_SPEED.calls(), since_mark(PAUSE_ALL_AUTO_HIGH_SPEED.calls(), LINE_MARKS[DOOR_PAUSE_ALL].load(atomic::Ordering::Relaxed))),
            (LINE_RESUME_ALL, RESUME_ALL_AUTO_HIGH_SPEED.calls(), since_mark(RESUME_ALL_AUTO_HIGH_SPEED.calls(), LINE_MARKS[DOOR_RESUME_ALL].load(atomic::Ordering::Relaxed))),
        ],
        flags: [
            (
                LINE_CAN_SKIP,
                FLAG_ANSWERS[FLAG_CAN_SKIP][0].load(atomic::Ordering::Relaxed),
                FLAG_ANSWERS[FLAG_CAN_SKIP][1].load(atomic::Ordering::Relaxed),
                can_skip_reads,
                since_mark(can_skip_reads, LINE_MARKS[DOOR_CAN_SKIP].load(atomic::Ordering::Relaxed)),
                read_age_ms(CAN_SKIP_LIVE.last_us(), now_us),
            ),
            (
                LINE_SUPPRESS,
                FLAG_ANSWERS[FLAG_SUPPRESS][0].load(atomic::Ordering::Relaxed),
                FLAG_ANSWERS[FLAG_SUPPRESS][1].load(atomic::Ordering::Relaxed),
                suppress_reads,
                since_mark(suppress_reads, LINE_MARKS[DOOR_SUPPRESS].load(atomic::Ordering::Relaxed)),
                read_age_ms(SUPPRESS_LIVE_HIGH_SPEED.last_us(), now_us),
            ),
        ],
        story: [
            (LINE_WAIT, wait.0, since_mark(wait.0, STORY_MARKS[0].load(atomic::Ordering::Relaxed)), Some(wait.1)),
            (LINE_WAITING, waiting.0, since_mark(waiting.0, STORY_MARKS[1].load(atomic::Ordering::Relaxed)), Some(waiting.1)),
            (LINE_SKIP_CHECK, check, since_mark(check, STORY_MARKS[2].load(atomic::Ordering::Relaxed)), None),
        ],
        story_mode: StoryTimelineController::IsStoryHighSpeedMode(),
        story_writes: StoryTimelineController::high_speed_write_count(),
        from_open: shown == 0,
    };

    info!("{}", visit_line(&parts));

    for (index, mark) in LINE_MARKS.iter().enumerate() {
        mark.store(PROBES[index].calls(), atomic::Ordering::Relaxed);
    }

    STORY_MARKS[0].store(wait.0, atomic::Ordering::Relaxed);
    STORY_MARKS[1].store(waiting.0, atomic::Ordering::Relaxed);
    STORY_MARKS[2].store(check, atomic::Ordering::Relaxed);
}

// Called from the GameSystem update detour beside the other probe reports: the visit lines first, so a
// line about a screen that is holding is not gated behind the totals having moved. One clock read, shared
// by both halves, and it is the suite's clock: the one the visit lines and the stall watchdog measure on.
pub fn report_if_due() {
    let now_us = elapsed_us();

    report_visit(now_us);

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();
    let now_sec = now_us / 1_000_000;
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
            let _ = write!(line, " {}={} peak {:.0}", probe.label(), calls, peak_seconds(probe.peak_bits()));
        }
        else {
            let _ = write!(line, " {}={}", probe.label(), calls);
        }
    }

    let opened_us = VISIT_OPENED_US.load(atomic::Ordering::Relaxed);
    let open_ms = if VISIT_VIEW.load(atomic::Ordering::Relaxed) == VIEW_NONE || opened_us < 0 { 0 } else { (now_us - opened_us) / 1_000 };

    info!(
        "Concert probe totals at {now_sec} s:{line} | grand live visits {} seen, {} lines printed for the visit in progress, open now {open_ms} ms",
        VISITS_SEEN.load(atomic::Ordering::Relaxed),
        VISIT_LINES.load(atomic::Ordering::Relaxed)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_career_live_screens_get_a_visit_clock() {
        assert!(live_view(1621), "the live itself is watched");
        assert!(live_view(1620), "the live selection top is watched too");
        assert!(!live_view(1101), "the training screen is this fork's own cut work");
        assert!(!live_view(1501), "the Inspiration event is watched by its own probe");
        assert!(!live_view(VIEW_NONE));
    }

    #[test]
    fn a_visit_gets_its_first_line_at_five_seconds_and_one_every_ten_after_that() {
        assert!(!visit_line_due(0, 4_999_999, 0), "a visit that has not reached the mark prints nothing");
        assert!(visit_line_due(0, 5_000_000, 0));
        assert!(!visit_line_due(0, 14_999_999, 1));
        assert!(visit_line_due(0, 15_000_000, 1));
        assert!(visit_line_due(0, 55_000_000, 5));
        assert!(!visit_line_due(0, 65_000_000, 6), "a visit prints no more than six lines");
        assert!(!visit_line_due(-1, 50_000_000, 0), "a clock that never opened a visit prints nothing");
    }

    #[test]
    fn a_visit_line_names_the_view_the_stay_and_what_moved_since_the_last_line() {
        let parts = VisitParts {
            view: 1621,
            line_no: 2,
            stood_us: 45_123_000,
            counted: [
                (LINE_SKIP, 3, 1),
                (LINE_PHASE, 1, 0),
                (LINE_PAUSE, 0, 0),
                (LINE_RESUME, 0, 0),
                (LINE_PAUSE_ALL, 2, 2),
                (LINE_RESUME_ALL, 1, 1),
            ],
            flags: [(LINE_CAN_SKIP, 40, 1, 41, 12, 3_400), (LINE_SUPPRESS, 0, 0, 0, 0, -1)],
            story: [
                (LINE_WAIT, 184, 12, Some(96)),
                (LINE_WAITING, 194, 0, Some(0)),
                (LINE_SKIP_CHECK, 26_515, 0, None),
            ],
            story_mode: true,
            story_writes: 2,
            from_open: false,
        };
        let line = visit_line(&parts);

        assert!(line.starts_with("Concert visit 2: view 1621 stood 45 s,"), "{line}");
        assert!(line.contains("live skip door 3 (+1)"), "{line}");
        assert!(line.contains("grand live phase transition 1 (+0)"), "{line}");
        assert!(line.contains("all auto high speed paused 2 (+2)"), "{line}");
        assert!(line.contains("can skip live answered 40 no 1 yes in this visit, read 41 times (+12), last read 3400 ms ago"), "{line}");
        assert!(line.contains("grand live high speed suppress flag never read by the game"), "{line}");
        assert!(line.contains("story wait frames 96 frames, 184 polls (+12)"), "{line}");
        assert!(line.contains("story or grand live wait skip check 26515 polls (+0)"), "{line}");
        assert!(line.ends_with("story high speed mode 1 writes 2 | deltas from the last line"), "{line}");
    }

    #[test]
    fn a_first_line_says_where_its_deltas_come_from() {
        let mut parts = VisitParts {
            view: 1620,
            line_no: 1,
            stood_us: 5_000_000,
            counted: [(LINE_SKIP, 0, 0), (LINE_PHASE, 0, 0), (LINE_PAUSE, 0, 0), (LINE_RESUME, 0, 0), (LINE_PAUSE_ALL, 0, 0), (LINE_RESUME_ALL, 0, 0)],
            flags: [(LINE_CAN_SKIP, 0, 0, 0, 0, -1), (LINE_SUPPRESS, 0, 0, 0, 0, -1)],
            story: [(LINE_WAIT, 0, 0, Some(-1)), (LINE_WAITING, 0, 0, Some(-1)), (LINE_SKIP_CHECK, 0, 0, None)],
            story_mode: false,
            story_writes: 0,
            from_open: true,
        };
        parts.from_open = false;

        assert!(visit_line(&parts).ends_with("deltas from the last line"));

        parts.from_open = true;
        assert!(visit_line(&parts).ends_with("deltas from when the screen opened"));
    }

    #[test]
    fn the_doors_are_the_ones_the_widened_dump_names_and_in_the_order_the_marks_read_them() {
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();

        assert_eq!(PROBES.len(), DOOR_COUNT);
        assert_eq!(labels[DOOR_SKIP], "SingleModeScenarioLiveCutControl::Skip()");
        assert_eq!(labels[DOOR_CAN_SKIP], "SingleModeScenarioLiveTopModel::get_CanSkipLive()");
        assert_eq!(labels[DOOR_PHASE], "PhaseGrandLiveAnimation::TransitionGrandlivePhase()");
        assert_eq!(labels[DOOR_SUPPRESS], "StoryViewController::get_SkipApplyGrandLiveHighSpeedType()");
        assert_eq!(labels[DOOR_PAUSE], "StoryViewController::PauseGrandLiveAutoHighSpeedSettings()");
        assert_eq!(labels[DOOR_RESUME], "StoryViewController::ResumeGrandLiveAutoHighSpeedSettings()");
        assert_eq!(labels[DOOR_PAUSE_ALL], "StoryViewController::PauseAutoHighSpeedSettings()");
        assert_eq!(labels[DOOR_RESUME_ALL], "StoryViewController::ResumeAutoHighSpeedSettings()");

        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();

        assert_eq!(sorted.len(), labels.len(), "two concert doors share a label in the suite");
    }

    #[test]
    fn the_answer_counters_sit_on_the_two_doors_that_hand_out_an_answer() {
        // The per visit counters file a false answer at index 0 and a true one at 1, and only the two state
        // getters hand out an answer. A void door's crossings filed under an answer index would print a
        // pause as a number of times the game said yes.
        assert_eq!(usize::from(false), 0);
        assert_eq!(usize::from(true), 1);
        assert!(PROBES[DOOR_CAN_SKIP].is_peaked() && PROBES[DOOR_SUPPRESS].is_peaked(), "the doors the answers are counted on are the two sampled getters");
        assert!(!PROBES[DOOR_SKIP].is_peaked() && !PROBES[DOOR_PAUSE].is_peaked(), "a counted door has no answer to count");
    }

    #[test]
    fn this_module_counts_the_games_own_doors_and_presses_nothing_of_its_own() {
        // Nothing here is a button handler, a setter, or a write: the doors are the concert's skip
        // control, its permission read, its phase coroutine, and the story controller's own high speed
        // levers.
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();

        assert!(!labels.iter().any(|label| label.contains("set_")), "a probe module that writes is a lever wearing a probe's clothes");
        assert!(!labels.iter().any(|label| label.contains("OnClick")), "the game's own button is not a census door");
        assert!(!labels.iter().any(|label| label.contains("MoveNext")), "a coroutine's step door is not a factory door");
    }

    #[test]
    fn a_restarted_census_reads_as_no_crossings_and_not_a_huge_number() {
        assert_eq!(since_mark(4, 9), 0, "a count below its mark is a restart, not a negative delta");
        assert_eq!(since_mark(9, 4), 5);
        assert_eq!(since_mark(0, 0), 0);
        assert_eq!(read_age_ms(-1, 900_000), -1, "a door never crossed has no age");
        assert_eq!(read_age_ms(400_000, 900_000), 500);
        assert_eq!(read_age_ms(900_000, 400_000), -1, "a clock that went backwards has no age");
    }
}
