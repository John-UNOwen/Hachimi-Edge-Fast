// Gallop hard-codes most of its screen animation timing in `static readonly`
// duration constants (FADE_TIME, COUNTUP_DURATION, NEXT_WAIT_DURATION, ...). Those
// are the animations that play *between* scenes and after a training turn, so they
// are not covered by Time.timeScale (many of them run on unscaled DOTween time).
//
// This module reads the shipped value of every known constant once, remembers it, and
// rewrites `original / factor` whenever the matching option changes. Because the
// original value is remembered, re-applying is idempotent: dragging a slider back and
// forth never compounds. Fields that do not exist in the current client are skipped
// silently, so one table serves every region.
use std::ffi::{CStr, CString};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use once_cell::sync::Lazy;

use crate::core::Hachimi;
use crate::core::hachimi::Config;
use crate::il2cpp::hook::umamusume::TrainingCuttProbe;
use crate::il2cpp::{
    api::{
        il2cpp_class_from_name, il2cpp_class_get_field_from_name, il2cpp_class_get_method_from_name,
        il2cpp_class_instance_size,
        il2cpp_field_get_flags, il2cpp_field_get_type, il2cpp_field_get_value_object, il2cpp_field_is_literal,
        il2cpp_field_static_get_value, il2cpp_field_static_set_value, il2cpp_method_get_param,
        il2cpp_method_get_return_type, il2cpp_type_get_class_or_element_class, il2cpp_type_get_name,
    },
    types::*,
};

const FIELD_ATTRIBUTE_STATIC: c_int = 0x10;
const FIELD_ATTRIBUTE_LITERAL: c_int = 0x40;

// Upper bound on how much of an animation may be removed in one step. Public because a timing preset
// names the same ceiling it writes (core::speed_preset), and the number a preset may offer has to be
// the number this module honours.
pub const MAX_FACTOR: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    // Screen-to-screen transitions: view change fades, wipes, loading overlays.
    Transition,
    // Race and career result screens: content fades, count-ups, reward cascades. This is the group the
    // GUI's Result Screen Animation Speed option writes, and it is now exactly what that option's name
    // promises. It used to carry the training turn's gates too, which is C58.
    Screens,
    // Story cutscene timeline.
    Story,
    // The training turn's own gates: the stat plate cascade, the HP gauge blend, the cut status panel,
    // and the plate classes' own durations. This group has no lever - no option, no slider, no preset
    // arm writes it (C58, ledger item 59). Every duration in it is the length of a coroutine or a tween
    // sequence whose completion is what the turn's coroutine resumes on, so shortening one changes what
    // the flow waits for rather than how long a screen takes to look done. The doors in it stay armed
    // because a run has to be able to read them; `factor` holds them at the number the game chose.
    Training,
}

// Which clock a duration door's completion is measured on, and therefore whether the two speed layers meet
// on it at all (C58, ledger item 62).
//
// The bound is a statement about a completion: a completion takes the duration it was handed over the clock
// it is advanced on, so the pair composes only where the duration a door shortened and the clock
// `tween_clocks` multiplies are both about that one completion. What `duration_factor` priced before this
// enum existed was a `Group`, and a group names which option wrote a lever. It says nothing about what the
// value paces. That is how a bound written to stop 400x on one completion also reached every value in the
// group, including the ones the clock layer never advances - a Unity coroutine yield (`WaitProbe`'s two
// doors; run 18 armed 7 of them for 6,150 ms and `ui_animation_scale` is on none of them), a
// `WaitForFixedUpdate` poll (run 21 read one in all five training cuts), a timeline a component steps in
// `LateUpdate`, and the `independentTime` half of a tween the game marked time scale independent, which
// `f1a6584` took out of the multiply (C5). A value on those channels trimmed by `ui_animation 20` runs at
// 1.0 where its own option asked for up to `MAX_FACTOR`: up to 20x slower in wall clock than the state every
// run in this ledger measured, and a one-layer completion at `MAX_FACTOR` is already under the ceiling the
// bound exists to hold, so the trim bought no safety there.
//
// The other half is just as fixed: a pace this fork has not read is not a licence to drop the bound. The
// count-up door is the one `Group::Screens` duration door a career session has been measured reaching
// (`CountupModifier_getDuration 0.16 -> 0.008`, run 17, in the same snapshot as `result 20`), and the dump
// this fork read for its classification names count-ups on both channels, so the classification is
// unproven, and unproven is the bounded side. `Gallop.PartsFanRaidFanNumCounter::CountUp/2 ->
// IEnumerator(long, bool)` (`introspect.log:16707`) steps one through a coroutine and its `MoveNext/0`
// (16693) is the machine doing it, while `Gallop.TextCountUpVertexCommon` (15873-15893) renders one through
// `_tween [class<DG.Tweening.Tween>]` and `_timeLine [class<Gallop.TweenAnimationTimelineComponent>]`, and
// `Gallop.PartsSingleModeResultFanRaid::CountUpTextFadeInFromRight/7 (class<Gallop.CountupModifier>,
// class<UnityEngine.UI.Text>, ulong, ulong, float, float, class<System.Action>)` (24196) feeds one into a
// result screen tween beside `AnimationDelayFunc/2 -> void(float, Action)` (24197) and the `countUpDelay` its
// closures capture (24189-24192). Nothing in a signature dump says which of those the 0.16 s lands on, and
// `measured_pace` is the arithmetic that says so when a run has read one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pace {
    /// The value is handed to animation measured on the delta channel `tween_clocks` multiplies. The two
    /// layers compose on this completion, and `MAX_TWEEN_SPEED_PRODUCT` is the ceiling on it. The payload is
    /// the dump line, or the run line, that put the door here.
    ///
    /// The lane says which channel, not which levers: that channel is the game's `Time.deltaTime`, so every
    /// clock this fork writes is under a door on it, `ui_animation_scale` and the whole scale the
    /// `Time.timeScale` write layer left in the game together (`delta_clock`), not the ui lever on its own. A
    /// door on this lane is priced by every lever the bound covers.
    TweenMeasured(&'static str),

    /// The value is consumed on a channel `ui_animation_scale` provably does not reach. No pair composes on
    /// it: the group's own `MAX_FACTOR` cap is the whole of the ceiling it needs, so the trim is not applied
    /// and the door hands its group's full ask. Taking a speed-up out of a duration on this lane is a
    /// slowdown of a completion the clock layer never paid for, which is the shape AGENTS section 5 refuses.
    /// The payload names the channel and the reading that put the door here.
    OffTweenClock(&'static str),

    /// Nothing this fork has read says which channel the completion runs on. Bounded as if composed, against
    /// `delta_clock()`, because the failure this item was written to stop is the fast one; the cost of the
    /// safe side and the measurement that would move the door are written at the door.
    Unproven,
}

impl Pace {
    /// Whether this pace leaves the door under `MAX_TWEEN_SPEED_PRODUCT`.
    pub const fn bounds_the_pair(self) -> bool {
        match self {
            Pace::OffTweenClock(_) => false,
            Pace::TweenMeasured(_) | Pace::Unproven => true,
        }
    }

    /// The evidence a pace stands on: `None` only for `Unproven`, which is the honest answer and the reason
    /// a door on it is bounded.
    pub const fn evidence(self) -> Option<&'static str> {
        match self {
            Pace::TweenMeasured(line) | Pace::OffTweenClock(line) => Some(line),
            Pace::Unproven => None,
        }
    }
}

#[derive(Clone, Copy)]
struct FieldSpec {
    class: &'static str,
    field: &'static str,
    group: Group,
}

macro_rules! spec {
    ($class:literal, $field:literal, $group:path) => {
        FieldSpec { class: $class, field: $field, group: $group }
    };
}

// Every name below was read out of the live client metadata (debug_mode introspect.log).
const FIELDS: &[FieldSpec] = &[
    // --- Screen transitions ---
    spec!("ChangeOrientationParam", "FADE_OUT_TIME_IN_CHANGE_VIEW", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_IN_TIME_IN_CHANGE_VIEW", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_OUT_TIME_PUSH_STORY_BUTTON", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_IN_TIME_PUSH_STORY_BUTTON", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_OUT_TIME_PUSH_LIVE_BUTTON", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_IN_TIME_PUSH_LIVE_BUTTON", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_OUT_TIME_PUSH_STORY_RACE_BUTTON", Group::Transition),
    spec!("ChangeOrientationParam", "FADE_IN_TIME_PUSH_STORY_RACE_BUTTON", Group::Transition),
    spec!("SceneManager", "CHANGE_RESOLUTION_FADE_OUT_TIME", Group::Transition),
    spec!("SceneManager", "CHANGE_RESOLUTION_FADE_IN_TIME", Group::Transition),
    spec!("NowLoading", "FADE_TIME", Group::Transition),
    spec!("NowLoading", "BLACK_FADE_TIME", Group::Transition),
    spec!("NowLoading", "WHITE_OUT_HORSE_SHOE_FADE_TIME", Group::Transition),

    // --- Race and career result screens ---
    spec!("SingleModeResultContentBase", "FADE_DURATION", Group::Screens),
    spec!("SingleModeResultContentBase", "FADE_OFFSET", Group::Screens),
    spec!("SingleModeResultContentBase", "DELAY_OFFSET", Group::Screens),
    spec!("SingleModeResultContentBase", "COUNTUP_DURATION", Group::Screens),
    spec!("SingleModeResultSequence", "DIALOG_BEFORE_INTERVAL_TIME", Group::Screens),
    spec!("PartsSingleModeResultSupportCardExp", "COUNTUP_DURATION", Group::Screens),
    spec!("PartsSingleModeResultRankScore", "HIDE_DURATION", Group::Screens),
    spec!("PartsTeamBuildingRaceResultRewardList", "SCOUT_TIME_BLUR_DURATION", Group::Screens),
    spec!("TeamStadiumGrandResultViewController", "DURATION", Group::Screens),
    spec!("TeamStadiumGrandResultViewController", "FADE_END", Group::Screens),
    spec!("TeamStadiumGrandResultViewController", "WAITING_FOR_COUNTUP", Group::Screens),
    spec!("TeamStadiumGrandResultViewController", "DELAY_SHOW_SKIP_BUTTON", Group::Screens),
    spec!("SingleModeScenarioTeamRaceGrandResultViewController", "DURATION", Group::Screens),
    spec!("SingleModeScenarioTeamRaceGrandResultViewController", "FADE_END", Group::Screens),
    spec!("HeroesStage1GrandResultViewController", "SHOW_DARK_DURATION", Group::Screens),
    spec!("HeroesStage1GrandResultViewController", "HIDE_DARK_DURATION", Group::Screens),
    spec!("HeroesStage1GrandResultViewController", "DELAY_DELTA", Group::Screens),
    spec!("DialogSingleModeResultDifficultyBox", "SKIP_ENABLE_DELAY", Group::Screens),
    spec!("DialogSingleModeResultDifficultyBox", "GAUGE_UP_DURATION", Group::Screens),
    spec!("DialogSingleModeResultDifficultyBox", "MINI_CHARA_IN_FADE_TIME", Group::Screens),
    spec!("DialogSingleModeResultDifficultyBox", "MINI_CHARA_OUT_FADE_TIME", Group::Screens),
    spec!("DialogSingleModeResultUpdateRankScoreRanking", "CONTENT_FADE_OFFSET", Group::Screens),
    spec!("DialogSingleModeResultUpdateRankScoreRanking", "CONTENT_FADE_DURATION", Group::Screens),
    spec!("DialogSingleModeResultUpdateRankScoreRanking", "SKIP_ENABLE_DELAY", Group::Screens),
    spec!("DialogSingleModeResultUpdateScenarioRecord", "CONTENT_FADE_OFFSET", Group::Screens),
    spec!("DialogSingleModeResultUpdateScenarioRecord", "CONTENT_FADE_DURATION", Group::Screens),
    spec!("DialogSingleModeResultUpdateScenarioRecord", "SKIP_ENABLE_DELAY", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_HONOR", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_WIN", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_WIN_SMALL", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_LOSE", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_WIN_WITH_HONOR", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_WIN_SMALL_WITH_HONOR", Group::Screens),
    spec!("PartsSingleModeResultDifficultyRandomReward", "EFFECT_DELAY_LOSE_WITH_HONOR", Group::Screens),

    // --- The training turn's own gates ---
    //
    // These five stood in `Group::Screens` until item 59. They are the schedule the stat plate cascade
    // and the training cut are built on - a plate's typewriter length, the wait between plates, the
    // high speed animation time of the A2U plate, the result flash labels the cutt controller plays -
    // and a duration the turn resumes on is not a result screen animation. `result_screen_speed` may not
    // reach them. On this client every one is a `static const float` IL2CPP folded into its call sites
    // (`X is a compile-time constant`, 0 field writes in every apply pass, C13), so this is the half of
    // the group that was never live; it moves because it would be the same defect on a client where the
    // fields have storage.
    spec!("TrainingParamChangeA2U", "ANIMATION_TIME_HIGH_SPEED", Group::Training),
    spec!("TrainingParamChangePlate", "TYPEWRITE_DURATION", Group::Training),
    spec!("TrainingParamChangePlate", "NEXT_WAIT_DURATION", Group::Training),
    spec!("SingleModeMainTrainingCuttController", "FLASH_LABEL_SPEED_UP_SUCCESS_IN", Group::Training),
    spec!("SingleModeMainTrainingCuttController", "FLASH_LABEL_SPEED_UP_FAILURE_IN", Group::Training),

    // --- Story cutscenes ---
    spec!("StoryViewController", "CHARACTER_FADE_DURATION", Group::Story),
    spec!("StoryViewController", "SINGLE_MODE_STILL_FADE_TIME", Group::Story),
    spec!("StoryViewController", "DELAY_AFTER_PARAMETER_CHANGE", Group::Story),
    spec!("StoryTimelineController", "FADE_TIME_FOR_HIGH_SPEED", Group::Story),
    spec!("StoryTimelineController", "TOUCH_BLOCK_INTERVAL", Group::Story),
    spec!("StoryTimelineController", "CONTINUOUS_TOUCH_INTERVAL", Group::Story),
    spec!("StoryTimelineTextClipData", "TYPEWRITER_WAIT_FRAME", Group::Story),
    spec!("StoryTimelineScreenEffectClipData", "FLASH_FADE_FRAME_COUNT", Group::Story),
    spec!("StoryTimelineTrainingCuttClipData", "DelayFrame", Group::Story),
];

struct Entry {
    class: &'static str,
    field: &'static str,
    group: Group,
    info: usize,
    kind: Il2CppTypeEnum,
    // The baseline we scale: the last value the game itself put in this field. It is
    // re-observed whenever the current value differs from what we last wrote, so a
    // constant stays a constant and a value the game reassigns at runtime gets scaled
    // from its new value instead of a stale snapshot. NAN means "never written".
    original: f64,
    last_written: f64,
}

static ENTRIES: Lazy<Mutex<Vec<Entry>>> = Lazy::new(|| Mutex::new(Vec::new()));

// Config was changed on the GUI thread; the rewrite happens on the game thread.
static DIRTY: AtomicBool = AtomicBool::new(false);

// The factor each group's fields were last written at, NAN while a group has never been
// written. `normalize` clamps a real factor to 1.0..=MAX_FACTOR, so NAN cannot collide
// with one, and "never written" is exactly the game state a neutral factor asks for.
// This is the applied marker `Time.rs`'s APPLIED_LEVER is for the lever: a field is
// rewritten once per change of its group's factor, and a pass that finds the factor it is
// about to write already applied has nothing left to do. Returning a factor to 1.0 still
// performs the single pass that puts the shipped values back, because the marker holding
// the old factor differs from the 1.0 being asked for now.
// Indexed by `group_index`: Transition, Screens, Story, Training. The Training marker is never written:
// that group has no factor to apply (C58), so `plan_group` finds it satisfied by the game's own values
// and its marker stays NAN for the life of the process.
static APPLIED_FACTORS: [AtomicU32; 4] = [const { AtomicU32::new(f32::NAN.to_bits()) }; 4];

// What a pass through `apply` costs, charged by the pass itself. C36 is a claim about cost, and a
// cost claim belongs in `hachimi.log` (AGENTS section 2: a change that cannot be shown in the log
// is not done), not in a counter a test module keeps over its own copy of the pass. `apply` is
// reached once per config change - `apply_if_dirty` from the game tick - and once per view change
// through `SceneManager::ChangeView`, so the line is the fork's usual pattern: the first
// `PASS_DETAIL_LIMIT` passes in full, then a totals line every `PASS_CHUNK` passes.
const PASS_DETAIL_LIMIT: usize = 6;
const PASS_CHUNK: usize = 64;

static APPLY_PASSES: AtomicUsize = AtomicUsize::new(0);
static APPLY_TABLE_PASSES: AtomicUsize = AtomicUsize::new(0);
static APPLY_CONFIG_READS: AtomicUsize = AtomicUsize::new(0);
static APPLY_ENTRY_LOCKS: AtomicUsize = AtomicUsize::new(0);
static APPLY_FIELD_READS: AtomicUsize = AtomicUsize::new(0);
static APPLY_FIELD_WRITES: AtomicUsize = AtomicUsize::new(0);

/// Charge a `config.load()` to the running pass. `refresh_config_mirrors` calls it, and so do
/// `HighSpeedSetting::apply` and `StoryTimelineController::apply_config`: their config read is
/// part of the same pass, and a pass cost that leaves the two game setting halves out is the
/// half of the cost C36 never measured.
pub fn note_config_read() {
    APPLY_CONFIG_READS.fetch_add(1, Ordering::Relaxed);
}

// Which passes get a line: the first `PASS_DETAIL_LIMIT` in full, then one every `PASS_CHUNK`.
// A career run reaches a few dozen view changes, so the detail window has to cover it; the chunk
// is what keeps a session with a lot of them from writing a line per scene.
fn pass_is_worth_logging(pass: usize) -> bool {
    pass <= PASS_DETAIL_LIMIT || pass % PASS_CHUNK == 0
}

/// The pass totals a run reads. `apply` calls it on every exit path past the guard that stops a
/// pass before it reads anything, including the one that bails out before the entry lock, because
/// a pass that did nothing is the fact the counters exist to carry.
fn report_pass(pass: usize) {
    if pass_is_worth_logging(pass) {
        debug!(
            "AnimationSpeed apply pass {pass}: {} config reads, {} entry locks, {} table passes, {} field reads, {} field writes",
            APPLY_CONFIG_READS.load(Ordering::Relaxed),
            APPLY_ENTRY_LOCKS.load(Ordering::Relaxed),
            APPLY_TABLE_PASSES.load(Ordering::Relaxed),
            APPLY_FIELD_READS.load(Ordering::Relaxed),
            APPLY_FIELD_WRITES.load(Ordering::Relaxed),
        );
    }
}

// Mirrored for hot paths (timeline getters run every frame while a cutscene plays).
static TRANSITION_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static SCREENS_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
// The plate cascade lever, mirrored apart from the groups. `Group::Training` is shared by the plate interval and
// the HP gauge blend time, and only one of those two is a completion the multiplied clock demonstrably does not
// measure, so the door that earned a lever has its own (C58, ledger item 74).
static PLATE_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
// The training cut-in speed lever, mirrored apart from the groups for the same reason the plate lever is, and
// priced apart from them too. The door it reaches hands the cut-in engine a *scale*, not a duration, so there is
// no pair to price: `MAX_TWEEN_SPEED_PRODUCT` bounds a completion the ui clock and a duration write both reach,
// and this value is not measured on the ui clock (ledger item 77). Its own ceiling is
// `MAX_TRAINING_CUT_TIME_SCALE`.
static TRAINING_CUT_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
// The tag cut-in effect lever. Run 40 measured 1214 and 1223 ms between `PlayCutIn` and its `done` action
// coming back as `PlayCutInOut`, which is the whole difference between a friendship cut (1931, 2239 ms) and a
// tag answer cut (1430, 1365, 1965 ms) on the same arm, and nothing in this fork reaches that leg: the effect
// is an `Animator` (`CreateLineEffect/1 -> Animator(class<Transform>)`, `_topLineAnimator`, `_bottomLineAnimator`,
// `PlayLineEffect/0`), so there is no duration argument to shorten and no scale door to raise. This lever
// writes the Animator's own speed, on the two doors that hand an Animator into the effect.
static TAG_CUT_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
// `UnityEngine.Animator::set_speed(System.Single)`, resolved once out of `UnityEngine.AnimationModule.dll`, and
// the two line animator fields of the tag cut-in player as `FieldInfo` addresses. A zero here means the lever
// has nowhere to write, which is the same inert shape as a hook that never resolved (never call address 0).
static ANIMATOR_SET_SPEED_ADDR: AtomicUsize = AtomicUsize::new(0);
static ANIMATOR_GET_SPEED_ADDR: AtomicUsize = AtomicUsize::new(0);
static TAG_CUT_TOP_ANIMATOR_FIELD: AtomicUsize = AtomicUsize::new(0);
static TAG_CUT_BOTTOM_ANIMATOR_FIELD: AtomicUsize = AtomicUsize::new(0);
// The first few writes are printed, so a run can tell "the lever is neutral" from "the Animator was not in hand"
// from "the game never reached the door" (A4). Counted, not locked, because this door runs on the cut path.
static TAG_CUT_WRITES_LOGGED: AtomicUsize = AtomicUsize::new(0);
// The clock the cut-in effect really runs on. Run 45 reached `AnimateToUnity.AnMotion` through
// `Gallop.FlashPlayer._motion` (`introspect.log:28743`, image `Plugins.dll`), and its speed door is
// `SetMotionSpeed/2 -> void(float, bool)` with `get_MotionSpeed/0 -> float()` beside it. Run 44 proved the flash
// player's own `Play/3` doors are hot on the training screen (1926 calls) and hand a label plus an int of 0, so the
// speed is not an argument there and this lever has to write on the motion object instead.
static MOTION_GET_SPEED_ADDR: AtomicUsize = AtomicUsize::new(0);
static MOTION_SET_SPEED_ADDR: AtomicUsize = AtomicUsize::new(0);
static FLASH_MOTION_FIELD: AtomicUsize = AtomicUsize::new(0);
static MOTION_SPEED_WRITES_LOGGED: AtomicUsize = AtomicUsize::new(0);
static STORY_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static TIME_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static UI_ANIMATION_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

// What the `Time.timeScale` write layer last handed Unity's setter, and the factor it added to the value it
// was handed, mirrored so the tween clock prices a completion without a config load. The pair ceiling reads
// the first one and the log line names both: `Time.deltaTime` is `Time.timeScale` times real elapsed time,
// so the channel `ui_animation_scale` multiplies carries the whole number this layer put into
// `Time.timeScale`, not only the factor it added to the game's request. 1.0 on both means Unity is holding
// the neutral speed, which is what every recorded run read: `Time::set_timeScale call 2: 1 -> 1 (lever x5)`
// in run 30 with `time_scale 5` in that run's snapshot, because `apply_time_scale` raises only a value the
// game wrote above 1.0 and this client holds 1.0. `UnityEngine_CoreModule::Time` is the only writer, and
// both mirrors move in that one call (C58, item 62).
//
// The record is of what this layer handed the setter, which is the only way this module knows the number: the
// `set_timeScale` detour is on Unity's own setter, so every game write arrives through it, and the two ceilings
// this fork owns (`apply_time_scale` and the story getters' `scale_time_scale`) both stop at `MAX_TIME_SCALE`.
// A scale this fork could not have produced is therefore a pass-through, and the line names it: run 19 read
// `StoryTimelineController_setTimeScale 8` with no `set_timeScale` call line beside it, so Unity held 1.0 there.
static TIME_SCALE_PRODUCED: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static TIME_SCALE_RAISE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

// The turn every test case that moves or reads these mirrors takes. `UnityEngine_CoreModule::Time`'s
// produced write moves `TIME_SCALE_PRODUCED` and `TIME_SCALE_RAISE`, which the completion ceiling reads and
// the ceiling line names, so its cases hold the same turn; `hook_turn` takes it before its own lock, which is
// the only order either module uses.
#[cfg(test)]
static SPEED_MIRROR_TURN: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn speed_mirror_turn() -> std::sync::MutexGuard<'static, ()> {
    SPEED_MIRROR_TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// The owed lines are the evidence a speed item closes on (AGENTS section 2), so a test has to read the
// sentence a counter actually wrote, not just the bool saying one was owed: a line no test ever looked at
// is where C58's `over the 2.5x ... (1x: it raised nothing)` sat unnoticed. The `log` crate holds one
// logger per process, so every case that reads a line back shares this capture and installs it once - two
// captures each trying to install would send the loser's lines to the winner's sink and leave the loser
// reading an empty one. The case's `speed_mirror_turn` guard is what keeps another case's lines out of the
// window; a caller that only counts lines holding one needle is not disturbed by the rest.
#[cfg(test)]
static CAPTURED_LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
static CAPTURED_INSTALL: std::sync::Once = std::sync::Once::new();

#[cfg(test)]
struct CapturedLog;

#[cfg(test)]
impl log::Log for CapturedLog {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        let line = record.args().to_string();
        CAPTURED_LINES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(line);
    }

    fn flush(&self) {}
}

#[cfg(test)]
static CAPTURED_LOG: CapturedLog = CapturedLog;

/// Open a capture window: install the process logger if no case has taken it yet, drop what earlier cases
/// left behind, and read at the level every owed `debug!` line is written at.
#[cfg(test)]
pub(crate) fn capture_log() {
    CAPTURED_INSTALL.call_once(|| {
        let _ = log::set_logger(&CAPTURED_LOG);
    });

    log::set_max_level(log::LevelFilter::Trace);

    CAPTURED_LINES.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
}

/// The captured lines holding `needle`, oldest first.
#[cfg(test)]
pub(crate) fn captured_lines(needle: &str) -> Vec<String> {
    CAPTURED_LINES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .iter()
        .filter(|line| line.contains(needle))
        .cloned()
        .collect()
}

// The story choice multiplier both sites read, already clamped. NAN means "leave the game's value
// alone", which is what a non finite or non positive delay means and what every build holds until
// the first config pass.
static STORY_CHOICE_AUTO_SELECT_MULT: AtomicU32 = AtomicU32::new(f32::NAN.to_bits());

// A ceiling on the time-scale lever, independent of the slider: a game path that reads
// a scaled value back and writes it again would otherwise compound every frame. Every
// layer that puts a number into `Time.timeScale` shares it, the story getters and the
// Unity write hook in `UnityEngine_CoreModule::Time` alike, so the two cannot stack
// past one bound.
pub const MAX_TIME_SCALE: f32 = 5.0;
// The floor of the same lever is its neutral 1.0: a time scale only goes up (AGENTS section 5),
// so a lever below 1.0 is not a slow down this option is allowed to apply, and a hand edited
// config asking for one falls back to doing nothing instead of multiplying the game's own fast
// forward down (C40).
pub const MIN_TIME_SCALE: f32 = 1.0;

// The ceiling on the speed this fork writes onto a training tag cut-in `Animator`. It is a rate on an animation
// Unity advances on `Time.timeScale`, which this fork's write layer already holds raised, so the pair is priced
// the way C58 prices the ui clock: the value written stops at `MAX_TWEEN_SPEED_PRODUCT` over the scale last
// handed to Unity's setter, and at this ceiling, and never below the neutral 1.0. The ceiling is `MAX_TIME_SCALE`
// because an Animator already playing at the game's own speed has no use for plate-lever speed, and because a
// speed this fork writes is a request to play faster, not a request to skip frames.
pub const MAX_TAG_CUT_ANIMATOR_SPEED: f32 = MAX_TIME_SCALE;

// The ceiling on the speed this fork writes onto an `AnimateToUnity.AnMotion`, the object a training cut-in effect
// is played on. It is priced the same way as the Animator lever: AnimateToUnity advances its own time on Unity's
// clock, so the pair with `Time.timeScale` is bounded by `MAX_TWEEN_SPEED_PRODUCT`. The ceiling itself is stated
// rather than inherited, because a run measured this one: `MAX_TIME_SCALE` on this door halved both training cut
// walls (friendship 1057 ms against 2380, tag answer 1181 against 1430, tag leg 324 ms against 1323) and taking the
// lever away brought them back (eight cuts at 1713 to 2164 ms), so the halving is this lever's and 5.0 is a floor
// the measurement has not reached. `2.0 * MAX_TIME_SCALE` is where the next run looks, and it is still inside the
// pair bound on the scale this layer actually holds: on both measurement runs `Time::set_timeScale` printed no call
// line, so the produced scale was 1.0 and the bound stood at 20.
pub const MAX_MOTION_SPEED: f32 = 2.0 * MAX_TIME_SCALE;

// The reach of the `training_cut_speed` control itself, stated because one slider now drives two channels and
// each caps what it hands in its own door: the cut-in scale door stops at `MAX_TRAINING_CUT_TIME_SCALE` (30.0) and
// the cut-in motion doors stop at `MAX_MOTION_SPEED` (10.0). The slider goes as far as the further of the two
// ceilings it feeds, so a player asking for more is not stopped by the channel with the smaller reach, and neither
// door can be handed past its own bound by a hand edited config (`normalize_training_cut_lever`).
pub const MAX_TRAINING_CUT_LEVER: f32 = MAX_MOTION_SPEED;

// The ceiling on the training cut-in's own speed channel, stated rather than inherited from
// `MAX_TIME_SCALE` (the decision A17 asks for before a lever sits on a value that is already above
// the ceiling). Every recorded run read the game putting a number past 5.0 on this door by itself:
// 11.280 in run 11 (`GetTrainingCutTimeScale` answered 5.640 in, 11.280 out), 8.334 in runs 9 and
// 17, 8.280, 8.000, 6.080 and 6.080 in runs 30, 31, 33 and 34. A lever capped at 5.0 would hand the
// game its own value back at every setting on this client, because a scale past the ceiling is a
// pass-through (AGENTS section 5, time scales only go up), and the option would be a slider that
// does nothing.
//
// 12.0 was that ceiling first, sitting just past the highest reading a run had seen on the door. Runs
// 36 and 37 paired the lever against the same build with the lever off and found the ceiling doing the
// multiplying rather than the lever: the game handed 5.680 and 4.800, the lever offered 28.4 and 24.0,
// and the door handed 12.0 both times, a 2.11x and a 2.50x raise. Tag answer cuts came out 18% shorter
// with that (1443 ms to 1180 ms, tap wait taken out) and the plate cascade 19% (1211 ms to 986 ms), so
// the open question is whether the gain scales with the raise, and a ceiling that binds cannot answer
// it. 30.0 is the lever's full reach on the largest scale that pair measured the game handing, 5.680 x
// 5.0 = 28.4, rounded up: the cap now sits past what the measured cuts ask for, so the next run reads
// the lever. What the bound still does is hold the door at max(the game's value, 30.0): a value the
// game put on the door is never pulled down, run 11's 11.280 is raised to 30.0 rather than 56.4, and a
// read-modify-write loop C22 and C24 are about cannot run past 30.0 however many times it re-prices.
pub const MAX_TRAINING_CUT_TIME_SCALE: f32 = 30.0;

// The ceiling `ui_animation_scale` may reach in code, independent of what any slider offers.
// The DOTween `Update` detour multiplies the delta time it hands the tween library once per
// tween tick, so this lever removes animation time the same way a group factor does and gets
// the same bound: MAX_FACTOR. The Config Editor and the first time setup wizard both allow
// 0.1..=1000.0, and the wizard is the only place a new user ever touches the option (C5).
// The floor is the slider's own: config.json is deserialized unbounded, and a hand edited 0
// would freeze every tween the game animates on DOTween's clock.
pub const MAX_UI_ANIMATION_SCALE: f32 = MAX_FACTOR;
pub const MIN_UI_ANIMATION_SCALE: f32 = 0.1;

// The ceiling on the *pair* of layers that reach one tween (C58, ledger item 62).
//
// `ui_animation_scale` multiplies the elapsed time `DG.Tweening.Core.TweenManager::Update` hands every
// tween that runs on the game's clock - the `deltaTime` channel only, never the `independentTime`
// channel a time scale independent tween is advanced by (`DOTween/TweenManager.rs`, C5) - and a group
// factor divides the duration a game method is handed (`scale_duration` below). The two act on the same
// quantity from opposite ends: a tween's real completion time is the duration it was told divided by
// the clock it is measured on, so what the game waits on is shortened by the *product*. Each layer has
// its own ceiling of `MAX_FACTOR` and neither one looked at the other, so a pair that is legal twice
// reached 400x. That is what the runs read: `InitializePlateList 1 -> 0.05` (run 20) and
// `CountupModifier_getDuration 0.16 -> 0.008` (run 17) beside `ui_animation 20` in the same snapshot. Each
// completion is inside the frame of the run that printed it: 2.5 ms on run 20's 16.7 ms frame
// (`target_fps 60`), 0.4 ms on run 17's 5.6 ms frame (`target_fps 200`, 88,728 frames over 499,199 ms) - a
// completion that finishes before the coroutine waiting on it is parked on it. Run 31 ended 26 min 43 s on the
// career end screen (`SceneDefine::ViewId` 1501) with that door the only training duration write the run
// reached, `1 -> 0.05` on its own 16.9 ms frame (28,662 frames over 484,037 ms at `target_fps 60`; the string
// `target_fps 200` is nowhere in that run's log), `cut runs 4 wall 8283 ms timeline 0 ms`, and
// `CutInTimelineController::UpdateSpeed()` growing 3,456 to 46,652 while every door that advances a flow had
// frozen.
//
// The pair therefore gets the same ceiling each layer already has: 20x on one completion, the end of
// the range the C58 concept's plain-Hachimi reading calls fast and still in order (2.4 s of play in
// landing in 120 ms, about 21 frames, and plates 50 ms apart). The duration half backs off, because it
// is the half that knows which group the value belongs to; the clock half is global to every tween in
// the game, including ones this fork never touched, so trimming it there would slow tweens the group
// factor has nothing to do with.
//
// The ceiling speaks about a completion both layers reach, and a door reaches it through its `Pace`, not
// through the group its option happens to sit in. On `TweenMeasured` and `Unproven` the duration half backs
// off to the headroom the clock left, so one completion is shortened by at most this number. On
// `OffTweenClock` the clock layer is not on the completion at all, the pair never composes, and the group's
// own `MAX_FACTOR` - the same 20.0 - is the ceiling that completion needs; trimming it there would take out
// speed the clock never paid for and leave a completion up to 20x slower than the state the runs measured.
// A door therefore leaves the trim only with the line that says its value is not measured on this clock, and
// a door whose channel nothing has read stays bounded: run 17's `CountupModifier_getDuration 0.16 ->
// 0.007999999` beside `ui_animation 20` is 0.4 ms against a 5 ms frame, and it was reached on the one
// `Group::Screens` duration door a career session actually calls.
//
// The clock this ceiling prices is the game's own `Time.deltaTime`, so the `time_scale` lever is under it too,
// and pricing only the duration and the ui lever left a third one free. `DOTweenComponent.Update` hands
// `TweenManager::Update` `Time.deltaTime`, and `Time.deltaTime` is `Time.timeScale` times real elapsed time, so
// a tween on the delta channel is advanced by the ui clock *on top of* whatever Unity is holding in
// `Time.timeScale`: `tDeltaTime = (t.isIndependentUpdate ? independentTime : deltaTime) * t.timeScale`. This
// fork writes that number through `apply_time_scale`, up to `MAX_TIME_SCALE`, and what it writes is the whole
// product, not the factor it added to the game's request: a request of 2.0 under a lever of 5 hands the setter
// 5.0, so `ui_animation 20` under that Unity is 100x on one completion while the factor this fork added is
// 2.5x. Pricing the 2.5x priced 2.5 of a 5 channel and left the completion running 4 ms on a state the ceiling
// printed 8 ms about, so `delta_clock_of` prices the produced scale - the number the channel carries - and
// `ui_clock_of` caps the half this module multiplies at `MAX_TWEEN_SPEED_PRODUCT` over it. The ceiling bounds
// what this fork puts on the channel, and it holds that at every state the writer can reach: a raise is capped
// at `MAX_TIME_SCALE`, so a scale past the ceiling can only be there as a pass-through of a number the game
// asked for (run 19 read the training cut at its own 8.0000 while the write hook logged `1 -> 1`), and there the
// cap floors the ui lever at 1.0 and the fork's share of the channel is nothing. What stays outside the ceiling
// is the `independentTime` channel `f1a6584` took out of the multiply and the story timeline's own time scale,
// which answers to `MAX_TIME_SCALE`. The story half is outside it on a measurement, not an assumption:
// runs 1, 2 and 30 read `StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` in the same sessions that read
// `Time::set_timeScale call N: 1 -> 1`, so raising the number the story timeline steps its clips by raised
// nothing Unity's `Time.deltaTime` carries on this client. Stated plainly because it is the difference between
// a bound and a claim: every `set_timeScale` call line in every run log this ledger holds is `1 -> 1`, so the
// 100x state is reachable in a shipped config and is not a number a run measured.
pub const MAX_TWEEN_SPEED_PRODUCT: f32 = MAX_FACTOR;

// The story choice auto select lever. `StoryChoiceController::CheckChoiceAutoTap` scales the
// increment of `_choiceAutoSelectWaitTime` and `StoryViewController::GetTimeScaleByHighSpeedType`
// scales the story time scale, both from `0.75 / delay` (C24 measured both sites multiplying by
// 7500 at the slider's 0.0001 floor). One clamp and one mirror serve both, and each half is capped
// by the quantity it touches. The wait time half multiplies an accumulator measured in seconds, so
// it is a duration scale and the ceiling is MAX_FACTOR. The getter half hands back the scale the
// story timeline steps its clips by, so the ceiling is MAX_TIME_SCALE: one ceiling for both is what
// put a 7.5 story time scale on the game at the Config Editor's left end, and 20 with a hand edited
// config, against the 5.0 every layer that reaches that scale is required to enforce.
// The floor is the one every other time lever uses: a Config Editor slider clamps to its range and
// then snaps from `range.start` (egui 0.33.3 `Slider::set_value`), so its left end lands exactly on
// this value, and config.json is deserialized unbounded.
pub const MIN_STORY_CHOICE_AUTO_SELECT_DELAY: f32 = 0.1;
pub const MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER: f32 = MAX_FACTOR;
pub const MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE: f32 = MAX_TIME_SCALE;

// What the client compares `_choiceAutoSelectWaitTime` against before it auto taps a story
// choice. C24: this is the shipped constant the hook pair already assumes, not a value read out
// of the live client. Public because the only `story_choice_auto_select_delay` that leaves both sites
// inert is this number divided by itself (core::speed_preset).
pub const CHOICE_AUTO_SELECT_TRIGGER_TIME: f32 = 0.75;

/// `CHOICE_AUTO_SELECT_TRIGGER_TIME / delay` as the wait time site is allowed to apply it: the
/// delay has a hard floor and the multiplier a hard ceiling, independent of what any slider offers or
/// what a hand edited config holds. A delay slower than the trigger still shortens nothing, so the
/// multiplier is only capped from above. `None` means "leave the game's value alone", which is what a
/// non finite or non positive delay already means today. The story time scale half caps this lower,
/// at `MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE`.
pub fn story_choice_auto_select_multiplier(delay: f32) -> Option<f32> {
    if !delay.is_finite() || delay <= 0.0 {
        return None;
    }

    let delay = delay.max(MIN_STORY_CHOICE_AUTO_SELECT_DELAY);

    Some((CHOICE_AUTO_SELECT_TRIGGER_TIME / delay).min(MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER))
}

/// The multiplier as both story choice sites read it, clamped once per config pass instead of
/// divided per call. `NAN` means "leave the game's value alone".
fn normalize_story_choice_auto_select_multiplier(delay: f32) -> f32 {
    story_choice_auto_select_multiplier(delay).unwrap_or(f32::NAN)
}

/// The wait time half, read from the mirror: the number `CheckChoiceAutoTap` scales the increment
/// of `_choiceAutoSelectWaitTime` by. `NAN` is the inert setting.
pub fn story_choice_wait_time_multiplier() -> f32 {
    f32::from_bits(STORY_CHOICE_AUTO_SELECT_MULT.load(Ordering::Acquire))
}

// The mirror value as the getter half may use it. Below 1.0 it is inert on this path: the wait
// time scaling is what makes a slower delay slower, and putting the story clock under the speed the
// game chose to run at is not what this option asks for (C12, C35).
fn story_choice_time_scale_factor(mult: f32) -> f32 {
    mult.max(1.0).min(MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE)
}

/// The getter half: what `GetTimeScaleByHighSpeedType` hands the story timeline. Kept apart from
/// the mirror so the bound is testable the way `plan_write` is. The raise goes through
/// `scale_read_time_scale`, the one arithmetic every time scale layer shares, so a stored pause or
/// slow motion is left as the game stored it, a scale the game already holds above the ceiling
/// passes through instead of being pulled down, and the product is capped at MAX_TIME_SCALE.
pub fn story_choice_time_scale_from(value: f32, mult: f32) -> f32 {
    if mult.is_nan() {
        return value;
    }

    scale_read_time_scale(value, story_choice_time_scale_factor(mult))
}

/// The same, read from the mirror.
pub fn story_choice_time_scale(value: f32) -> f32 {
    story_choice_time_scale_from(value, story_choice_wait_time_multiplier())
}

pub fn story_factor() -> f32 {
    f32::from_bits(STORY_FACTOR.load(Ordering::Acquire))
}

/// The configured `time_scale` lever, already clamped to the bounds above. Read by the
/// Unity write hook once per call instead of loading the config.
pub fn time_scale() -> f32 {
    f32::from_bits(TIME_SCALE.load(Ordering::Acquire))
}

/// The configured `ui_animation_scale`, already clamped to the bounds above. Read by the
/// DOTween `Update` detour, which runs on every tween tick, instead of loading the config.
pub fn ui_animation_scale() -> f32 {
    f32::from_bits(UI_ANIMATION_SCALE.load(Ordering::Acquire))
}

/// The factor `apply_time_scale` added to the value it was handed: what this fork put on top of the game's
/// own clock. A value at or below 1.0 is the game's pause, its slow motion or its neutral speed and comes
/// back unchanged, so the raise is 1.0 and the lever reached nothing. A value the game already held above
/// `MAX_TIME_SCALE` passes through untouched, so a cut running at its own 8.0000 is not this fork's
/// speed-up. The name of the number the clock carries is `time_scale_produced`; this one is what the log
/// line says about the fork's own share.
pub fn time_scale_raise_of(value: f32, lever: f32) -> f32 {
    if !value.is_finite() || !lever.is_finite() || value <= 0.0 {
        return 1.0;
    }

    (apply_time_scale(value, lever) / value).max(1.0).min(MAX_TIME_SCALE)
}

/// The raise as the log reads it. A mirror that is not a number, which `note_time_scale_write` never stores,
/// is read as the neutral 1.0 the way every other mirror here is read.
pub fn time_scale_raise() -> f32 {
    let raise = f32::from_bits(TIME_SCALE_RAISE.load(Ordering::Acquire));

    if !raise.is_finite() { 1.0 } else { raise.max(1.0) }
}

/// The number the `Time.timeScale` write layer last handed Unity's setter, floored at the neutral 1.0: a
/// pause or a slow motion shortens no completion, so it asks the pair ceiling for no headroom. This is the
/// scale `Time.deltaTime` carries, so it is the number a completion on the delta channel is divided by, and
/// the number `ui_animation_scale` has to leave room under. A scale the game reached on its own above
/// `MAX_TIME_SCALE` is in it unchanged: the ceiling prices the channel the completion ran on, and a scale the
/// game is holding is not this fork's speed-up but is still the speed the completion ran at.
pub fn time_scale_produced() -> f32 {
    let produced = f32::from_bits(TIME_SCALE_PRODUCED.load(Ordering::Acquire));

    if !produced.is_finite() { 1.0 } else { produced.max(1.0) }
}

/// Record both halves of a produced write: `value` is what the game asked for, `produced` is the number the
/// write layer handed the setter. A write that changed nothing records 1.0 on the raise, which is what clears
/// a raise an earlier config left behind when the lever goes back to neutral. `Time.rs` calls it at its two
/// produced writes and never on the echoed one, where the game is handing this layer its own product back and
/// the number the clock is carrying is the one already recorded.
///
/// Returns whether the pair ceiling line is owed for the clock this write left behind, so a scale that
/// appears mid-session reaches `hachimi.log` in the same sentence a config change does. The latch inside
/// `note_pair_ceiling` is keyed on the composed clock and the ui cap it leaves, so one line per state the
/// levers actually settle to, and a produced write that changes neither changes no line. Nothing here reaches
/// a config load or a game pointer: `mirrored_factors` reads the same atomics every scaling detour reads.
pub fn note_time_scale_write(value: f32, produced: f32) -> bool {
    let raise = if value.is_finite() && produced.is_finite() && value > 0.0 {
        (produced / value).max(1.0).min(MAX_TIME_SCALE)
    } else {
        1.0
    };

    let scale = if produced.is_finite() { produced.max(1.0) } else { 1.0 };

    TIME_SCALE_RAISE.store(raise.to_bits(), Ordering::Release);
    TIME_SCALE_PRODUCED.store(scale.to_bits(), Ordering::Release);

    // The three groups a factor writes; `mirrored_factors`' fourth slot is the training group's pinned 1.0,
    // which is not a lever and has no headroom to name on the line.
    let factors = mirrored_factors();

    note_pair_ceiling(ui_animation_scale(), [factors[0], factors[1], factors[2]])
}

/// The `Time.timeScale` the delta channel carries, floored at the neutral 1.0. A pause or a slow motion is
/// not a speed-up, so it asks for no headroom; a number the game holds above the pair ceiling is left what
/// it is, because the bound is about what this fork may put on a completion and never about pulling a game
/// scale down (AGENTS section 5: an option must not slow the game).
fn carried_time_scale(produced: f32) -> f32 {
    if produced.is_finite() { produced.max(1.0) } else { 1.0 }
}

/// The multiplier `ui_animation_scale` is allowed to put on the delta channel once the scale this fork's
/// write layer left in `Time.timeScale` is counted against `MAX_TWEEN_SPEED_PRODUCT`. The scale is inside
/// this lever's headroom because the lever multiplies `Time.deltaTime`, and `Time.deltaTime` already carries
/// `Time.timeScale`: at a Unity holding 5.0 a 20x ui clock runs 4x, so the channel is 20x and not 100x. Below
/// the cap the lever is exactly what the config says, and the cap only ever lowers it, so the slider's 0.1
/// floor is not turned into a faster clock. Its own floor is 1.0: trimming the lever below that would make
/// this fork the reason a game animation ran slower than the game asked, which a ceiling never does. A mirror
/// that is not a number is the neutral 1.0.
pub fn ui_clock_of(ui: f32, produced: f32) -> f32 {
    let scale = carried_time_scale(produced);

    if !ui.is_finite() {
        return 1.0;
    }

    ui.max(0.0).min((MAX_TWEEN_SPEED_PRODUCT / scale).max(1.0))
}

/// The multiplier the delta channel carries: the ui clock above, times the whole scale under it. Under a scale
/// at or below the pair ceiling - which is every scale this module's own ceilings let it write, because a raise
/// stops at `MAX_TIME_SCALE` - the number is at most `MAX_TWEEN_SPEED_PRODUCT`. Past the ceiling it is the
/// game's own scale unchanged: the cap floors at 1.0, and a scale that high can only have arrived as a
/// pass-through, where this fork added nothing. That is why the duration headroom below can be priced against
/// this number instead of against one lever.
pub fn delta_clock_of(ui: f32, produced: f32) -> f32 {
    ui_clock_of(ui, produced) * carried_time_scale(produced)
}

/// The same, read from the mirrors. This is the number a completion on the delta channel is shortened by
/// before any duration door is touched, and `MAX_TWEEN_SPEED_PRODUCT` is a ceiling on what this fork's
/// levers put there (C58, item 62).
pub fn delta_clock() -> f32 { delta_clock_of(ui_animation_scale(), time_scale_produced()) }

/// The two elapsed time channels of one DOTween tick, exactly as `DOTween/TweenManager.rs` hands them
/// to the game: the `deltaTime` half multiplied by the clamped mirror, capped by what the pair ceiling
/// leaves once the scale this fork's write layer left in `Time.timeScale` is counted, and the
/// `independentTime` half passed on as the caller computed it (C5, the half this line used to scale as well).
///
/// The lever is a clock on the delta channel only because DOTween reads the other one per tween.
/// `DG.Tweening.Core.TweenManager.Update(tween, deltaTime, independentTime, ..)` advances a tween by
/// `float tDeltaTime = (t.isIndependentUpdate ? independentTime : deltaTime) * t.timeScale;`, and
/// `DOTweenComponent.Update` computes the `independentTime` argument from `Time.unscaledDeltaTime`. A tween
/// the game marked time scale independent (`SetUpdate(true)`, `DOTween.defaultTimeScaleIndependent`) is
/// therefore measured on wall clock on purpose, and that is how UI keeps animating while `Time.timeScale` is
/// 0 - a paused race, a story wait. Multiplying it took wall clock out of a tween the game had deliberately
/// taken out of every time lever this fork touches: at `MAX_UI_ANIMATION_SCALE` a 1 s real time UI tween
/// finished in 50 ms, pause included. The other argument is `Time.deltaTime`, which is `Time.timeScale` times
/// real elapsed time, so the multiply below sits on top of the scale the `Time.timeScale` write layer left in
/// the game rather than beside it (C58, item 62). A mirror that is not a number - which this module never
/// stores - is read as the neutral 1.0, the way `duration_factor` reads it.
pub fn tween_clocks(delta_time: f32, independent_time: f32) -> (f32, f32) {
    let scale = ui_clock_of(ui_animation_scale(), time_scale_produced());

    // Fast exit at the neutral setting: no work on a tween tick that nothing speeds up.
    if scale == 1.0 || !scale.is_finite() {
        return (delta_time, independent_time);
    }

    (delta_time * scale, independent_time)
}

// The factors are cached so a detour that runs per call reads one atomic instead of
// loading the whole config.
pub fn factor(group: Group) -> f32 {
    let bits = match group {
        Group::Transition => TRANSITION_FACTOR.load(Ordering::Relaxed),
        Group::Screens => SCREENS_FACTOR.load(Ordering::Relaxed),
        Group::Story => STORY_FACTOR.load(Ordering::Relaxed),
        // The training group has no lever behind it: not a constant this module is allowed to raise, and
        // not a mirror any config can write. It is the answer `Group::Training` exists to carry - the
        // turn's gates run at the length the game gave them (C58). A door that reads this gets the value
        // it received, which `scale_duration` then hands back untouched at its `factor == 1.0` bail.
        Group::Training => return 1.0,
    };

    f32::from_bits(bits)
}

/// The plate cascade lever as the config pass left it. Read by one door, on one screen, so it is a plain atomic
/// read on a call the training census counts a handful of times per turn rather than a group factor read on every
/// duration detour.
pub fn plate_factor() -> f32 { f32::from_bits(PLATE_FACTOR.load(Ordering::Relaxed)) }

/// The training cut-in lever as the config pass left it. One door reads it, on the training screen, so it is a
/// plain atomic load on a call the run logs at up to a few hundred times a session rather than a per frame read:
/// run 11 counted 220 calls of this door across a whole career, and the per frame door beside it
/// (`CutInTimelineController::UpdateSpeed`) stays untouched.
fn training_cut_factor() -> f32 { f32::from_bits(TRAINING_CUT_FACTOR.load(Ordering::Relaxed)) }
fn tag_cut_factor() -> f32 { f32::from_bits(TAG_CUT_FACTOR.load(Ordering::Relaxed)) }

// Most of Gallop's durations are `const`, which IL2CPP folds into the call sites, so
// the value has no writable storage (see the init log). The methods that play the
// animation still receive it as a plain argument, so scaling the argument is the same
// adjustment applied one level earlier, and it does not care how the caller stores it.
//
// The bounded default: a door that does not name a pace is trimmed by the pair ceiling, the same way every
// duration door this fork arms is (C58, ledger item 62). Stepping out of the trim is a named act -
// `scale_paced` with a `Pace::OffTweenClock` that carries the line saying the completion is not measured on
// this clock - and never something a door falls into by omission.
pub fn scale_duration(value: f32, group: Group) -> f32 {
    scale_paced(value, group, Pace::Unproven)
}

/// The value a door hands the game, on the pace that door stands on.
pub fn scale_paced(value: f32, group: Group, pace: Pace) -> f32 {
    let factor = duration_factor_at(group, pace);

    if factor == 1.0 || !value.is_finite() || value == 0.0 {
        return value;
    }

    // Dividing keeps the sign, so a negative offset moves towards zero instead of
    // flipping into a duration that did not exist before.
    value / factor
}

/// The headroom a door has left under `MAX_TWEEN_SPEED_PRODUCT`, on its pace.
///
/// On the two bounded lanes the number the duration is priced against is `delta_clock()`: the
/// `ui_animation_scale` the DOTween `Update` detour multiplies the tween clock by, capped by what is left
/// once the scale the `Time.timeScale` write layer left in the game is counted, times that scale. The shorter
/// this fork has already made every tween that runs on that clock, the less a group factor may take out of the
/// duration handed to the game. At both levers neutral the headroom is `MAX_FACTOR` and the door scales exactly
/// as it did; at `ui_animation 20` with Unity holding 1.0, or `ui_animation 20` beside a Unity holding the 5.0
/// this fork's lever wrote, nothing is left and the door hands on the number it received. A clock below 1.0
/// (the slider's 0.1 floor slows the tween clock) leaves the group's own `MAX_FACTOR` ceiling in force, and a
/// mirror that is not a number - which this module never stores - is read as the neutral 1.0.
///
/// On `OffTweenClock` the clock layer is not on the completion, so there is no pair to price: the door gets
/// its group's whole factor, which `normalize` already holds at `MAX_FACTOR`, and the ceiling is satisfied
/// by that factor alone. This is the half that stops the bound being a slowdown: a value the clock never
/// advances is not trimmed by it, whatever the clock is set to.
///
/// Neither lane ever returns more than the group's own factor, and neither goes under 1.0: this is a ceiling
/// on a speed-up, never a lever that slows the game (AGENTS section 5). The `1.0` floor is what holds that
/// promise when Unity is holding a scale past the ceiling: the channel is already faster than the bound, the
/// bound has no headroom left to give, and the door hands the game its own duration.
pub fn duration_factor_at(group: Group, pace: Pace) -> f32 {
    let factor = factor(group);

    // A completion the multiplied clock does not measure is not a pair, and the trim on it is a slowdown of
    // a lever the clock never touched. The group's own cap is its ceiling.
    if !pace.bounds_the_pair() {
        return factor;
    }

    let clock = delta_clock();

    // Fast path, and the shipped state: with both clock levers neutral this is the group's own factor,
    // no division, and `MAX_TWEEN_SPEED_PRODUCT / clock` would be a wider bound than the group already has.
    if clock <= 1.0 || !clock.is_finite() {
        return factor;
    }

    factor.min((MAX_TWEEN_SPEED_PRODUCT / clock).max(1.0))
}

/// The headroom of a door that has not said what it paces. Every armed duration door reads the trim through
/// this, or through `scale_paced` on a bounded pace (C58, ledger item 62).
pub fn duration_factor(group: Group) -> f32 { duration_factor_at(group, Pace::Unproven) }

/// The speed a completion runs at under the bound: the factor the duration is shortened by, times the factor
/// the clock it is measured on is sped up by. `MAX_TWEEN_SPEED_PRODUCT` is a statement about this number,
/// not about either lever, and a test that only reads the levers cannot see the defect it bounds (C58).
///
/// The clock here is `delta_clock()`, not `ui_animation_scale` alone: the argument the tween library
/// multiplies is `Time.deltaTime`, which is `Time.timeScale` times real elapsed time, and `Time.timeScale`
/// holds the whole number the `time_scale` write layer handed the setter. Pricing only the ui half left that
/// channel unpriced, and pricing only the factor the layer added to a game write left the rest of it unpriced:
/// a 20x ui lever under a Unity holding 5.0 is 100x on the completion, and a game write of 2.0 raised to 5.0
/// was priced as 2.5x of it.
///
/// On a door whose completion the clock layer never reaches, the clock is not in the number: the group factor
/// is the whole speed that completion runs at, and the same ceiling binds it there.
///
/// What the ceiling holds in every state is this fork's share of the channel: `ui_clock_of` times the factor
/// the write layer added, which cannot pass `MAX_TWEEN_SPEED_PRODUCT` because the cap is `MAX_TWEEN_SPEED_PRODUCT`
/// over the whole scale under the lever. The number below equals it whenever Unity is holding a scale at or
/// below the ceiling, which is every scale any layer of this module is allowed to write: a pass-through of a
/// scale past 20x is the game's own speed, and holding it down would be this fork slowing the game.
pub fn completion_speed(group: Group, pace: Pace) -> f32 {
    let factor = duration_factor_at(group, pace);

    if !pace.bounds_the_pair() {
        return factor;
    }

    factor * delta_clock()
}

/// The composed speed of a door that has not said what it paces.
pub fn tween_speed(group: Group) -> f32 { completion_speed(group, Pace::Unproven) }

// The floors a completion measurement has to clear before `measured_pace` believes it. A completion under 20 ms
// is one to four frames at the frame budgets this fork measures at: run 17's clock read 5.6 ms a frame
// (`target_fps 200`, 88,728 frames over 499,199 ms) and run 31's read 16.9 ms (`target_fps 60`, 28,662 frames
// over 484,037 ms). A completion shorter than the floor cannot say which of them it was measured on, and a
// clock closer to neutral than 4x cannot separate "the duration the door handed" from "that duration over 20".
// Both say when a measurement says nothing.
const MEASUREMENT_MIN_WALL_SEC: f32 = 0.02;
const MEASUREMENT_OFF_CLOCK_MAX_RATIO: f32 = 1.5;
pub const MEASUREMENT_MIN_CLOCK: f32 = 4.0;

// How far the wall time may run past the duration it was handed before the reading stops being a clean one.
// Off the multiplied clock a completion closes at about the length it was handed; a wall time several times
// that length is a start delay ahead of the animation, or a yield that is one step of a longer chain, and
// neither says anything about which clock the door's own duration is measured on. Without this ceiling a 6 s
// delay in front of a 0.16 s count-up at `ui_animation 20` closes 308 ms later and answers `OffTweenClock`,
// which is the 20x speed-up handed to a completion the run has not shown the clock is absent from. The band
// is generous in the other direction on purpose: a coroutine polled on the fixed update clock (run 21 read one
// in all five training cuts) closes a 0.16 s yield a frame or two late.
const MEASUREMENT_OFF_CLOCK_MIN_RATIO: f32 = 0.5;

/// What a run's measurement of one completion says about its pace, which is the only thing that moves a door
/// between the bounded lanes and `OffTweenClock` (C58, ledger item 62). `handed` is the duration the door's own
/// hit line printed it gave the game, `wall` is how long that completion actually took, and `clock` is
/// `delta_clock()`: the multiplier the delta channel carried in the same config snapshot, the ui cap over the
/// whole scale the `Time.timeScale` write layer left in the game. A completion running on the game's own clock
/// on top of that reads a ratio higher than the clock asked for, and a higher ratio still answers
/// `TweenMeasured`.
///
/// A completion that ran at the duration it was handed was measured on a channel the clock layer does not
/// reach. A completion that ran at about the speed that clock was set to was measured on the channel it
/// multiplies. Anything else - a completion inside one frame, a completion far longer than the duration it was
/// handed, a clock near neutral, a wall time part of the way between the two candidates - says the door is not
/// cleanly on one channel, and a door that is not cleanly on one channel stays bounded: the answer this
/// function withholds is the answer a door keeps its bound on.
pub fn measured_pace(handed: f32, wall: f32, clock: f32) -> Option<Pace> {
    if !handed.is_finite() || !wall.is_finite() || !clock.is_finite() {
        return None;
    }

    if handed <= 0.0 || wall < MEASUREMENT_MIN_WALL_SEC || clock < MEASUREMENT_MIN_CLOCK {
        return None;
    }

    let ratio = handed / wall;

    if ratio <= MEASUREMENT_OFF_CLOCK_MAX_RATIO {
        if ratio < MEASUREMENT_OFF_CLOCK_MIN_RATIO {
            return None;
        }

        return Some(Pace::OffTweenClock("measured: the completion ran at the duration the door handed it, on a channel this fork's clock levers did not reach"));
    }

    if ratio >= clock * 0.5 {
        return Some(Pace::TweenMeasured("measured: the completion ran on the delta channel this fork's clock levers multiply"));
    }

    None
}

// A frame count never drops below one frame: the coroutines that advance animation
// poll these counts, and a zero wait can leave them spinning without advancing.
pub fn scale_frame_count(value: i32, group: Group) -> i32 {
    let factor = factor(group);

    if factor == 1.0 || value == 0 {
        return value;
    }

    let scaled = value as f32 / factor;

    if value > 0 { scaled.max(1.0).round() as i32 }
    else { scaled.min(-1.0).round() as i32 }
}

// The one arithmetic every time-scale layer shares, so no two of them can multiply the
// same quantity past a ceiling nobody is enforcing.
//
// This half is for a value on its way into `Time.timeScale`. A value at or below 1.0 is left
// exactly as it is: 0 is how the game pauses, under 1 is how it does slow motion, and 1.0 is
// the speed the game chose to run at. Multiplying any of those by a speed-up overwrites that
// choice (C12). Only a value already above 1.0 is the game's own fast forward, and the result
// is bounded by MAX_TIME_SCALE, so a raise the mod itself makes stops at the ceiling and never
// compounds. It never falls below the value it started from either, so a scale the game already
// holds above the ceiling passes through instead of being pulled down: an option must never slow
// the game, the same floor the read half states (AGENTS section 5).
//
// Public because `UnityEngine_CoreModule::Time` decides its one config write with this same
// arithmetic, so the value it puts in the game and the values it lets the game put in are
// bounded the same way. A getter reads a scale instead of writing one, and uses
// `scale_read_time_scale` (C39).
//
// The factor is a speed up, so one at or below 1.0 is inert: multiplying a value above 1.0 by a
// sub 1 lever would slow the game's own fast forward down, the same inversion C35 exists to
// refuse (C40).
pub fn apply_time_scale(value: f32, factor: f32) -> f32 {
    if factor <= 1.0 || !value.is_finite() || value <= 1.0 {
        return value;
    }

    (value * factor).min(MAX_TIME_SCALE).max(value)
}

// The read half, for a getter that hands back the scale the game then steps its own clips by.
// Only a value under 1.0 is protected, because that is a pause or a slow motion the game
// stored. The game's 1.0 is its neutral playback speed, which is what a speed-up option exists
// to raise, and it is the value the three installed Story getters measured live
// (`StoryTimeline_getTimeScaleAfterEndStory 1 -> 5` in runs 1, 2 and 3), so leaving 1.0 alone
// made `story_speed` change nothing on the story path (C39). The same ceiling binds, and a
// scale the game already holds above MAX_TIME_SCALE passes through instead of being pulled
// down: an option must never slow the game. A factor at or below 1.0 is inert for the same
// reason it is on the write half (C40).
pub fn scale_read_time_scale(value: f32, factor: f32) -> f32 {
    if factor <= 1.0 || !value.is_finite() || value < 1.0 || value > MAX_TIME_SCALE {
        return value;
    }

    (value * factor).min(MAX_TIME_SCALE)
}

// A getter's time scale only ever goes up, and only to MAX_TIME_SCALE. It scales on the read
// half, so `story_speed` reaches the story path (C39).
pub fn scale_time_scale(value: f32, group: Group) -> f32 {
    scale_read_time_scale(value, factor(group))
}

// The Unity write layer: a value on its way into `Time.timeScale`, raised by the
// configured lever and bounded by the same ceiling `scale_time_scale` uses. A story
// getter that already handed out MAX_TIME_SCALE therefore passes through this hook
// unchanged instead of being multiplied again.
pub fn scale_game_time_scale(value: f32) -> f32 {
    apply_time_scale(value, time_scale())
}

// A detour installed against the wrong overload reads its arguments from the wrong
// registers, so the parameter list and the return type both have to match the declared
// method before anything is hooked. How permissive that match may be depends on how the
// wrapper declares itself: a wrapper that reserves a register for `this` cannot take a static
// target, a wrapper without one must have it, and a wrapper that declares value parameters
// cannot take a method whose parameters are references, because those travel as addresses.
#[derive(Clone, Copy)]
struct MethodMatch {
    allow_static: bool,
    require_static: bool,
    require_ref: bool,
    forbid_ref: bool,
    // Every parameter and the result has to be a value this wrapper can hold in the position it
    // declared. A reference type in that position would be handed the number the wrapper wrote.
    values_only: bool,
    // Set only by a wrapper that declares a pointer where the dump spells a generic instantiation,
    // which is how `List<SupportCardData>` becomes resolvable at all (C48). No scaling wrapper sets
    // it: they resolve against signatures that already matched exactly.
    generic_slots: bool,
}

// A 4 byte enum travels in a general purpose register (A5). A value type bigger than this is
// moved through memory, and the `i32` a wrapper declared in front of it then reads the first
// field of a struct that was never in the register at all.
const MAX_INLINE_VALUE_BYTES: u32 = 4;

// `il2cpp_class_instance_size` counts the object header, so subtracting it leaves the payload,
// which is the number the ABI moves and the number `introspect.rs` prints as `struct<Name:N B>`.
const OBJECT_HEADER_BYTES: u32 = 16;

// A wrapper with a `this` parameter and plain value arguments.
const MATCH_INSTANCE_VALUE: MethodMatch = MethodMatch { allow_static: false, require_static: false, require_ref: false, forbid_ref: true, values_only: false, generic_slots: false };
// A zero argument getter: a static one simply ignores the `this` register it is handed.
const MATCH_GETTER_EITHER: MethodMatch = MethodMatch { allow_static: true, require_static: false, require_ref: false, forbid_ref: true, values_only: false, generic_slots: false };
// A wrapper that writes through reference parameters.
const MATCH_REFERENCE: MethodMatch = MethodMatch { allow_static: false, require_static: false, require_ref: true, forbid_ref: false, values_only: false, generic_slots: false };
// A wrapper that declares only the real arguments, so the target has to be static.
const MATCH_STATIC_VALUE: MethodMatch = MethodMatch { allow_static: true, require_static: true, require_ref: false, forbid_ref: true, values_only: false, generic_slots: false };
// A static wrapper whose arguments and result are all values sized like the ones it declares,
// which is the shape of a 4 byte enum setting. A reference candidate is refused by name instead
// of bound: the enum value would land in the register the target expects a pointer in.
const MATCH_STATIC_VALUES_ONLY: MethodMatch = MethodMatch { allow_static: true, require_static: true, require_ref: false, forbid_ref: true, values_only: true, generic_slots: false };
// A wrapper that declares a pointer where the dump spells a generic instantiation. One of a class is
// a managed object and the ABI moves it as an address, so a pointer is what belongs in the slot, and
// what is unknown is the element type. Nothing that reads or writes an argument may resolve this way;
// it exists so an observe only probe can reach `List<...>` signatures at all (C48).
const MATCH_GENERIC_REF: MethodMatch = MethodMatch { allow_static: false, require_static: false, require_ref: false, forbid_ref: true, values_only: false, generic_slots: true };
const MATCH_STATIC_GENERIC_REF: MethodMatch = MethodMatch { allow_static: true, require_static: true, require_ref: false, forbid_ref: true, values_only: false, generic_slots: true };

pub unsafe fn resolve_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, MATCH_INSTANCE_VALUE)
}

// A method that reports its result through `ref` parameters hands back pointers, and this
// client keeps the element type in the enum: the dumped signature
// `GetNextFrameCount_HighSpeed/2 -> void(float&, int&)` still reports R4 and I4, with the
// reference marked by a separate bit on Il2CppType. A wrapper that writes through those
// parameters is only safe once the bit has been confirmed on the resolved overload.
pub unsafe fn resolve_ref_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, MATCH_REFERENCE)
}

// A static method has no hidden `this`, so its wrapper declares only the dumped arguments.
// The dump spells a class typed parameter as `class<...>`, which is the CLASS enum.
pub unsafe fn resolve_static_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, MATCH_STATIC_VALUE)
}

// The same, for a static whose arguments and result are values the wrapper declares directly:
// `StoryTimelineController::SetHighSpeedType/1` and `IsHighSpeedMode/1`, dumped as
// `struct<StoryTimelineController.HighSpeedType:4B>`, behind wrappers that declare an `i32`.
// The value proof is mandatory here, and a `class<...>` candidate is refused rather than bound.
pub unsafe fn resolve_static_value_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, MATCH_STATIC_VALUES_ONLY)
}

// A dumped `generic<System.Collections.Generic.List<...>>` parameter reports `GENERICINST` in its
// parameter record, never `CLASS`, so every exact walk this file has run simply reported the method as
// missing (C48). These two resolvers are for a wrapper that declares a pointer in that slot, and the
// exact overload walk still runs first, so a signature that resolved before binds the same method it
// always did. No scaling hook uses them: a wrapper that reads or writes an argument cannot be written
// against a type whose element layout is unknown here.
pub unsafe fn resolve_generic_ref_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, MATCH_GENERIC_REF)
}

pub unsafe fn resolve_static_generic_ref_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, MATCH_STATIC_GENERIC_REF)
}

// The word `introspect.rs` prints for one spelling of a type, so an install line can name the
// candidate it bound to instead of printing a bare flag. `struct` and `class` are two different
// installs, and a run has to be able to tell them apart.
pub const fn type_word(kind: Il2CppTypeEnum) -> &'static str {
    match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_VOID => "void",
        Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN => "bool",
        Il2CppTypeEnum_IL2CPP_TYPE_I4 => "int",
        Il2CppTypeEnum_IL2CPP_TYPE_U4 => "uint",
        Il2CppTypeEnum_IL2CPP_TYPE_I8 => "long",
        Il2CppTypeEnum_IL2CPP_TYPE_U8 => "ulong",
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => "float",
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => "double",
        Il2CppTypeEnum_IL2CPP_TYPE_STRING => "string",
        Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE => "struct",
        Il2CppTypeEnum_IL2CPP_TYPE_ENUM => "enum",
        Il2CppTypeEnum_IL2CPP_TYPE_CLASS => "class",
        Il2CppTypeEnum_IL2CPP_TYPE_OBJECT => "object",
        Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST => "generic",
        _ => "other",
    }
}

// The install-line word for a candidate that resolved, or for one that never did.
pub const fn candidate_word(matched: Option<Il2CppTypeEnum>) -> &'static str {
    match matched {
        Some(kind) => type_word(kind),
        None => "unresolved",
    }
}

// How the ABI moves a parameter or a result, measured from the class behind the dumped type
// instead of trusted from the spelling the caller passed. `get_method_overload` compares the
// `Il2CppTypeEnum` alone, so `VALUETYPE` accepted a 24 byte struct as readily as a 4 byte enum,
// and `CLASS` accepted any reference a caller wanted to hold as a value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ValueShape {
    // A value the wrapper may hold where it declared one.
    Inline,
    // A pointer. A value only for a wrapper that declared a pointer.
    Reference,
    // A value type too large for a register, or a type this client has no wrapper shape for.
    Unproven,
}

// The decision, kept free of il2cpp so it is testable the way `Time.rs`'s `plan_write` and
// `HighSpeedSetting`'s `plan_pass` are: given the dumped type and the payload size measured from
// the class behind it (`None` when that class could not be read), what may a wrapper hold there?
fn value_shape_for(kind: Il2CppTypeEnum, payload: Option<u32>) -> (ValueShape, u32) {
    match kind {
        // The dump spells these `struct<Name:N B>`. Measuring the class is what separates the
        // 4 byte enum that travels in a general register (A5) from a struct that does not, and a
        // type whose class cannot be read is a type nothing here can prove anything about.
        Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE | Il2CppTypeEnum_IL2CPP_TYPE_ENUM => match payload {
            Some(size) if size <= MAX_INLINE_VALUE_BYTES => (ValueShape::Inline, size),
            Some(size) => (ValueShape::Unproven, size),
            None => (ValueShape::Unproven, 0),
        },

        // Every reference type travels as an address.
        Il2CppTypeEnum_IL2CPP_TYPE_CLASS
        | Il2CppTypeEnum_IL2CPP_TYPE_OBJECT
        | Il2CppTypeEnum_IL2CPP_TYPE_STRING
        | Il2CppTypeEnum_IL2CPP_TYPE_ARRAY
        | Il2CppTypeEnum_IL2CPP_TYPE_SZARRAY
        | Il2CppTypeEnum_IL2CPP_TYPE_PTR => (ValueShape::Reference, 0),

        // `void`, `bool`, the integral types and the float types are already pinned by the enum
        // comparison `get_method_overload` and the return type check make.
        Il2CppTypeEnum_IL2CPP_TYPE_VOID
        | Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN
        | Il2CppTypeEnum_IL2CPP_TYPE_CHAR
        | Il2CppTypeEnum_IL2CPP_TYPE_I1
        | Il2CppTypeEnum_IL2CPP_TYPE_U1
        | Il2CppTypeEnum_IL2CPP_TYPE_I2
        | Il2CppTypeEnum_IL2CPP_TYPE_U2
        | Il2CppTypeEnum_IL2CPP_TYPE_I4
        | Il2CppTypeEnum_IL2CPP_TYPE_U4
        | Il2CppTypeEnum_IL2CPP_TYPE_I8
        | Il2CppTypeEnum_IL2CPP_TYPE_U8
        | Il2CppTypeEnum_IL2CPP_TYPE_I
        | Il2CppTypeEnum_IL2CPP_TYPE_U
        | Il2CppTypeEnum_IL2CPP_TYPE_R4
        | Il2CppTypeEnum_IL2CPP_TYPE_R8 => (ValueShape::Inline, 0),

        // A generic instantiation, a type variable, a function pointer, a typed reference: no
        // wrapper this client writes can claim to know how one moves.
        _ => (ValueShape::Unproven, 0),
    }
}

// Whether a shape may sit behind a wrapper. `values_only` is the flag the value-shaped wrappers
// carry: in front of one, a reference receives the number the wrapper declared.
fn shape_is_allowed(shape: ValueShape, values_only: bool) -> bool {
    match shape {
        ValueShape::Inline => true,
        ValueShape::Reference => !values_only,
        ValueShape::Unproven => false,
    }
}

// Reads the payload size out of the metadata and hands it to the decision above.
unsafe fn measure_value_shape(type_: *const Il2CppType) -> (ValueShape, u32) {
    let kind = (*type_).type_();

    let payload = match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE | Il2CppTypeEnum_IL2CPP_TYPE_ENUM => {
            let class = il2cpp_type_get_class_or_element_class(type_);

            if class.is_null() {
                None
            }
            else {
                Some((il2cpp_class_instance_size(class) as u32).saturating_sub(OBJECT_HEADER_BYTES))
            }
        },
        _ => None,
    };

    value_shape_for(kind, payload)
}

unsafe fn type_name(type_: *const Il2CppType) -> String {
    let name = il2cpp_type_get_name(type_);

    if name.is_null() {
        return "?".to_owned();
    }

    CStr::from_ptr(name).to_string_lossy().into_owned()
}

// The value proof, applied to a parameter (`Some(index)`) or to the result (`None`): resolve the
// class behind the dumped type, measure what the ABI moves, and refuse what the wrapper cannot
// hold. Both the shape that was accepted and the one that was refused are printed, so a run can
// read which spelling it bound to rather than inferring it from a flag.
unsafe fn prove_value_shape(type_: *const Il2CppType, name: &str, index: Option<u32>, required: MethodMatch) -> bool {
    let position = match index {
        Some(index) => format!("parameter {index}"),
        None => "result".to_owned(),
    };

    // A generic instantiation has no shape provable from the enum, and the only resolver that can get
    // here with `generic_slots` set is one whose wrapper declared a pointer. That is what the ABI moves
    // for an instantiated class, so the slot is answered as a reference and the value proof still has
    // to hold for everything else (C48).
    if required.generic_slots && (*type_).type_() == Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST {
        debug!("AnimationSpeed: {name} {position} is a generic instantiation, bound as the pointer this wrapper declares");

        return true;
    }

    let (shape, payload) = measure_value_shape(type_);

    if shape_is_allowed(shape, required.values_only) {
        // The proof, printed: which value type bound, and how much of it the ABI moves.
        if payload != 0 {
            debug!("AnimationSpeed: {name} {position} travels by value: {}:{payload}B", type_name(type_));
        }

        return true;
    }

    match shape {
        ValueShape::Reference => debug!(
            "AnimationSpeed: {name} {position} is {}<{}>: a reference, and this wrapper declares a value",
            type_word((*type_).type_()), type_name(type_)
        ),
        _ => debug!(
            "AnimationSpeed: {name} {position} is {}<{}> of {payload} bytes: not a value a wrapper can hold",
            type_word((*type_).type_()), type_name(type_)
        ),
    }

    false
}

// The wrapper's own shape, spelled as the question asked of the class table: the parameters it
// declares, the return type it reads, whether it reserves a register for `this`, and whether a slot
// it declared as a pointer may be answered by a generic instantiation. A profile mapped onto the
// wrong question is the C15 decision again - an instance wrapper asking the table as if it had no
// `this`, or the return type it reads left out of the ask.
fn request_for<'a>(params: &'a [Il2CppTypeEnum], ret: Il2CppTypeEnum, required: MethodMatch) -> crate::il2cpp::symbols::MethodRequest<'a> {
    crate::il2cpp::symbols::MethodRequest {
        params,
        ret: Some(ret),
        allow_static: required.allow_static,
        require_static: required.require_static,
        generic_slots: required.generic_slots,
    }
}

// Whether one candidate that carries the signature can also sit behind this wrapper: it is not a
// generic definition, its parameters travel the way the wrapper declares them, and so does its
// result. Which candidate carries the signature is `symbols::select_overload`'s answer.
unsafe fn candidate_holds_wrapper(
    method: *const MethodInfo,
    name: &str,
    params: &[Il2CppTypeEnum],
    required: MethodMatch,
) -> bool {
    if (*method).is_generic() != 0 {
        debug!("AnimationSpeed: {} is generic", name);
        return false;
    }

    for position in 0..params.len() as u32 {
        let param = il2cpp_method_get_param(method, position);

        if param.is_null() {
            if required.require_ref {
                debug!("AnimationSpeed: {} parameter {} has no readable type", name, position);
                return false;
            }

            continue;
        }

        let byref = (*param).byref() != 0;

        if required.require_ref && !byref {
            debug!("AnimationSpeed: {} parameter {} is not passed by reference", name, position);
            return false;
        }

        if required.forbid_ref && byref {
            debug!("AnimationSpeed: {} parameter {} is passed by reference, the wrapper declares a value", name, position);
            return false;
        }

        // A reference parameter travels as an address no matter what it points at, so its size is
        // the callee's business. The proof below is for the value a wrapper declared in its place.
        if !byref && !prove_value_shape(param, name, Some(position), required) {
            return false;
        }
    }

    let return_type = il2cpp_method_get_return_type(method);

    if return_type.is_null() {
        debug!("AnimationSpeed: {} has no readable return type", name);
        return false;
    }

    // The result travels the same way the arguments do, and the wrappers that read these results
    // as numbers are sized for a register: a struct returned through memory, or a reference handed
    // back where an `i32` was read, is the A5 and A7 hazard on the output side.
    prove_value_shape(return_type, name, None, required)
}

unsafe fn resolve_method_any(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
    required: MethodMatch,
) -> usize {
    use crate::il2cpp::symbols::{OverloadAnswer, OverloadRejection, get_method_overloads, overload_rejects_request, select_overload};

    // C15. The class table is asked for the whole signature the wrapper was written against, and
    // every method carrying it is a candidate. The walk used to answer the first method under the
    // name and parameter list and the checks ran against that one method, so an overload that was
    // not the one meant decided the install. The pair this client's dump prints for
    // `Gallop.ModelController::GetBodyShader/2` - the `static class<Shader>(struct, struct)` at
    // `introspect.log:1188` and the instance `class<Shader>(struct, struct)` at 1189 - is that
    // shape: name, arity and parameter list answer 1188 first, and a caller written for 1189 has
    // nothing left to stand on. No site in this tree asks for `GetBodyShader`, so what the pair
    // would do to such a wrapper is what the checks below are for; the C15 concept counts the pairs
    // of this shape this client has (two, in this dump) and which of them a hook site names (none).
    let request = request_for(params, ret, required);
    let candidates = get_method_overloads(class, name, &request);

    if candidates.is_empty() {
        debug!("AnimationSpeed: {} has no overload with the expected signature", name);
        return 0;
    }

    match select_overload(&candidates, &request) {
        // Nothing under this name carries the signature. The two reasons a candidate was not it are
        // the two the install line used to print about a single method picked by table order, now
        // read off each method that was found.
        OverloadAnswer::None => {
            for candidate in &candidates {
                match overload_rejects_request(candidate, &request) {
                    Some(OverloadRejection::Return { actual, expected }) => warn!(
                        "AnimationSpeed: {name} returns il2cpp type {}, wrapper expects {}",
                        actual.unwrap_or(u32::MAX),
                        expected,
                    ),
                    Some(OverloadRejection::Staticness { candidate_is_static: true }) => {
                        debug!("AnimationSpeed: {name} is static, its arguments would be misread")
                    },
                    Some(OverloadRejection::Staticness { candidate_is_static: false }) => {
                        debug!("AnimationSpeed: {name} is not static, a wrapper without `this` would misread it")
                    },
                    None => {},
                }
            }

            return 0;
        },

        // A2 is still open, and this is the honest shape of it: `CLASS` matches every reference type,
        // so `SingleModeResultContentBase::FadeInContentFromRight/3` taking `UnityEngine.CanvasGroup`
        // and the one taking `UnityEngine.UI.MaskableGraphic` (`introspect.log:24167-24168`) are one
        // request to a matcher that compares enums. That collision is live at the site written
        // against it: `hook/umamusume/SingleModeResultContentBase.rs` asks for the `/3` pair with
        // `[class, r4, class]` and gets the ambiguity line below on every install. This file's own
        // door is the different method the dump prints once, `TeamStadiumGrandResultViewController::
        // FadeInContentFromRight/2 -> void(class<UnityEngine.CanvasGroup>, float)` at 25842, and
        // answers it uniquely. The collision is printed instead of settled in silence, and the first
        // candidate the table lists is bound as it always was: both travel as the pointer the wrapper
        // declares, so what the collision decides is which of the two calls get scaled, not whether
        // the arguments are read from the right register. Telling them apart is parameter class names.
        OverloadAnswer::Ambiguous(count) => debug!(
            "AnimationSpeed: {name} has {count} overloads carrying the requested signature, binding the first the class table lists",
        ),

        OverloadAnswer::Unique(_) => {},
    }

    for candidate in &candidates {
        if overload_rejects_request(candidate, &request).is_some() {
            continue;
        }

        let method = candidate.method;

        if candidate_holds_wrapper(method, name, params, required) {
            return (*method).methodPointer;
        }
    }

    0
}

// Scaling only the read half of a settable property is unsafe: a game path that does
// `prop = prop - 1` writes the already divided value back into the backing field, and
// the value collapses geometrically instead of being shortened once. A getter is only
// wrapped when the class has no matching setter to write it back.
unsafe fn resolve_getter(class: *mut Il2CppClass, name: &str, ret: Il2CppTypeEnum) -> usize {
    // Zero-argument getters are safe either way: a static one simply ignores the `this`
    // register the wrapper hands over.
    let addr = resolve_method_any(class, name, &[], ret, MATCH_GETTER_EITHER);

    if addr == 0 {
        return 0;
    }

    let rest = match name.strip_prefix("get_").or_else(|| name.strip_prefix("Get")) {
        Some(rest) => rest,
        None => return addr,
    };

    let setter = CString::new(format!("set_{rest}")).unwrap();

    if !il2cpp_class_get_method_from_name(class, setter.as_ptr(), 1).is_null() {
        debug!("AnimationSpeed: {} is settable, read-only scaling would compound", name);
        return 0;
    }

    addr
}

// One debug line per scaling point, the first time the game actually calls through it.
// Installing a hook and reaching it are different facts, and a test run has to tell them
// apart.
const HIT_SLOTS: usize = 20;
static FIRST_HIT: [AtomicBool; HIT_SLOTS] = [const { AtomicBool::new(false) }; HIT_SLOTS];

// Counts as well as first changed value. `hit` used to print only when `raw != scaled`, so a getter
// the game calls every frame with a duration of `0.0`, or a value the option cannot move, left no
// trace at all and looked identical to a hook the game never reached. That is how five training hooks
// stayed unreadable across two career runs (A21), and the training footer and plate getters are the
// regular training path, not the friendship one. A slot is now counted on every call, the first
// `HIT_DETAIL_LIMIT` calls print whether or not anything changed, and later ones only on a chunk
// boundary so a per frame getter cannot fill the log.
const HIT_DETAIL_LIMIT: usize = 4;
const HIT_CHUNK: usize = 1024;
static HIT_CALLS: [AtomicUsize; HIT_SLOTS] = [const { AtomicUsize::new(0) }; HIT_SLOTS];

pub fn hit(slot: usize, name: &str, raw: f32, scaled: f32) {
    if slot >= HIT_SLOTS {
        return;
    }

    let calls = HIT_CALLS[slot].fetch_add(1, Ordering::Relaxed) + 1;

    if raw != scaled {
        if !FIRST_HIT[slot].swap(true, Ordering::AcqRel) {
            debug!("AnimationSpeed: {name} {raw} -> {scaled}");
        }

        return;
    }

    if calls <= HIT_DETAIL_LIMIT {
        debug!("AnimationSpeed: {name} call {calls} {raw} -> {scaled} unchanged");
    }
    else if calls % HIT_CHUNK == 0 {
        debug!("AnimationSpeed: {name} {calls} calls, none of them changed anything");
    }
}

// What a run reads: how many times the game actually reached a scaling point, read apart from whether
// the value moved.
pub fn hit_calls(slot: usize) -> usize {
    HIT_CALLS.get(slot).map(|counter| counter.load(Ordering::Relaxed)).unwrap_or(0)
}

// The training doors a run reads, as run 11 found them. The five these replaced installed in run 8 and
// printed no call line in four career runs (C51), so they were hooks on doors this client never opens.
//
// They are the census of the training gates, not the list of what this fork scales: since item 59 both
// slots hand the game the value the caller passed, and the counts are there because a run has to be able
// to tell a gate the fork left alone from a gate the game never reached (A4). A slot printing `=0` and a
// slot printing 36 are different facts even though neither door moved a number.
//
// `SingleModeMainViewTrainingCutStatus.PlayIn` was a third point here until run 26. That session read it
// scaling 2.4 s to 0.12 s while three of its seven training cuts held the status panel off for 7866, 8182 and
// 7866 ms, and the human turned this group off mid session and the next two cuts closed with no hole at all at
// 1792 and 2042 ms. The door that actually plays the panel in is `CoroutinePlayIn/2 -> IEnumerator(float, int)`
// next to it, so shortening only the `PlayIn` float leaves the cut's coroutine gated on a play in the panel no
// longer performs, which is the shape item 72 measured: the cut-in clock sitting finished at 2.4 of 2.4 while
// branch 6 polls 283 times. A scaling point that costs eight seconds a turn is not a speed option, so the
// door is left to the game and this list is what a run can still check.
pub const TRAINING_HIT_SLOTS: [(usize, &str); 2] = [
    (11, "SingleModeMainViewHpGauge.SetProgressbarBlendTime"),
    (12, "TrainingParamChangeUI.InitializePlateList"),
];

// These getters hand out a hardcoded duration or a playback scale, and are the only way
// to reach a `const` duration that is not passed as an argument anywhere.
//
// C2: this was the last helper macro in the tree that wrote an armed wrapper with nothing in front
// of its body. `get_orig_fn!` + `scale_*` + `hit()` in a bare `extern "C" fn` means a panic in the
// `debug!` behind `hit`, or a fault on the way to the original, leaves through a trampoline frame
// that has no unwind info at all, and the process goes with it. It now expands into `def_detour!`'s
// publish arm, the shape every other armed detour in this tree is written with, and the two answers
// a trip at this boundary may give are both stated here rather than left to the macro's last resort:
//
// - **The game's own number, published on the line it arrives on.** `answer.publish(raw)` sits
//   directly after the original returns, so a trip anywhere later - in the scaling, in `hit`, in the
//   logger behind it - hands the game back the value its method produced instead of a zero. At
//   `get_TimeScaleEventWipe` / `get_TimeScaleAfterEndStory` a zero is a story clip that stops
//   advancing, and at the three duration getters it is an animation removed wholesale; neither is a
//   decision the mod gets to make on a trip that came out of its own logging half. It is the same
//   order `StoryFrameProbe::get_TimeScale` and `StoryTimelineController_GetTimeScaleByHighSpeedType`
//   publish in, for the same reason.
// - **For the one case where the game's number cannot be reached at all, the value the wrapper
//   states at its own call site.** That case is C1: `get_orig_fn!` answers 0 for a trampoline the
//   detach path has just taken back, the call jumps to 0, the barrier takes an access violation, and
//   the value it would then have to invent is the zero above. `get_orig_fn_guarded!` is the same
//   `CachedTrampoline` answering `None`, so AGENTS section 2 ("unresolved targets stay inert, said
//   once, off the hot path") holds at this boundary and the fault is never taken in the first place.
//   What the wrapper answers then is the neutral value for what that getter hands out:
//   `MIN_TIME_SCALE` for a playback scale (the game's own speed - a 0 scale is a pause), and 0.0 for
//   a duration (no wait: the one value `scale_duration` already passes through untouched, and the
//   failure mode that cannot stall a coroutine or add time the game did not plan for).
macro_rules! def_getter_hook {
    ($hook:ident, $group:expr, $scale:ident, $slot:literal, no_answer $no_answer:block) => {
        def_getter_hook!(
            $hook,
            $group,
            $scale,
            $slot,
            no_answer $no_answer,
            // The shipped game half: this wrapper's own trampoline, or nothing.
            orig {
                type Original = extern "C" fn(*mut Il2CppObject) -> f32;

                get_orig_fn_guarded!($hook, Original)
            },
            after_call {}
        );
    };

    // The same wrapper with the two halves a test cannot reach supplied by the test: the shipped
    // body asks the registry for its trampoline, whose cold branch reaches `Hachimi::instance()`,
    // which ends the test process (AGENTS section 4). Nothing shipped instantiates this arm - the
    // arm above delegates to it - so the wrappers the barrier tests drive are built from the shipped
    // shape, with the game call and the mod half behind it written as expressions rather than as a
    // copy of this body nobody checks against the shipped one.
    ($name:ident, $group:expr, $scale:ident, $slot:literal, no_answer $no_answer:block, orig $orig:block, after_call $after_call:block) => {
        def_detour! {
            $name(this: *mut Il2CppObject) answer -> f32 {
                let original = $orig;

                let Some(orig) = original else {
                    // Inert, and the reason is said once by `resolve_or_none` on the branch that is
                    // already skipping the call. No call through 0 stands on this boundary.
                    return $no_answer;
                };

                let raw = orig(this);

                // Published the moment the game answers: a trip after this line answers with the
                // game's value, and `hit`'s logging half can no longer cost the game its own number.
                answer.publish(raw);

                $after_call;

                let scaled = $scale(raw, $group);

                hit($slot, stringify!($name), raw, scaled);

                scaled
            }
        }
    };
}

macro_rules! install_getter {
    ($classes:ident, $hook:ident, $class:ident, $method:ident) => {
        if let Some(class) = $classes.get(stringify!($class)).copied() {
            let addr = unsafe { resolve_getter(class, stringify!($method), Il2CppTypeEnum_IL2CPP_TYPE_R4) };

            if addr != 0 {
                new_hook!(addr, $hook);
            }
        }
    };
}

fn normalize(value: f32) -> f32 {
    // These are speed-ups, and the multiplier is written straight into the game's live
    // constants, so a hand-edited config cannot push it past what the UI offers.
    if value.is_finite() { value.clamp(1.0, MAX_FACTOR) }
    else { 1.0 }
}

// The `time_scale` lever is clamped here too, to the same bounds the Config Editor
// offers and to MAX_TIME_SCALE: config.json is deserialized unbounded, and this is the
// one place every consumer of the lever reads it through. Its floor is the neutral 1.0, so
// a lever asking for a slow down lands on doing nothing (C40).
fn normalize_time_scale(value: f32) -> f32 {
    if value.is_finite() { value.clamp(MIN_TIME_SCALE, MAX_TIME_SCALE) }
    else { 1.0 }
}

// The cut-in lever's own clamp, wider than `normalize_time_scale` because the control feeds two doors and stops at
// the further of their ceilings (`MAX_TRAINING_CUT_LEVER`). The floor is the neutral 1.0 for the same reason as
// above, and a non numeric value lands on doing nothing.
fn normalize_training_cut_lever(value: f32) -> f32 {
    if value.is_finite() { value.clamp(MIN_TIME_SCALE, MAX_TRAINING_CUT_LEVER) }
    else { 1.0 }
}

// The DOTween lever is bounded here, by the same argument as the group factors: it is a
// multiplier that reaches the game's own clock, so a hand edited config or the 1000.0 the
// sliders offer cannot take more out of an animation than MAX_FACTOR allows. Non numeric
// input falls back to the neutral 1.0 rather than reaching a tween as NAN.
fn normalize_ui_animation_scale(value: f32) -> f32 {
    if value.is_finite() { value.clamp(MIN_UI_ANIMATION_SCALE, MAX_UI_ANIMATION_SCALE) }
    else { 1.0 }
}

// The config is read here, once per pass, and mirrored into the same atomics every
// argument scaling detour reads. The game tick path never reaches it: `apply_if_dirty`
// bails out before this when nothing has changed.
fn refresh_config_mirrors() {
    let config = Hachimi::instance().config.load();
    note_config_read();

    mirror_config(&config);
}

// The mirrors themselves, held apart from the config read so the values `apply`, `plan_pass`
// and every scaling detour work from are the shipped code, testable the way `Time.rs`'s
// `plan_write` is. `Config::default()` is what a build at the shipped options holds.
fn mirror_config(config: &Config) {
    let transition = normalize(config.transition_speed);
    let screens = normalize(config.result_screen_speed);
    let story = normalize(config.story_speed);
    let ui = normalize_ui_animation_scale(config.ui_animation_scale);

    TRANSITION_FACTOR.store(transition.to_bits(), Ordering::Release);
    SCREENS_FACTOR.store(screens.to_bits(), Ordering::Release);
    STORY_FACTOR.store(story.to_bits(), Ordering::Release);
    TIME_SCALE.store(normalize_time_scale(config.time_scale).to_bits(), Ordering::Release);
    UI_ANIMATION_SCALE.store(ui.to_bits(), Ordering::Release);
    // The same ceiling as the group factors, on the one training duration this fork scales: a hand edited
    // config.json cannot ask for more than MAX_FACTOR out of a plate cascade.
    PLATE_FACTOR.store(normalize(config.training_plate_speed).to_bits(), Ordering::Release);
    // The cut-in lever takes the time-scale floor, not the duration one: it is a lever on a rate, so its floor is
    // its neutral 1.0 and a hand edited config asking for 0.1 cannot turn a training cut into a slow motion (C40).
    // Its top is `MAX_TRAINING_CUT_LEVER`, and what each door may hand is capped there rather than here: the scale
    // door at `MAX_TRAINING_CUT_TIME_SCALE`, the motion doors at `MAX_MOTION_SPEED`.
    TRAINING_CUT_FACTOR.store(normalize_training_cut_lever(config.training_cut_speed).to_bits(), Ordering::Release);
    // The tag cut-in Animator lever takes the same clamp: it is a rate, so its floor is its neutral 1.0 and a
    // hand edited config asking for 0.1 cannot slow a training cut-in down. What it may raise to is bounded in
    // `tag_cut_animator_speed`, next to the write, because the bound reads the scale this layer put into
    // `Time.timeScale`.
    TAG_CUT_FACTOR.store(normalize_time_scale(config.training_tag_cut_speed).to_bits(), Ordering::Release);
    // Clamped here so neither story choice site divides by the delay itself: `CheckChoiceAutoTap`
    // runs while a choice is up and `GetTimeScaleByHighSpeedType` is a story path getter, so both
    // read this mirror instead of the config (C24).
    STORY_CHOICE_AUTO_SELECT_MULT.store(
        normalize_story_choice_auto_select_multiplier(config.story_choice_auto_select_delay).to_bits(),
        Ordering::Release,
    );

    note_pair_ceiling(ui, [transition, screens, story]);
    note_training_gate_levers();
}

// What each training gate is priced on has to be visible in `hachimi.log`, because both are armed and a door that
// hands the value it received prints `call N 1 -> 1 unchanged`, which is the same shape as a hook the game never
// reached (A4). Without this line a run reading `TrainingParamChangeUI.InitializePlateList=36` cannot tell "the
// lever is at its neutral 1.0" from "the lever does not reach this door" from "the game never called it". Said once
// per process, by the config pass that mirrors the levers, naming the doors exactly as the training census names
// them, and naming the lever the plate door carries so a run can tell which arm it was on (C58, ledger items 59
// and 74).
static TRAINING_DOORS_NOTE_LOGGED: AtomicBool = AtomicBool::new(false);

// Named exactly as `TRAINING_HIT_SLOTS` names them, so a run can match this line to the census counts.
const TRAINING_PLATE_DOOR: &str = "TrainingParamChangeUI.InitializePlateList";
const TRAINING_GAUGE_DOOR: &str = "SingleModeMainViewHpGauge.SetProgressbarBlendTime";
// The cut-in clock door, named as `TrainingCuttProbe` names it in its totals line
// (`SingleModeUtils::GetTrainingCutTimeScale(scale)=N peak X`), and as `hit` prints it on its first calls. It is
// not in `TRAINING_HIT_SLOTS`: that table counts doors that shorten a *duration*, and this one hands the cut-in
// engine a scale. The slot is the one `hit` counts it under.
pub const TRAINING_CUT_DOOR: &str = "SingleModeUtils.GetTrainingCutTimeScale";
const TRAINING_CUT_SLOT: usize = 13;

/// Whether this config pass got the line.
fn note_training_gate_levers() -> bool {
    if TRAINING_DOORS_NOTE_LOGGED.swap(true, Ordering::AcqRel) {
        return false;
    }

    debug!(
        "AnimationSpeed: training gates: {} scales on training_plate_speed {} alone, off the pair bound; {} hands the game the value it was passed; {} scales on training_cut_speed {} alone, the scale it hands capped at {}; result_screen_speed reaches result screens only",
        TRAINING_PLATE_DOOR,
        plate_factor(),
        TRAINING_GAUGE_DOOR,
        TRAINING_CUT_DOOR,
        training_cut_factor(),
        MAX_TRAINING_CUT_TIME_SCALE,
    );

    true
}

// A clamp a run cannot see is a clamp that did not happen (AGENTS section 2). The pair ceiling shows
// up on a door's hit line as a value handed on unchanged, which reads the same as a hook the game
// never reached, so the config pass - once per setting change or view change, never a detour - says
// it once for each clock setting that binds it, and it says it door by door: which armed doors the trim
// reaches because the pair composes on them, which it reaches because nothing has measured otherwise, and
// which it does not reach at all. `PAIR_CEILING_LOGGED_CLOCK` holds the pair of numbers the line states about
// the clock, the composed delta clock and the ui cap it leaves; `normalize_ui_animation_scale` cannot produce
// 0.0 and `delta_clock_of` returns 1.0 for a mirror it cannot read, so its 0 sentinel means "nothing owed".
// The cap is in the key because a `Time.timeScale` write moves it while leaving the composed clock where the
// last line said it: `ui_animation 20` over a Unity holding 1.0 and over a Unity holding 5.0 are both a 20x
// channel, at a 20x ui clock and a 4x one, and the second is a clamp a run has to be able to see.
static PAIR_CEILING_LOGGED_CLOCK: AtomicU64 = AtomicU64::new(0);

/// The latch key for one announced clock: the composed clock and the ui cap that produced it.
fn pair_ceiling_key(clock: f32, cap: f32) -> u64 {
    ((clock.to_bits() as u64) << 32) | cap.to_bits() as u64
}

/// The word a lane is read under in `hachimi.log`.
fn pace_lane(pace: Pace) -> &'static str {
    match pace {
        Pace::TweenMeasured(_) => PACE_LANE_TWEEN_MEASURED,
        Pace::OffTweenClock(_) => PACE_LANE_OFF_CLOCK,
        Pace::Unproven => PACE_LANE_UNPROVEN,
    }
}

const PACE_LANE_TWEEN_MEASURED: &str = "tween measured";
const PACE_LANE_UNPROVEN: &str = "unproven, bounded";
const PACE_LANE_OFF_CLOCK: &str = "off the tween clock, whole group factor";

/// The armed doors grouped by the lane their bound reaches them on, in the order `ARMED_DOOR_PACES` holds
/// them. A door that ever appears under "off the tween clock" got there through a measurement this file armed,
/// and the line is where a run reads that none has - the same `countup_pace` the door scales with, so the lanes
/// a career run reads and the lanes the shipped code trims on are one fact rather than two that can drift.
fn armed_door_lanes() -> String {
    let mut lanes = String::new();

    for lane in [PACE_LANE_TWEEN_MEASURED, PACE_LANE_UNPROVEN, PACE_LANE_OFF_CLOCK] {
        let doors: Vec<&str> = ARMED_DOOR_PACES
            .iter()
            .filter(|(door, _, pace)| pace_lane(armed_door_pace(door, *pace)) == lane)
            .map(|(door, _, _)| *door)
            .collect();

        if doors.is_empty() {
            continue;
        }

        if !lanes.is_empty() {
            lanes.push_str("; ");
        }

        let _ = write!(lanes, "{lane}: {}", doors.join(", "));
    }

    lanes
}

/// What the line says about this fork's share of the scale Unity is holding. The share is a fact about the
/// clock the line is pricing, so it is read off the raise that clock carries and never written into the
/// sentence: the neutral 1.0 means the write layer added nothing and the whole scale is what the game put
/// there, and a raise above 1.0 is named with its own number, because a sentence that prices a 2.5x scale
/// cannot also claim it raised nothing. The literal this replaces said so whatever the raise was, and every
/// test for the line asked only whether it was owed, so no gate could see the sentence contradict the number
/// beside it (C58, ledger item 62).
fn time_scale_raise_note(raise: f32) -> String {
    if raise > 1.0 {
        // The same words `note_countup_completion` prints, so the ceiling line and a completion line name the
        // share of the game's clock this fork put there with one phrase a run can grep.
        format!("{raise}x of it the time_scale lever added")
    } else {
        String::from("the time_scale lever raised nothing")
    }
}

/// Whether this config pass got the line. The delta clock - the ui cap over the whole scale the
/// `Time.timeScale` write layer left in the game - is what binds the ceiling, so a scale the write layer
/// produced is a fact the line is owed for even with `ui_animation` neutral, and a clock back at 1.0 clears
/// the marker so an arm switch into a binding clock later in the same session is a window a run can still see.
fn note_pair_ceiling(ui: f32, factors: [f32; 3]) -> bool {
    let produced = time_scale_produced();
    let raise = time_scale_raise();
    let clock = delta_clock_of(ui, produced);
    let cap = ui_clock_of(ui, produced);

    if clock <= 1.0 || !clock.is_finite() {
        PAIR_CEILING_LOGGED_CLOCK.store(0, Ordering::Release);
        return false;
    }

    let key = pair_ceiling_key(clock, cap);

    if PAIR_CEILING_LOGGED_CLOCK.swap(key, Ordering::AcqRel) == key {
        return false;
    }

    let lanes = armed_door_lanes();
    let lever_note = time_scale_raise_note(raise);

    // The owed reading, stated on the line a run reads, in the tense the door is actually in: bounded while
    // nothing has measured it, moved once the bracket has. A clamp a run cannot see is a clamp that did not
    // happen, and a door that moved out of the clamp has to be as visible as one held in it.
    let countup = match countup_pace() {
        Pace::OffTweenClock(_) => "has moved off the trim on a measured completion wall time and hands its group's whole factor",
        Pace::TweenMeasured(_) => "is held at the trim on a measured completion wall time",
        Pace::Unproven => "is bounded until one is read",
    };

    debug!(
        "AnimationSpeed: pair ceiling {product}x on one completion both layers reach: ui_animation {ui}x over the {produced}x Unity is holding in Time.timeScale from this fork's write layer ({lever_note}) is a {clock}x delta clock, so the ui clock runs {cap}x and {headroom}x is left for a group factor (transition {transition}, result {screens}, story {story}); the 20x bounds what this fork puts on the delta channel - the independentTime channel a time scale independent tween is advanced on and the story timeline's own time scale (MAX_TIME_SCALE {max_time_scale}x) are outside it, and a Time.timeScale past the ceiling is the game's own, left where the game put it; the trim reaches each armed door through its pace - {lanes}; a door leaves the trim only on a measured completion wall time, and {COUNTUP_DOOR} {countup}",
        product = MAX_TWEEN_SPEED_PRODUCT,
        produced = produced,
        lever_note = lever_note,
        clock = clock,
        cap = cap,
        headroom = (MAX_TWEEN_SPEED_PRODUCT / clock).max(1.0).min(MAX_FACTOR),
        transition = factors[0],
        screens = factors[1],
        story = factors[2],
        max_time_scale = MAX_TIME_SCALE,
        lanes = lanes,
    );

    true
}

// The factors as the write loop sees them: the mirrors, not the config. Indexed by
// `group_index`. The fourth is the training group's pinned 1.0 - it is in the array because the write
// loop walks one slot per group, and it is 1.0 because no option writes that group.
fn mirrored_factors() -> [f32; 4] {
    [
        factor(Group::Transition),
        factor(Group::Screens),
        factor(Group::Story),
        factor(Group::Training),
    ]
}

// The speed groups, in the order `Group` declares them and the order `APPLIED_FACTORS` is indexed.
fn group_index(group: Group) -> usize {
    match group {
        Group::Transition => 0,
        Group::Screens => 1,
        Group::Story => 2,
        Group::Training => 3,
    }
}

/// The decision one group gets from a pass, kept free of il2cpp so the sequence is
/// testable the way `Time.rs`'s plan_write and `HighSpeedSetting`'s plan_pass are.
/// `wanted` is the configured factor, `applied` the factor the group's fields currently
/// sit at, NAN for "this module has never written it", which is the state a neutral
/// factor asks for: a build with every option at 1.0 rewrites nothing at all.
fn plan_group(wanted: f32, applied: f32) -> bool {
    wanted != if applied.is_nan() { 1.0 } else { applied }
}

/// The plan of a whole pass: which groups are due for a rewrite, read from the mirrors and the
/// applied markers. `apply` runs this and the tests run this, so "does this pass touch the field
/// table at all" is one shipped decision rather than a shape a test module copies. Whether a
/// group has any field in it is the table's business; this answers only which groups are due.
fn plan_pass(factors: [f32; 4]) -> [bool; 4] {
    [
        plan_group(factors[0], f32::from_bits(APPLIED_FACTORS[0].load(Ordering::Acquire))),
        plan_group(factors[1], f32::from_bits(APPLIED_FACTORS[1].load(Ordering::Acquire))),
        plan_group(factors[2], f32::from_bits(APPLIED_FACTORS[2].load(Ordering::Acquire))),
        // Never due: `plan_group(1.0, NAN)` is "the game's own values are what this group asks for".
        plan_group(factors[3], f32::from_bits(APPLIED_FACTORS[3].load(Ordering::Acquire))),
    ]
}

/// Close a pass out. A group is marked as written at the factor it was written at unless a field
/// in it had no baseline this pass, which is the state that asks the next pass to look again
/// (a class whose static constructor has not run reads as zero until its scene loads). Leaving
/// the marker unset is what turns "retry" into one pass per view change, not one per frame.
fn finish_pass(rewrite: [bool; 4], factors: [f32; 4], no_baseline: [usize; 4]) {
    for group in 0..rewrite.len() {
        if rewrite[group] && no_baseline[group] == 0 {
            APPLIED_FACTORS[group].store(factors[group].to_bits(), Ordering::Release);
        }
    }
}

/// The baseline a write scales from: whatever the game left in the field, unless the
/// field still holds our own last write, in which case the remembered baseline is kept.
/// Multiplying our own write is what makes a factor compound against a value the game
/// reassigns at runtime (C22/C24).
fn baseline(current: f64, last_written: f64, remembered: f64) -> f64 {
    if current != last_written { current } else { remembered }
}

/// Re-read the configured lever, clamp it and mirror it. The deferred `Time.timeScale`
/// write calls this so the value it hands the game is bounded even if `apply()` has not
/// run yet; the detour that runs per write only reads the mirror.
pub fn refresh_time_scale() -> f32 {
    let value = normalize_time_scale(Hachimi::instance().config.load().time_scale);
    TIME_SCALE.store(value.to_bits(), Ordering::Release);
    value
}

// One `il2cpp_field_static_get_value` per call: `init` keeps only fields `is_number` accepts,
// so every entry the write loop reaches lands on one of the three arms below. The count is here
// rather than at the call sites because the pass, not a test of it, is what owes the number.
fn read_static(field: *mut FieldInfo, kind: Il2CppTypeEnum) -> f64 {
    APPLY_FIELD_READS.fetch_add(1, Ordering::Relaxed);

    match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => {
            let mut value: f32 = 0.0;
            il2cpp_field_static_get_value(field, &mut value as *mut f32 as *mut c_void);
            value as f64
        }
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => {
            let mut value: f64 = 0.0;
            il2cpp_field_static_get_value(field, &mut value as *mut f64 as *mut c_void);
            value
        }
        Il2CppTypeEnum_IL2CPP_TYPE_I4 => {
            let mut value: i32 = 0;
            il2cpp_field_static_get_value(field, &mut value as *mut i32 as *mut c_void);
            value as f64
        }
        _ => 0.0
    }
}

// The arithmetic one write performs, kept apart from the il2cpp call so the passes are
// testable without a game (see the tests at the end of this file).
//
// This half scales by `factor(group)` alone, not by `duration_factor(group)`: the field table has no clock
// layer to pair it with, and the group's own `MAX_FACTOR` cap is the ceiling these writes have (C58, ledger
// item 62). It is not an oversight to patch here. The table mixes `f32` durations with `i4` frame counts, and frame
// counts are polled on the frame clock the tween layer does not speed up, so a bound applied per group
// would also cap half the entries that have no pair to bound; and one `APPLIED_FACTORS` marker per group
// cannot record a duration factor and a frame count factor at once, which is the marker that decides
// whether a pass rewrites a field at all. On this client the question is inert - 0 of 61 duration fields
// resolve, every one of them a compile time constant (C13), and every apply pass reports `0 field
// writes`. Bounding this half first means widening the marker table, and that is a decision for a client
// whose fields do resolve.
fn scale_value(original: f64, factor: f32, kind: Il2CppTypeEnum) -> f64 {
    match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => (original as f32 / factor) as f64,
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => original / factor as f64,
        _ => {
            // Frame counts round to the nearest frame but never below one frame unless
            // the field is already zero: a zero wait between story blocks can starve the
            // block-advance coroutine. A negative offset moves towards zero, which is
            // the same shortening in the other direction.
            let scaled = original / factor as f64;

            let frames = if original > 0.0 { scaled.max(1.0).round() as i32 }
                else if original < 0.0 { scaled.min(-1.0).round() as i32 }
                else { 0 };

            frames as f64
        }
    }
}

// One `il2cpp_field_static_set_value` per call, counted on the way in for the same reason
// `read_static` is.
fn write_static(field: *mut FieldInfo, kind: Il2CppTypeEnum, original: f64, factor: f32) -> f64 {
    APPLY_FIELD_WRITES.fetch_add(1, Ordering::Relaxed);

    let value = scale_value(original, factor, kind);

    match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => {
            let stored = value as f32;
            il2cpp_field_static_set_value(field, std::ptr::from_ref(&stored) as *mut c_void);
        }
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => {
            let stored = value;
            il2cpp_field_static_set_value(field, std::ptr::from_ref(&stored) as *mut c_void);
        }
        _ => {
            let stored = value as i32;
            il2cpp_field_static_set_value(field, std::ptr::from_ref(&stored) as *mut c_void);
        }
    }

    value
}

fn is_number(kind: Il2CppTypeEnum) -> bool {
    matches!(
        kind,
        Il2CppTypeEnum_IL2CPP_TYPE_R4
            | Il2CppTypeEnum_IL2CPP_TYPE_R8
            | Il2CppTypeEnum_IL2CPP_TYPE_I4
    )
}

pub fn init(umamusume: *const Il2CppImage) {
    let mut entries: Vec<Entry> = Vec::new();
    let mut classes: Vec<(&'static str, usize)> = Vec::new();
    let mut skipped = 0usize;

    for spec in FIELDS {
        let class = match classes.iter().find(|(name, _)| *name == spec.class) {
            Some((_, ptr)) => *ptr as *mut Il2CppClass,
            None => {
                let name = CString::new(spec.class).unwrap();
                let class = il2cpp_class_from_name(umamusume, c"Gallop".as_ptr(), name.as_ptr());

                if class.is_null() {
                    debug!("AnimationSpeed: Gallop.{} not present in this build", spec.class);
                    skipped += 1;
                    continue;
                }

                classes.push((spec.class, class as usize));
                class
            }
        };

        let field_name = CString::new(spec.field).unwrap();
        let field = il2cpp_class_get_field_from_name(class, field_name.as_ptr());

        if field.is_null() {
            debug!("AnimationSpeed: {}.{} not present in this build", spec.class, spec.field);
            skipped += 1;
            continue;
        }

        // Instance fields live inside an object and have no single value to scale.
        if il2cpp_field_get_flags(field) & FIELD_ATTRIBUTE_STATIC == 0 {
            debug!("AnimationSpeed: {}.{} is not static", spec.class, spec.field);
            skipped += 1;
            continue;
        }

        // A `const` has no slot in the static data area: IL2CPP folds it into the call
        // sites, so writing through its FieldInfo lands on unrelated memory.
        if il2cpp_field_get_flags(field) & FIELD_ATTRIBUTE_LITERAL != 0 || il2cpp_field_is_literal(field) {
            debug!("AnimationSpeed: {}.{} is a compile-time constant", spec.class, spec.field);
            skipped += 1;
            continue;
        }

        let field_type = il2cpp_field_get_type(field);
        if field_type.is_null() {
            skipped += 1;
            continue;
        }

        let kind = unsafe { (*field_type).type_() };
        if !is_number(kind) {
            debug!("AnimationSpeed: {}.{} is not a duration number", spec.class, spec.field);
            skipped += 1;
            continue;
        }

        entries.push(Entry {
            class: spec.class,
            field: spec.field,
            group: spec.group,
            info: field as usize,
            kind,
            original: 0.0,
            last_written: f64::NAN,
        });
    }

    info!(
        "AnimationSpeed: resolved {}/{} duration fields ({} unavailable)",
        entries.len(),
        FIELDS.len(),
        skipped
    );

    *ENTRIES.lock().unwrap() = entries;
    install_getters(umamusume);

    // Not `apply()`. This function runs between `Interceptor::begin_batch` and `finish_batch`
    // (`hook/mod.rs:272-307`), which sits inside `DllMain` under the loader lock
    // (`windows/main.rs:62-73`, `windows/hook.rs:64-71`): nothing is armed yet and the game has
    // not finished initialising. `apply()` reaches StoryManager and the save loader through
    // `HighSpeedSetting` - its getter chain and `SaveHighSpeedType`, which persists - and rewrites
    // the duration fields, so the install path was calling game code, including a saving write,
    // from the loader lock.
    //
    // The mirrors are refreshed because they are plain atomics that touch no game state, and they
    // are what the argument scaling detours and the `set_timeScale` hook read: leaving them at 1.0
    // until the first tick would run the boot fades at the shipped speed. The writes wait for
    // DIRTY, so the first pass is the first `GameSystem_Update` tick on the game thread through
    // `apply_if_dirty` (`GameSystem.rs:49`) - the deferred shape `Time::init` already uses - and
    // `SceneManager::ChangeView` runs one before the first view change reads its fade constants.
    refresh_config_mirrors();
    mark_dirty();
}

// Each site states the answer its getter gives when it cannot reach its own original: 0.0 for a
// duration (no wait - a value the game itself uses and `scale_duration` already refuses to change),
// `MIN_TIME_SCALE` for a playback scale (the game's own speed, because a 0 scale is a pause).
//
// The pace every armed duration door stands on, with the line that put it there. This is where item 62's
// bound is aimed: the trim reaches a door through this table and not through the group its option sits in, so
// it cannot reach a door whose completion the clock layer is not on, and it cannot be lifted off a door by a
// classification nobody read. Two of these lanes trim the same way - `TweenMeasured` because the pair composes
// there, `Unproven` because nothing has said it does not - and only `OffTweenClock` leaves the trim, with the
// reading that put the door there carried inside the variant.
//
// A door belongs on `TweenMeasured` only when the lines it cites put the completion on that channel. A field
// being present says what a class holds, not which half of it measures a completion, so where the dump names both
// channels for the value a door hands, the door is `Unproven`. The count up door and the text modifier pair are
// the two in that shape below, and each carries the lines that put it there.
//
// The wipe fades are animation this class builds as tweens: `PlayFadeFrontCanvas/5 -> void(float, float,
// float, class<System.Action>, struct<DG.Tweening.Ease:4B>)` (`introspect.log:18317`) carries a DOTween ease
// in the same class as the door, and the class' two coroutine paths, `SetupCrossFadeAsync/1` and
// `PlayCrossFadeAsync/1` (18319-18320), take an `Action` and no duration at all. Nothing in this client's dump
// hands the float `PlayFadeNowLoading` is given to a yield.
pub const PACE_NOW_LOADING_WIPE: Pace = Pace::TweenMeasured("NowLoading::PlayFadeFrontCanvas/5 -> void(float, float, float, class<System.Action>, struct<DG.Tweening.Ease:4B>) (introspect.log:18317); NowLoading::SetupCrossFadeAsync/1 and PlayCrossFadeAsync/1 (18319-18320) are IEnumerator(Action) with no duration argument");

// The result parts' fades are tweeners the class holds in a list and can skip or complete: `field
// _fadeInSequenceList [generic<System.Collections.Generic.List<DG.Tweening.Tweener>:24B>] (24272), next to
// `SkipFadeInTween/0` (24264) and `CompleteFadeInTween/1 -> void(bool)` (24265) - the skip path this fork's
// auto skip option already drives.
pub const PACE_RESULT_CONTENT_FADE: Pace = Pace::TweenMeasured("SingleModeResultContentBase::field _fadeInSequenceList [generic<System.Collections.Generic.List<DG.Tweening.Tweener>:24B>] (introspect.log:24272) with SkipFadeInTween/0 (24264) and CompleteFadeInTween/1 -> void(bool) (24265)");

// The grand result's own fade door. `field _fadeInCircle [class<DG.Tweening.Tweener>]` and `field _skipDelay
// [class<DG.Tweening.Tween>]` (26077-26078) are the tween handles in the class the door belongs to.
pub const PACE_GRAND_RESULT_FADE: Pace = Pace::TweenMeasured("TeamStadiumGrandResultViewController::field _fadeInCircle [class<DG.Tweening.Tweener>] and _skipDelay [class<DG.Tweening.Tween>] (introspect.log:26077-26078)");

// The text modifier's lengths are entries in a tween timeline: `SetTimelineData/1 -> void(class<Gallop.
// TweenAnimationTimelineData>)` (15946) stores `field _timelineData` (15989), whose keys carry `field Duration
// [public float]` / `field Delay [public float]` (26535-26536), and the class holds a tween handle, `field _tweener
// [class<DG.Tweening.Tweener>]` (16000), built by `GetTweener/0 -> class<DG.Tweening.Tweener>()` (15972) with its
// compiler closure `<GetTweener>b__52_2/0 -> void()` (15985). A tweener is advanced on the channel `tween_clocks`
// multiplies.
//
// The same block names a second clock, and it is the half the first classification did not read. `LateUpdate/0`
// (15979) and `UpdateTime/0` (15980) are this component's own per frame step, and the class holds the state such a
// step runs on: `field _totalTime` (15991), `_internalTime` (15992), `_lastInternalTime` (15993), `_frameCount`
// (15995), beside `field <TimeScale>k__BackingField` (16004) with `get_TimeScale/0`+`set_TimeScale/1` (15961-15962),
// a scale the component keeps to itself. The clock the bound prices is `ui_animation_scale` over the whole
// `Time.timeScale` Unity is holding, so a component stepping its own time is on it only if that step reads
// `Time.deltaTime`, and no signature dump says whether it does. Nothing in the block says which of the two halves
// advances the timeline whose length `get_Duration/0` (15952) reports, so the lane is not shown and the door is
// `Unproven`. That is the bounded side and it hands the game the same number the claimed lane did, because
// `duration_factor_at` prices `TweenMeasured` and `Unproven` on one branch; only the claim comes off.
//
// Two readings from the same block are owed a run before these doors are settled. The block dumps no duration
// field for the class, while a setter for the number the getter door scales sits on the adapter it holds,
// `get_TimelineAdapter/0 -> class<Gallop.TimelineDataAdapter>()` (15960) over `Gallop.TimelineDataAdapter::
// set_Duration/1 -> void(float)` (16009) at its `field <Duration>k__BackingField` (16011), which is the read,
// modify, write shape AGENTS section 5 refuses; absence in a truncated dump proves nothing (A8). No run has read
// either door (installed is not called, A4).
pub const PACE_TEXT_MODIFIER_TIMELINE: Pace = Pace::Unproven;

// The gauge blend time keeps the lane item 59 left it on: no lever behind it, `Unproven`, and so the bounded side
// of the pair, because a progress bar blend is a tween duration and nothing has measured it off the clock.
pub const PACE_TRAINING_GATE: Pace = Pace::Unproven;

// The plate interval is the one training duration with readings on it. Three runs closed a plate cascade at or
// above the interval the door was handed and never at a fraction of it, which is what a completion advanced by the
// clock this fork multiplies would look like: `InitializePlateList 1 -> 0.05` with `9 cascades closed mean 338.2
// ms` (run 31), `1 -> 0.25` with `8 cascades closed mean 396.9 ms` (run 32), and the interval handed on unchanged
// at 1.0 s with `10 cascades closed mean 1242.1 ms` while `ui_animation 20` held a 20x delta clock (run 33). A
// 1.0 s wait advanced by that clock closes in 50 ms, and it closed in 1242 ms. So the interval is the beat the
// cascade waits on, which is what C54 read backwards from the fast end and C62 now states, and it is not advanced
// by the multiplied clock, so there is no pair on it to bound and it carries its own lever.
pub const PACE_PLATE_INTERVAL: Pace = Pace::OffTweenClock("run 31 closed a plate cascade in 338.2 ms on the 0.05 s the door handed, run 32 in 396.9 ms on 0.25 s, run 33 in 1242.1 ms with the 1.0 s interval handed on unchanged while ui_animation 20 held a 20x delta clock; none of them at a fraction of the interval, which is what a completion the multiplied clock advances would read like");

// The count-up door is the one `Group::Screens` duration door a career session has been measured reaching
// (`CountupModifier_getDuration 0.16 -> 0.008`, run 17, in the same snapshot as `result 20`), so it is the
// door item 62's bound exists to hold on, and its pace is `Unproven`: the dump names count-up consumers on
// both channels, so no reading puts it on one of them.
//
// The line first read as proof, `Gallop.PartsFanRaidEventResult::StartCoutup/1 -> IEnumerator(class<
// CountupModifier>)` (`introspect.log:16650`), takes a modifier and says nothing about what steps the
// count-up, and the class that owns it builds its animations as DOTween work: `BuildPlayInAnimation/0`,
// `BuildPlayOutAnimation/1 -> void(class<System.Action>)`, `PlayNextAnimation/0` and `CreateAnimationQueueAction/1
// -> class<System.Action>(class<System.Action>)` (16636-16639) over `_showItemSequence [class<DG.Tweening.
// Sequence>]` and `_animationQueue [generic<System.Collections.Generic.Queue<System.Action>:32B>]`
// (16687-16688). `Gallop.CountupModifier` itself (15899-15941) carries no coroutine anywhere: `OnPlay/0`,
// `OnComplete/0`, `OnUpdateText/1 -> void(float)` and `GetProgress/2 -> float(int, float)` (15918-15921) over
// `_valueCurve [class<UnityEngine.AnimationCurve>]` and `_countupDuration`. Consumers sit on both channels:
// `Gallop.TextCountUpVertexCommon` (15873-15893) draws a count-up through `_tween [class<DG.Tweening.Tween>]`
// and `_timeLine [class<Gallop.TweenAnimationTimelineComponent>]`, `Gallop.PartsSingleModeResultFanRaid::
// CountUpTextFadeInFromRight/7` (24196) feeds one into a result screen tween beside `AnimationDelayFunc/2 ->
// void(float, Action)` (24197) and the `countUpDelay` its closures capture (24189-24192), while `Gallop.
// PartsFanRaidFanNumCounter`'s `CountUp/2 -> IEnumerator(long, bool)` (16707) and its `MoveNext/0` (16693)
// step one through a coroutine. No signature dump says which channel the 0.16 s this door hands lands on.
//
// So the door is bounded, and what it owes before it is not is a completion wall time, not a longer look at
// the dump: `CountupModifier::OnPlay/0` and `OnComplete/0` (15918-15919) bracket one count-up on the same
// instance this getter is called on, so that pair gives the wall time of the completion the door paced, and
// `measured_pace(handed, wall, ui)` says which channel it ran on. That bracket is armed in the shipped tree
// (`CountupModifier_OnPlay`, `CountupModifier_OnComplete`), it feeds `countup_pace`, and `countup_pace` is what
// this door scales with, so a verdict a run reads moves the door and a verdict nobody has read leaves it here.
// Until one is read the door stays bounded, and with it the 8 ms completion rather than the 0.4 ms the ledger
// row prints as a fortieth of a frame. The wrapper is what the armed hook and the tests both call, so a door
// put back on an untrimmed scale fails a test rather than only moving a line.
pub const PACE_COUNTUP: Pace = Pace::Unproven;

/// The door's name in `ARMED_DOOR_PACES` and in `hachimi.log`. One spelling so the census row, the lane the log
/// prints and the lane this door scales on cannot be three different facts.
pub const COUNTUP_DOOR: &str = "CountupModifier.get_Duration";

/// Every duration door this fork arms, and the pace its bound reaches it on. `note_pair_ceiling` prints this
/// table door by door and `every_armed_duration_door_states_the_pace_its_bound_reaches_it_on` asserts against
/// it, so the lanes a career run reads in `hachimi.log` and the lanes the shipped code trims on are one fact
/// rather than two that can drift apart. A door that is armed and not listed here is a door no bound reaches.
///
/// A row on a bounded pace is a completion measured on the game's `Time.deltaTime`, so the clock that bound
/// prices at that door is `delta_clock()`: `ui_animation_scale` capped by what is left under
/// `MAX_TWEEN_SPEED_PRODUCT` once the whole scale the `Time.timeScale` write layer left in the game is
/// counted, times that scale. A row on `OffTweenClock` is a completion that clock is measured not to reach, and
/// no lever of this module's touches it. Both are named on the line a run reads, next to the lever the 20x does
/// not cover.
pub const ARMED_DOOR_PACES: &[(&str, Group, Pace)] = &[
    ("NowLoading.PlayFadeNowLoading", Group::Transition, PACE_NOW_LOADING_WIPE),
    ("NowLoading.PlayInNowLoading", Group::Transition, PACE_NOW_LOADING_WIPE),
    ("NowLoading.PlayOutNowLoading", Group::Transition, PACE_NOW_LOADING_WIPE),
    ("SingleModeResultContentBase.FadeInContent", Group::Screens, PACE_RESULT_CONTENT_FADE),
    ("SingleModeResultContentBase.FadeInContentFromRight", Group::Screens, PACE_RESULT_CONTENT_FADE),
    ("SingleModeResultContentBase.FadeInContentFromBottom", Group::Screens, PACE_RESULT_CONTENT_FADE),
    ("TeamStadiumGrandResultViewController.FadeInContentFromRight", Group::Screens, PACE_GRAND_RESULT_FADE),
    ("TextModifier.get_Duration", Group::Screens, PACE_TEXT_MODIFIER_TIMELINE),
    ("TextModifier.get_Delay", Group::Screens, PACE_TEXT_MODIFIER_TIMELINE),
    (COUNTUP_DOOR, Group::Screens, PACE_COUNTUP),
    ("TrainingParamChangeUI.InitializePlateList", Group::Training, PACE_PLATE_INTERVAL),
    ("SingleModeMainViewHpGauge.SetProgressbarBlendTime", Group::Training, PACE_TRAINING_GATE),
];

/// What the armed count-up door hands the game: the value it received, on the pace this door currently stands
/// on. `countup_pace` is `PACE_COUNTUP` - the bounded one - until this file's own bracket has read a completion
/// wall time that says otherwise, so the shipped state is the bounded one and a door leaving it is a door a run
/// measured (`note_countup_completion` is the only writer of that lane).
pub fn countup_screen_duration(value: f32, group: Group) -> f32 {
    let scaled = match countup_pace() {
        Pace::OffTweenClock(proof) => measured_off_clock_duration(value, group, proof),
        pace => scale_paced(value, group, pace),
    };

    note_countup_handed(value, scaled);

    scaled
}

def_getter_hook!(CountupModifier_getDuration, Group::Screens, countup_screen_duration, 8, no_answer { 0.0 });

// The reading the count-up door owes (see `PACE_COUNTUP`): one completion's wall time, read in a game run, and
// the only thing that can move this door off the pair bound. `Gallop.CountupModifier` dumps `OnPlay/0 -> void()`
// and `OnComplete/0 -> void()` (`introspect.log:15918-15919`) - the two ends of one count-up on the instance
// `get_Duration` is called on - so bracketing them gives the wall time of the completion this door paced, and
// `measured_pace` is the arithmetic that says what that wall time means. Both wrappers are observe only: each
// hands the game its own call, the barrier's `Panicked` answer owes it too, and nothing here writes a value or
// reaches a duration, so measuring a completion cannot shorten or lengthen the completion it measures.
//
// The lane starts where `PACE_COUNTUP` says it starts and moves on a verdict, never on a reading of a
// signature. `COUNTUP_MEASURED_MIN_SAMPLES` is why one completion is not enough: the dump names count-up
// consumers on both channels, so one count-up on one instance is a sample of one consumer, not of this door.
const COUNTUP_LANE_UNPROVEN: u8 = 0;
const COUNTUP_LANE_TWEEN_MEASURED: u8 = 1;
const COUNTUP_LANE_OFF_CLOCK: u8 = 2;
const COUNTUP_MEASURED_MIN_SAMPLES: usize = 2;

// The proof a measured lane carries. `Pace` carries a `&'static str`, so the numbers a run read go on the log
// line and these say what the numbers meant.
const COUNTUP_MEASURED_TWEEN_PROOF: &str = "measured: the count-up bracket closed a completion at about the duration the door handed it divided by the delta clock, the ui cap over the Time.timeScale this fork's write layer left in the game";
const COUNTUP_MEASURED_OFF_CLOCK_PROOF: &str = "measured: the count-up bracket closed a completion at about the duration the door handed it while ui_animation_scale was binding, the channel that lever does not reach";

// Four open brackets: a result screen plays several count-ups at once, and a bracket that only ever holds one
// instance closes them all as unmatched, which is the same blind spot the door it was written to prove out has.
// Fixed-size atomics, so the bracket costs no allocation and no lock (AGENTS section 6).
const COUNTUP_BRACKET_SLOTS: usize = 4;
const COUNTUP_COMPLETION_DETAIL_LIMIT: usize = 4;
const COUNTUP_COMPLETION_CHUNK: usize = 512;
const MS_PER_SECOND: f32 = 1000.0;
const NS_PER_SECOND: f32 = 1_000_000_000.0;

static COUNTUP_LANE: AtomicU8 = AtomicU8::new(COUNTUP_LANE_UNPROVEN);
static COUNTUP_RAW: AtomicU32 = AtomicU32::new(0);
static COUNTUP_HANDED: AtomicU32 = AtomicU32::new(0);
static COUNTUP_OPEN_INSTANCE: [AtomicPtr<Il2CppObject>; COUNTUP_BRACKET_SLOTS] = [const { AtomicPtr::new(std::ptr::null_mut()) }; COUNTUP_BRACKET_SLOTS];
static COUNTUP_OPEN_AT_NS: [AtomicU64; COUNTUP_BRACKET_SLOTS] = [const { AtomicU64::new(0) }; COUNTUP_BRACKET_SLOTS];
static COUNTUP_OPEN_HANDED: [AtomicU32; COUNTUP_BRACKET_SLOTS] = [const { AtomicU32::new(0) }; COUNTUP_BRACKET_SLOTS];
static COUNTUP_OPEN_RAW: [AtomicU32; COUNTUP_BRACKET_SLOTS] = [const { AtomicU32::new(0) }; COUNTUP_BRACKET_SLOTS];
static COUNTUP_PLAYS: AtomicUsize = AtomicUsize::new(0);
static COUNTUP_COMPLETIONS: AtomicUsize = AtomicUsize::new(0);
static COUNTUP_UNMATCHED: AtomicUsize = AtomicUsize::new(0);
static COUNTUP_OFF_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static COUNTUP_TWEEN_SAMPLES: AtomicUsize = AtomicUsize::new(0);
static COUNTUP_BRACKET_START: OnceLock<Instant> = OnceLock::new();

/// The door's live pace: the dump-backed default, or the lane a run's bracket measured. This is what
/// `countup_screen_duration` scales with and what `armed_door_lanes` prints, so the lane the log names and the
/// lane the bound reaches are read from one place.
pub fn countup_pace() -> Pace {
    match COUNTUP_LANE.load(Ordering::Acquire) {
        COUNTUP_LANE_TWEEN_MEASURED => Pace::TweenMeasured(COUNTUP_MEASURED_TWEEN_PROOF),
        COUNTUP_LANE_OFF_CLOCK => Pace::OffTweenClock(COUNTUP_MEASURED_OFF_CLOCK_PROOF),
        _ => PACE_COUNTUP,
    }
}

/// The pace `ARMED_DOOR_PACES` lists for one door, read as the door actually stands on it. Only the count-up
/// door has a lane a run can move; every other row is the dump line written at it.
fn armed_door_pace(door: &str, listed: Pace) -> Pace {
    if door == COUNTUP_DOOR { countup_pace() } else { listed }
}

/// What the armed door handed the game on its last call, and what the game handed it. `CountupModifier` reads
/// one duration through this one door, so these are the numbers the door's own hit line prints and the bracket
/// prices a completion against. Two relaxed stores on a door the runs reach on result screens, not a frame path.
pub fn note_countup_handed(raw: f32, handed: f32) {
    COUNTUP_RAW.store(raw.to_bits(), Ordering::Release);
    COUNTUP_HANDED.store(handed.to_bits(), Ordering::Release);
}

/// The counts a run reads: completions closed on a bracket this door opened, and closures that found no open
/// bracket on their instance. A bracket armed and reached is a different fact from a bracket armed and matched
/// (A4), and the second one is what a verdict needs.
pub fn countup_bracket_counts() -> (usize, usize) {
    (COUNTUP_COMPLETIONS.load(Ordering::Relaxed), COUNTUP_UNMATCHED.load(Ordering::Relaxed))
}

fn countup_now_ns() -> u64 {
    match COUNTUP_BRACKET_START.get() {
        Some(start) => start.elapsed().as_nanos() as u64,
        None => 0,
    }
}

/// `CountupModifier::OnPlay/0`: open a bracket on this instance, carrying the duration the door last handed.
/// `now_ns` is an argument so the pairing is a testable thing; the wrapper takes it from the process clock, and
/// a bracket with no clock behind it (no install, so no `COUNTUP_BRACKET_START`) opens nothing.
pub fn countup_play(this: *mut Il2CppObject, now_ns: u64) -> bool {
    let handed = f32::from_bits(COUNTUP_HANDED.load(Ordering::Acquire));

    // A door that never ran, or one that handed the game a zero, has no duration to price a completion with.
    if this.is_null() || now_ns == 0 || !handed.is_finite() || handed <= 0.0 {
        return false;
    }

    let plays = COUNTUP_PLAYS.fetch_add(1, Ordering::Relaxed);
    let free = COUNTUP_OPEN_AT_NS.iter().position(|slot| slot.load(Ordering::Acquire) == 0);
    let slot = free.unwrap_or(plays % COUNTUP_BRACKET_SLOTS);

    COUNTUP_OPEN_INSTANCE[slot].store(this, Ordering::Release);
    COUNTUP_OPEN_HANDED[slot].store(handed.to_bits(), Ordering::Release);
    COUNTUP_OPEN_RAW[slot].store(f32::from_bits(COUNTUP_RAW.load(Ordering::Acquire)).to_bits(), Ordering::Release);
    // Last, because a non zero time is what says this slot is open. Written first, a completion could read a
    // half written bracket.
    COUNTUP_OPEN_AT_NS[slot].store(now_ns, Ordering::Release);

    true
}

/// `CountupModifier::OnComplete/0`: close the bracket on this instance and ask `measured_pace` what the pair
/// says. `None` means the closure found no open bracket, or the reading says nothing, and both leave the door
/// on the bounded side it started on.
pub fn countup_complete(this: *mut Il2CppObject, now_ns: u64) -> Option<Pace> {
    let slot = if this.is_null() {
        None
    } else {
        COUNTUP_OPEN_INSTANCE
            .iter()
            .enumerate()
            .find(|(_, instance)| instance.load(Ordering::Acquire) == this)
            .map(|(index, _)| index)
            .filter(|index| COUNTUP_OPEN_AT_NS[*index].load(Ordering::Acquire) != 0)
    };

    let Some(index) = slot else {
        COUNTUP_UNMATCHED.fetch_add(1, Ordering::Relaxed);
        return None;
    };

    let opened_at = COUNTUP_OPEN_AT_NS[index].load(Ordering::Acquire);
    let handed = f32::from_bits(COUNTUP_OPEN_HANDED[index].load(Ordering::Acquire));
    let raw = f32::from_bits(COUNTUP_OPEN_RAW[index].load(Ordering::Acquire));

    COUNTUP_OPEN_AT_NS[index].store(0, Ordering::Release);
    COUNTUP_OPEN_INSTANCE[index].store(std::ptr::null_mut(), Ordering::Release);

    let wall = (now_ns.saturating_sub(opened_at)) as f32 / NS_PER_SECOND;

    note_countup_completion(handed, raw, wall, delta_clock())
}

/// The verdict line, and the only writer of `COUNTUP_LANE`. Every door that leaves the pair bound got there
/// through this function, which is what makes the bound provable rather than permanent. `clock` is
/// `delta_clock()`, the multiplier the delta channel carried - the ui cap over the whole scale the
/// `Time.timeScale` write layer left in the game - because that is the speed a completion on that channel ran
/// at.
pub fn note_countup_completion(handed: f32, raw: f32, wall: f32, clock: f32) -> Option<Pace> {
    let calls = COUNTUP_COMPLETIONS.fetch_add(1, Ordering::Relaxed) + 1;
    let verdict = measured_pace(handed, wall, clock);

    // The fork's usual first N then every N: a completion is not a frame path, but a session with a lot of
    // count-ups is not a reason to write a line per one either.
    if calls <= COUNTUP_COMPLETION_DETAIL_LIMIT || calls % COUNTUP_COMPLETION_CHUNK == 0 {
        debug!(
            "AnimationSpeed: count-up completion {calls}: the door handed {handed} s and the same instance closed {} ms later at a {}x delta clock (ui_animation {}, Time.timeScale {} from this fork's write layer, {}x of it the time_scale lever added) -> {}; {} closures with no open bracket on that instance",
            wall * MS_PER_SECOND,
            clock,
            ui_animation_scale(),
            time_scale_produced(),
            time_scale_raise(),
            verdict_word(verdict),
            COUNTUP_UNMATCHED.load(Ordering::Relaxed),
        );
    }

    if let Some(pace) = verdict {
        countup_lane_note(pace, raw, handed, wall, clock);
    }

    verdict
}

fn verdict_word(verdict: Option<Pace>) -> &'static str {
    match verdict {
        // The same words `armed_door_lanes` prints, so a completion line and the lane line name one thing.
        Some(pace) => pace_lane(pace),
        None => "nothing, the door stays unproven and bounded",
    }
}

fn countup_lane_note(pace: Pace, raw: f32, handed: f32, wall: f32, clock: f32) {
    match pace {
        Pace::OffTweenClock(_) => { COUNTUP_OFF_SAMPLES.fetch_add(1, Ordering::Relaxed); },
        Pace::TweenMeasured(_) => { COUNTUP_TWEEN_SAMPLES.fetch_add(1, Ordering::Relaxed); },
        Pace::Unproven => return,
    }

    let off = COUNTUP_OFF_SAMPLES.load(Ordering::Relaxed);
    let tween = COUNTUP_TWEEN_SAMPLES.load(Ordering::Relaxed);

    // A reading has to repeat before it moves a door, and a run that answers both ways has named a count-up
    // consumer rather than this door: the dump puts count-ups on both channels. A contradicted reading leaves
    // the door on the bounded side, which is the side that cannot re-open the 400x state item 62 was written
    // to stop.
    let wanted = if off >= COUNTUP_MEASURED_MIN_SAMPLES && tween == 0 {
        COUNTUP_LANE_OFF_CLOCK
    } else if tween >= COUNTUP_MEASURED_MIN_SAMPLES && off == 0 {
        COUNTUP_LANE_TWEEN_MEASURED
    } else {
        return;
    };

    if COUNTUP_LANE.swap(wanted, Ordering::AcqRel) == wanted {
        return;
    }

    // `countup_screen_duration` after the swap: the number the door hands from here, read through the door
    // itself rather than asserted about it.
    let now_hands = countup_screen_duration(raw, Group::Screens);
    let completion = match countup_pace() {
        Pace::OffTweenClock(_) => now_hands * MS_PER_SECOND,
        _ => now_hands / clock * MS_PER_SECOND,
    };

    info!(
        "AnimationSpeed: {COUNTUP_DOOR} moves to the {} lane on {off} off clock and {tween} clock completions: the door handed {handed} s and the same instance closed {} ms later at a {}x delta clock (ui_animation {}, Time.timeScale {} from this fork's write layer, {}x of it the time_scale lever added); the door hands {now_hands} s from here and that completion runs {completion} ms",
        pace_lane(countup_pace()),
        wall * MS_PER_SECOND,
        clock,
        ui_animation_scale(),
        time_scale_produced(),
        time_scale_raise(),
    );
}

/// The word one bracket door deserves on the install line: resolved and armed, or there with nothing this
/// wrapper can stand on. A bracket with one end missing measures nothing, and a run has to be able to see that
/// rather than infer it from a completion line that never comes (A4).
pub const fn bracket_door_state(addr: usize) -> &'static str {
    if addr == 0 { "is there with no method this wrapper can stand on" } else { "armed" }
}

/// The lane a door moves to when a run has measured its completion off the multiplied clock: the group's whole
/// factor, never trimmed. A door comes here through `measured_pace`, carrying the reading that put it here in
/// `proof`, and `countup_screen_duration` is the armed door that routes through it once its bracket has read
/// that verdict twice. `a_door_the_tween_clock_never_reaches_keeps_its_groups_whole_factor` drives the same
/// lane through the macro the installed hooks are built by.
pub fn measured_off_clock_duration(value: f32, group: Group, proof: &'static str) -> f32 {
    scale_paced(value, group, Pace::OffTweenClock(proof))
}

pub fn text_modifier_timeline_duration(value: f32, group: Group) -> f32 { scale_paced(value, group, PACE_TEXT_MODIFIER_TIMELINE) }

def_getter_hook!(TextModifier_getDuration, Group::Screens, text_modifier_timeline_duration, 9, no_answer { 0.0 });
def_getter_hook!(TextModifier_getDelay, Group::Screens, text_modifier_timeline_duration, 10, no_answer { 0.0 });
def_getter_hook!(StoryTimeline_getTimeScaleEventWipe, Group::Story, scale_time_scale, 14, no_answer { MIN_TIME_SCALE });
def_getter_hook!(StoryTimeline_getTimeScaleAfterEndStory, Group::Story, scale_time_scale, 15, no_answer { MIN_TIME_SCALE });

// The wrappers the barrier tests below drive, built by `def_getter_hook!` itself through the arm the
// shipped arm delegates to. Slots 17 and 18 are test slots (the installed hooks use 6 through 16).
//
// The game half is one of these two functions rather than a trampoline: a unit test reaches no
// trampoline and no `Hachimi::instance()` (AGENTS section 4), and the shipped body's registry lookup
// ends the test process on its cold branch.
#[cfg(test)]
#[inline(never)]
extern "C" fn game_getter_that_answers_2_4(_this: *mut Il2CppObject) -> f32 {
    2.4
}

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
#[inline(never)]
extern "C" fn game_getter_that_faults(_this: *mut Il2CppObject) -> f32 {
    // The read `guard` already uses for this purpose: an address no module owns, taken inside the
    // frames the barrier wrapped.
    let mut value: u64 = 0;
    unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
    value as f32
}

#[cfg(test)]
def_getter_hook!(
    GetterWithNoOriginalForADuration, Group::Screens, scale_duration, 17,
    no_answer { 0.0 },
    orig { None::<extern "C" fn(*mut Il2CppObject) -> f32> },
    after_call {}
);

#[cfg(test)]
def_getter_hook!(
    GetterWithNoOriginalForATimeScale, Group::Story, scale_time_scale, 17,
    no_answer { MIN_TIME_SCALE },
    orig { None::<extern "C" fn(*mut Il2CppObject) -> f32> },
    after_call {}
);

#[cfg(test)]
def_getter_hook!(
    GetterThatScalesCleanly, Group::Screens, scale_duration, 17,
    no_answer { 0.0 },
    orig { Some(game_getter_that_answers_2_4) },
    after_call {}
);

// The count-up door as the armed one is built: `Group::Screens`, the scale half the installed hook calls,
// and the 0.16 s the runs read out of the client coming back from the game half. A test that only read
// `scale_duration` would still pass if this door were put back onto a scale that steps out of the pair trim;
// this one reads the same wrapper the armed detour calls.
#[cfg(test)]
#[inline(never)]
extern "C" fn game_getter_that_answers_the_shipped_countup_length(_this: *mut Il2CppObject) -> f32 {
    0.16
}

#[cfg(test)]
def_getter_hook!(
    CountupDoorOnThePairTrim, Group::Screens, countup_screen_duration, 17,
    no_answer { 0.0 },
    orig { Some(game_getter_that_answers_the_shipped_countup_length) },
    after_call {}
);

// The same door on the lane a measured completion moves a door to: the same `Group::Screens`, the same 0.16 s
// answering out of the game half, and a scale half that takes the group's whole factor because the completion
// it paces is not measured on the clock the pair bound prices. Built by the same macro the installed hooks use,
// so the off clock lane is a door a test can drive rather than a formula a test copies.
#[cfg(test)]
fn wait_paced_countup(value: f32, group: Group) -> f32 {
    measured_off_clock_duration(value, group, "test door: this 0.16 s paces a Unity coroutine yield, the channel WaitProbe's two doors stand on (run 18 armed 7 waits for 6,150 ms and ui_animation_scale reached none of them)")
}

#[cfg(test)]
def_getter_hook!(
    CountupDoorOffTheTweenClock, Group::Screens, wait_paced_countup, 17,
    no_answer { 0.0 },
    orig { Some(game_getter_that_answers_the_shipped_countup_length) },
    after_call {}
);

#[cfg(test)]
#[inline(never)]
fn mod_half_that_panics_after_the_answer() {
    // A function rather than a `panic!` written in the body: the wrapper's own half is what trips
    // here, and a diverging call the compiler cannot see through is what keeps the scaling half it
    // stands in front of a real statement instead of dead code.
    panic!("the mod half of a getter tripped after the game had answered");
}

#[cfg(test)]
def_getter_hook!(
    GetterPanickingAfterTheGameAnswered, Group::Screens, scale_duration, 17,
    no_answer { 0.0 },
    orig { Some(game_getter_that_answers_2_4) },
    after_call { mod_half_that_panics_after_the_answer() }
);

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
def_getter_hook!(
    GetterFaultingAfterTheGameAnswered, Group::Story, scale_time_scale, 18,
    no_answer { MIN_TIME_SCALE },
    orig { Some(game_getter_that_answers_2_4) },
    after_call {
        let mut value: u64 = 0;
        unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
        let _ = value;
    }
);

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
def_getter_hook!(
    GetterFaultingInsideTheOriginal, Group::Story, scale_time_scale, 18,
    no_answer { MIN_TIME_SCALE },
    orig { Some(game_getter_that_faults) },
    after_call {}
);

// `SingleModeMainViewTrainingCutStatus.PlayIn/4` is not hooked any more, and this is the note that says why
// (run 26, quoted at `TRAINING_HIT_SLOTS`): the float in position 0 is the play in *duration* the caller sets
// up, while the animation itself runs on `CoroutinePlayIn/2 -> IEnumerator(float, int)` next door, and run 26
// measured 7866, 8182 and 7866 ms of status panel held off in the same session that read `PlayIn 2.4 -> 0.12`.
//
// The two doors below are the training gates this fork still arms, and since item 59 they are armed to be
// read, not to be scaled: both sit on the training turn's own presentation, `Group::Training` has no factor
// behind it, and each hands the game the float it was passed.
//
// The HP gauge progress bar blend time, the only float the gauge class takes. Run 11 measured 63,731 frames on
// the screen this class draws and never measured one animation door of it - the census has read `=0` in every
// run - so scaling it bought nothing and cost the training screen one more number a result screen slider moved.
type HpGaugeSetBlendTimeFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
def_detour! {
    HpGauge_SetBlendTimeSpeed(this: *mut Il2CppObject, time: f32) {
            let scaled = training_gate_duration(time);
        hit(11, "SingleModeMainViewHpGauge.SetProgressbarBlendTime", time, scaled);

        get_orig_fn!(HpGauge_SetBlendTimeSpeed, HpGaugeSetBlendTimeFn)(this, scaled);
    }
}

/// What the gauge blend door hands the game. This is item 59 in one call: the door scales on `Group::Training`,
/// and that group has no factor, so the value is the one the caller passed. The wrapper exists so the tests drive
/// the same decision the armed detours make - a test that only reads `Group::Training` would still pass if a door
/// went back to `Group::Screens`, which is the regression this item exists to keep out. Its pace is written down
/// too (PACE_TRAINING_GATE) so the day a lever is ever put on this door, the lane the bound reaches it on is
/// already stated. It is deliberately not the plate door's path: a blend time is a tween duration, and the plate
/// interval has readings saying the multiplied clock does not reach it.
pub fn training_gate_duration(value: f32) -> f32 { scale_paced(value, Group::Training, PACE_TRAINING_GATE) }

/// What the plate lever takes out of a cascade interval. Its own mirror, its own ceiling (`normalize` holds it at
/// `MAX_FACTOR`), and no share of the pair bound, because the completion it prices is one the runs above measured
/// the multiplied clock not to advance. At the shipped 1.0 the door hands the caller's interval on unchanged, which
/// is the state item 59 left the door in and the state `Config::default()` ships.
pub fn plate_interval_duration(value: f32) -> f32 {
    let factor = plate_factor();

    if factor == 1.0 || !value.is_finite() || value == 0.0 {
        return value;
    }

    value / factor
}

// The plate cascade interval floor. This door is the only duration write this fork reaches on the
// training screen that run 31 actually hit: its one hit line took the game's 1.0 s to 0.05 s
// (`run log/hachimi-run31.log` line 1184) and its census measured 9 cascades closing in 338.2 ms mean (that
// log line 2505), and that session ended stuck on `SingleModeSuccessionEvent`
// (view 1501) with `CutInTimelineController::UpdateSpeed()` still called every frame while every door that
// advances a flow had frozen 3.5 s earlier. The literals that pace the cut-in around the cascade
// (`INSPIRATION_TYPEWRITE_DELAY`, `INSPIRATION_MINI_MODEL_MOTION_WAIT`, `INSPIRATION_WIPE_TIME_SCALE`) are
// folded into the game's call sites and this fork cannot reach them, so a cascade 20x faster than the beat
// the cut-in is built on is a schedule the animation was not made for. 0.25 s is a quarter of the game's own
// spacing: it keeps 4x of the 20x, and it is the value C62 tests against a career end run rather than a
// number measured to be safe.
//
// The lever `training_plate_speed` reaches this door, so the floor is what the fast arm lands on rather than a
// guard nothing reaches: at the lever's `MAX_FACTOR` ceiling a 1.0 s interval hands 0.05 s, the value run 31 read
// before any floor existed, in the session that ended stuck on the career end event.
pub const MIN_PLATE_INTERVAL_SEC: f32 = 0.25;

/// The interval to hand `InitializePlateList`, with the floor applied and the game's own value as the
/// ceiling. The floor may not raise a number above what the caller passed: making a cascade the game asked
/// to run in 0.1 s take 0.25 s would be a second bug on the same door. A zero or non finite `scaled` means
/// the lever already declined to touch the value, so it stays declined. With the lever at its shipped 1.0
/// `scaled` is the caller's own interval and this is the identity, which is the state item 59 asked for.
pub fn plate_cascade_interval(interval: f32, scaled: f32) -> f32 {
    if !interval.is_finite() || !scaled.is_finite() || interval <= 0.0 || scaled <= 0.0 {
        return interval;
    }

    scaled.max(MIN_PLATE_INTERVAL_SEC).min(interval)
}

/// What the plate door hands `InitializePlateList`, in one call, on the mirrors as they stand: the plate lever
/// first, then C62's floor as the guard on the result. The armed detour calls this, so a test that calls it is
/// driving the same decision the hook makes in game.
pub fn plate_cascade_handoff(interval: f32) -> f32 { plate_cascade_interval(interval, plate_interval_duration(interval)) }

/// What the cut-in clock door hands back: the scale the game computed, raised by the lever, never lowered,
/// never past `MAX_TRAINING_CUT_TIME_SCALE`. The target is a static helper that takes a scale and returns one
/// (`introspect.log:24381`, `GetTrainingCutTimeScale/1 -> static float(float)`), and every pair a run read off it
/// has the answer above the question: `[3.04, 6.08]` and `[2.4, 4.8]` in run 9, 5.640 in to 11.280 out in run 11,
/// 6.080 peaked in runs 33 and 34 against 8.000 in run 31. So the number this door scales is the training cut's
/// own speed channel rather than one argument among several. At the shipped 1.0 it is the identity, which is the
/// state every recorded run measured with.
pub fn training_cut_time_scale(value: f32) -> f32 {
    let factor = training_cut_factor();

    if factor == 1.0 || !value.is_finite() || value < MIN_TIME_SCALE {
        return value;
    }

    // A scale only goes up (AGENTS section 5), and `value.max(...)` is what keeps the ceiling from turning into
    // a slow down: runs 9 and 11 read the game putting 8.334 and 11.280 on this door by itself, and a value the
    // game asked for above the cap arrives as it arrived rather than being trimmed to it. Below
    // `MIN_TIME_SCALE` the game chose a pause or a slow motion and it arrives as it arrived too.
    value.max((value * factor).min(MAX_TRAINING_CUT_TIME_SCALE))
}

// The training cut-in clock door. `GetTrainingCutTimeScale/1` is static, so the wrapper declares the dumped
// float and no `this` (A3), and it is reached 8 to 220 times a session on this client, never once a frame, so
// the scaling half pays one atomic load and a `hit` on the first four calls. The two doors that *read* the
// channel this one writes, `SingleModeTrainingCutInHelper::GetTargetSpeed` and `CutInTimelineController::
// UpdateSpeed`, stay off the lever: item 45 keeps them off limits until something is built beside their setters
// `SetSpeed/1` and `set_SkipFrame/1`, and scaling the source once is also the shape that cannot put the same
// factor on a value twice (C22, C24). `SetSpeed` and `set_SkipFrame` are themselves installed and silent - 29
// of the probe's 47 counted doors printed no call line in the run C49 records - so there is no write half to
// build beside them yet.
//
// What is not proven is what a raised channel buys, and that is the measurement this door exists to make. Run
// 25 measured a 7867 ms and an 11688 ms hole in which the cut-in had already finished (the timeline handing
// 2.4 to 2.4), and runs 33 and 34 closed a plate cascade in 1140 to 1242 ms on this door peaked at 6.080 while
// run 31 closed one in 338 ms at 8.000. So the run this lever owes reads `hit` here beside `Cutt probe plate
// cascade:` and `cut runs ... wall`, and says whether the wall moved with the scale or not at all.
type GetTrainingCutTimeScaleFn = extern "C" fn(scale: f32) -> f32;
def_detour! {
    SingleModeUtils_GetTrainingCutTimeScaleSpeed(scale: f32) answer -> f32 {
            let value = get_orig_fn!(SingleModeUtils_GetTrainingCutTimeScaleSpeed, GetTrainingCutTimeScaleFn)(scale);
        // The game's own scale is published before the scaling half runs: a 0 scale on this door is a training
        // cut that stops advancing, which is the machinery C62's career end stall lived in, and a trip in this
        // mod's own logging is not this mod's decision to freeze the cut.
        answer.publish(value);

        let scaled = training_cut_time_scale(value);
        hit(TRAINING_CUT_SLOT, TRAINING_CUT_DOOR, value, scaled);
        // The probe keeps its pair reading on this door and does not hook it a second time, so the census peak
        // stays the game's own scale while the line above says what the lever made of it.
        TrainingCuttProbe::note_cut_clock(scale, value);

        scaled
    }
}

/// The speed the tag cut-in lever writes onto an Animator, priced against the scale this layer last handed
/// Unity's `Time.timeScale` setter. An Animator advances on that scale, so the lever and the scale multiply on
/// one completion and `MAX_TWEEN_SPEED_PRODUCT` is the ceiling on the pair, the way C58 bounds the ui clock. At
/// the neutral lever this returns 1.0, and 1.0 means "write nothing" at both doors.
pub fn tag_cut_animator_speed() -> f32 {
    let lever = tag_cut_factor();

    if lever == 1.0 {
        return 1.0;
    }

    let produced = f32::from_bits(TIME_SCALE_PRODUCED.load(Ordering::Relaxed)).max(1.0);
    let pair_bound = (MAX_TWEEN_SPEED_PRODUCT / produced).max(1.0);

    lever.max(1.0).min(MAX_TAG_CUT_ANIMATOR_SPEED).min(pair_bound)
}

type AnimatorGetSpeedFn = extern "C" fn(animator: *mut Il2CppObject) -> f32;
type AnimatorSetSpeedFn = extern "C" fn(animator: *mut Il2CppObject, speed: f32);

/// Reads the speed the Animator is actually at and writes only a higher one, because this lever is raise-only
/// like the cut scale door: a cut-in Animator the game already put at 8.48 must not be pulled back to 4.0 by
/// this fork. The read is also the measurement the friendship cut owes, since it says what the game set on the
/// object the effect runs on. `None` is "nothing here to write on", which covers a null Animator and a door
/// whose `get_speed` / `set_speed` pair never resolved.
unsafe fn raise_animator_speed(animator: *mut Il2CppObject, target: f32) -> Option<(f32, bool)> {
    let get_addr = ANIMATOR_GET_SPEED_ADDR.load(Ordering::Relaxed);
    let set_addr = ANIMATOR_SET_SPEED_ADDR.load(Ordering::Relaxed);

    if animator.is_null() || get_addr == 0 || set_addr == 0 {
        return None;
    }

    let get_speed: AnimatorGetSpeedFn = std::mem::transmute(get_addr);
    let current = get_speed(animator);

    if !(current < target) {
        return Some((current, false));
    }

    let set_speed: AnimatorSetSpeedFn = std::mem::transmute(set_addr);
    set_speed(animator, target);

    Some((current, true))
}

/// The Animator held in one of the tag cut-in player's line animator fields. A field that did not resolve at
/// init reads as null here, and a null Animator is a no-op.
unsafe fn read_line_animator(this: *mut Il2CppObject, field: usize) -> *mut Il2CppObject {
    if this.is_null() || field == 0 {
        return std::ptr::null_mut();
    }

    il2cpp_field_get_value_object(field as *mut FieldInfo, this)
}

// Named as `TrainingCuttProbe` names these doors, so a run can match the write lines to the census counts.
pub const TAG_CUT_DOOR: &str = "SingleModeMainViewTagTrainingCutInPlayer.CreateLineEffect";
pub const TAG_CUT_LINE_DOOR: &str = "SingleModeMainViewTagTrainingCutInPlayer.PlayLineEffect";
const TAG_CUT_SLOT: usize = 14;
const TAG_CUT_LINE_SLOT: usize = 15;

// The first few writes on both doors are printed, then counted: at the neutral lever the line says so, with the
// lever up it names the speed the Animator was at and whether the lever raised it, and a door the game never
// reached prints nothing at all (A4).
fn note_tag_cut_write(door: &str, slot: usize, current: Option<(f32, bool)>, target: f32) {
    let (raw, wrote) = match current {
        Some((current, wrote)) => (current, wrote),
        None => (0.0, false),
    };
    let seen = TAG_CUT_WRITES_LOGGED.fetch_add(1, Ordering::Relaxed) + 1;

    if seen <= HIT_DETAIL_LIMIT {
        let tail = match current {
            Some((_, true)) => " raised",
            Some((_, false)) => " already at or above the lever",
            None => " no Animator in hand",
        };

        debug!("AnimationSpeed: {door} call {seen}: Animator {raw} -> {target}{tail}");
    }

    hit(slot, door, raw, if wrote { target } else { raw });
}

type TagCutCreateLineEffectFn = extern "C" fn(this: *mut Il2CppObject, transform: *mut Il2CppObject) -> *mut Il2CppObject;
// `CreateLineEffect/1 -> class<UnityEngine.Animator>(class<Transform>)` (`introspect.log:23805`): the door hands
// the line effect's Animator back, so the write goes on the object the game actually made rather than on a guess
// about which field the class plays with. Both parameters are references held as addresses and passed back, and
// the Animator returned is handed back untouched (A5, C48).
def_detour! {
    TagCutInPlayer_CreateLineEffectSpeed(this: *mut Il2CppObject, transform: *mut Il2CppObject) -> *mut Il2CppObject {
            let animator = get_orig_fn!(TagCutInPlayer_CreateLineEffectSpeed, TagCutCreateLineEffectFn)(this, transform);
        let speed = tag_cut_animator_speed();
        let at = unsafe { raise_animator_speed(animator, speed) };

        note_tag_cut_write(TAG_CUT_DOOR, TAG_CUT_SLOT, at, speed);

        animator
    }
    bail {
                get_orig_fn!(TagCutInPlayer_CreateLineEffectSpeed, TagCutCreateLineEffectFn)(this, transform)
    }
}

type TagCutPlayLineEffectFn = extern "C" fn(this: *mut Il2CppObject);
// `PlayLineEffect/0 -> void()` (`introspect.log:23809`), the door the class plays `_topLineAnimator` and
// `_bottomLineAnimator` through. The speed is an absolute value this fork holds rather than a multiple of what
// the Animator already had, so a cut that reaches this door and `CreateLineEffect` is not written twice (C22).
def_detour! {
    TagCutInPlayer_PlayLineEffectSpeed(this: *mut Il2CppObject) {
            let speed = tag_cut_animator_speed();

        get_orig_fn!(TagCutInPlayer_PlayLineEffectSpeed, TagCutPlayLineEffectFn)(this);

        let at = unsafe {
            let top = read_line_animator(this, TAG_CUT_TOP_ANIMATOR_FIELD.load(Ordering::Relaxed));
            let wrote = match raise_animator_speed(top, speed) {
                Some((_, wrote)) => wrote,
                None => false,
            };
            let bottom = read_line_animator(this, TAG_CUT_BOTTOM_ANIMATOR_FIELD.load(Ordering::Relaxed));

            match raise_animator_speed(bottom, speed) {
                Some((current, bottom_wrote)) => Some((current, wrote || bottom_wrote)),
                None => None,
            }
        };

        note_tag_cut_write(TAG_CUT_LINE_DOOR, TAG_CUT_LINE_SLOT, at, speed);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_PlayLineEffectSpeed, TagCutPlayLineEffectFn)(this)
    }
}

/// The speed the cut-in lever writes onto an `AnimateToUnity.AnMotion`, priced against the scale this layer last
/// handed Unity's `Time.timeScale` setter, the way C58 prices the ui clock and the tag cut-in Animator prices its
/// pair. At the neutral lever this returns 1.0, and 1.0 means "write nothing" at every door below.
pub fn flash_motion_speed() -> f32 {
    let lever = training_cut_factor();

    if lever == 1.0 {
        return 1.0;
    }

    // The slider is the rate, the ceiling is the channel's own and the pair bound is C58's, so raising the control
    // cannot push a motion past 10.0 or past what the scale this layer hands Unity's setter leaves room for.
    let produced = f32::from_bits(TIME_SCALE_PRODUCED.load(Ordering::Relaxed)).max(1.0);
    let pair_bound = (MAX_TWEEN_SPEED_PRODUCT / produced).max(1.0);

    lever.max(1.0).min(MAX_MOTION_SPEED).min(pair_bound)
}

type MotionGetSpeedFn = extern "C" fn(motion: *mut Il2CppObject) -> f32;
type MotionSetSpeedFn = extern "C" fn(motion: *mut Il2CppObject, speed: f32, to_children: bool);

/// Reads the speed the motion is actually at and writes only a higher one, so a motion the game already put at 8.0
/// is never pulled back to the lever's 4.0. `SetMotionSpeed/2 -> void(float, bool)` carries a second flag whose
/// meaning this fork has not measured, so the write passes `false` for it: the narrower reading, this motion only.
/// `None` is "nothing here to write on", which covers a null motion and a pair that never resolved.
unsafe fn raise_motion_speed(motion: *mut Il2CppObject, target: f32) -> Option<(f32, bool)> {
    let get_addr = MOTION_GET_SPEED_ADDR.load(Ordering::Relaxed);
    let set_addr = MOTION_SET_SPEED_ADDR.load(Ordering::Relaxed);

    if motion.is_null() || get_addr == 0 || set_addr == 0 {
        return None;
    }

    let get_speed: MotionGetSpeedFn = std::mem::transmute(get_addr);
    let current = get_speed(motion);

    if !(current < target) {
        return Some((current, false));
    }

    let set_speed: MotionSetSpeedFn = std::mem::transmute(set_addr);
    set_speed(motion, target, false);

    Some((current, true))
}

/// The motion a flash player plays through: `field _motion [class<AnimateToUnity.AnMotion>]`
/// (`introspect.log:26507`), read the same way the tag cut-in line animators are read. A field that did not resolve
/// at init reads as null here, and a null motion is a no-op.
unsafe fn read_flash_motion(this: *mut Il2CppObject) -> *mut Il2CppObject {
    read_line_animator(this, FLASH_MOTION_FIELD.load(Ordering::Relaxed))
}

fn note_motion_write(door: &str, slot: usize, at: Option<(f32, bool)>, target: f32) {
    let (raw, wrote) = match at {
        Some((current, wrote)) => (current, wrote),
        None => (0.0, false),
    };
    let seen = MOTION_SPEED_WRITES_LOGGED.fetch_add(1, Ordering::Relaxed) + 1;

    if seen <= HIT_DETAIL_LIMIT {
        let tail = match at {
            Some((_, true)) => " raised",
            Some((_, false)) => " already at or above the lever",
            None => " no motion in hand",
        };

        debug!("AnimationSpeed: {door} call {seen}: motion speed {raw} -> {target}{tail}");
    }

    hit(slot, door, raw, if wrote { target } else { raw });
}

// Named after the doors `TrainingCuttProbe` counted in run 44, so a run can match the write lines to those counts.
pub const FLASH_PLAY_INT_DOOR: &str = "Gallop.FlashPlayer.Play(label, action, int)";
pub const FLASH_PLAY_FLOAT_DOOR: &str = "Gallop.FlashPlayer.Play(label, action, float)";
pub const MOTION_SET_SPEED_DOOR: &str = "AnimateToUnity.AnMotion.SetMotionSpeed(speed, flag)";
const FLASH_PLAY_INT_SLOT: usize = 16;
const FLASH_PLAY_FLOAT_SLOT: usize = 17;
const MOTION_SET_SPEED_SLOT: usize = 18;

type FlashPlayerPlayIntFn = extern "C" fn(this: *mut Il2CppObject, label: *mut Il2CppString, action: *mut Il2CppObject, value: i32);
// `FlashPlayer::Play/3 -> void(string<System.String>, class<System.Action>, int)` (`introspect.log:28693`), reached
// 1926 times in run 44 on the labels `in00`, `in`, `loop` and `end` with an int of 0. The label names a motion label
// and the int is not a speed, so the lever writes on the motion the player holds rather than on an argument, after
// the game has started the play. The string and the Action travel as addresses and go back untouched (A5).
def_detour! {
    FlashPlayer_PlayLabelActionIntSpeed(this: *mut Il2CppObject, label: *mut Il2CppString, action: *mut Il2CppObject, value: i32) {
            get_orig_fn!(FlashPlayer_PlayLabelActionIntSpeed, FlashPlayerPlayIntFn)(this, label, action, value);
        let speed = flash_motion_speed();

        if speed == 1.0 {
            hit(FLASH_PLAY_INT_SLOT, FLASH_PLAY_INT_DOOR, 1.0, 1.0);
            return;
        }

        let at = unsafe { raise_motion_speed(read_flash_motion(this), speed) };
        note_motion_write(FLASH_PLAY_INT_DOOR, FLASH_PLAY_INT_SLOT, at, speed);
    }
    bail {
                get_orig_fn!(FlashPlayer_PlayLabelActionIntSpeed, FlashPlayerPlayIntFn)(this, label, action, value)
    }
}

type FlashPlayerPlayFloatFn = extern "C" fn(this: *mut Il2CppObject, label: *mut Il2CppString, action: *mut Il2CppObject, value: f32);
// `Play/3 -> void(string<System.String>, class<System.Action>, float)` (`introspect.log:28694`), the same door with a
// float, which run 44 reached zero times. Armed so a run says whether this client ever plays a cut-in effect through
// it, with the same write on the motion behind the player.
def_detour! {
    FlashPlayer_PlayLabelActionFloatSpeed(this: *mut Il2CppObject, label: *mut Il2CppString, action: *mut Il2CppObject, value: f32) {
            get_orig_fn!(FlashPlayer_PlayLabelActionFloatSpeed, FlashPlayerPlayFloatFn)(this, label, action, value);
        let speed = flash_motion_speed();

        if speed == 1.0 {
            hit(FLASH_PLAY_FLOAT_SLOT, FLASH_PLAY_FLOAT_DOOR, 1.0, 1.0);
            return;
        }

        let at = unsafe { raise_motion_speed(read_flash_motion(this), speed) };
        note_motion_write(FLASH_PLAY_FLOAT_DOOR, FLASH_PLAY_FLOAT_SLOT, at, speed);
    }
    bail {
                get_orig_fn!(FlashPlayer_PlayLabelActionFloatSpeed, FlashPlayerPlayFloatFn)(this, label, action, value)
    }
}

/// What the scaling door may hand `SetMotionSpeed`: raised to the lever, capped at the pair bound, never below what
/// the caller handed, and a motion the game paused (`0.0`) or slowed under the lever left as the game set it. The
/// caller hands its own value on every call, so scaling the argument cannot compound (C22).
fn raise_speed_argument(speed: f32, bound: f32) -> f32 {
    if bound == 1.0 || !(speed > 0.0) {
        return speed;
    }

    (speed * bound).min(bound).max(speed)
}

type MotionSetSpeedDoorFn = extern "C" fn(this: *mut Il2CppObject, speed: f32, to_children: bool);
// `AnMotion::SetMotionSpeed/2 -> void(float, bool)` (`introspect.log:28841`), the game's own speed door on the
// motion. The argument is scaled rather than the field multiplied, and the value written is never below what the
// caller handed nor past the pair bound: a motion the game put at 8.0 stays at 8.0, and a paused motion stays paused.
def_detour! {
    AnMotion_SetMotionSpeed(this: *mut Il2CppObject, speed: f32, to_children: bool) {
            let raised = raise_speed_argument(speed, flash_motion_speed());

        hit(MOTION_SET_SPEED_SLOT, MOTION_SET_SPEED_DOOR, speed, raised);

        get_orig_fn!(AnMotion_SetMotionSpeed, MotionSetSpeedDoorFn)(this, raised, to_children);
    }
    bail {
                get_orig_fn!(AnMotion_SetMotionSpeed, MotionSetSpeedDoorFn)(this, speed, to_children)
    }
}

// The training stat plate cascade. Run 13 measured 10,324 ms inside one training cut, of which 439 ms
// waited for a tap and 10,034 ms was the cut playing before it asked, while the cut's own timeline
// reported 2.4 s of total length. The door on that path carrying a duration is `InitializePlateList`,
// reached six times in the session on a float of 1.0, next to twelve gauge plays whose duration this
// group had already cut from 2.4 s to 0.12 s. The list is a generic parameter and travels as a pointer
// untouched (C48). Since item 59 the float is off the group levers too: this door is `Group::Training`,
// which has no factor behind it, because the interval is the beat `TrainingParamChangeUI` stores as
// `_groupInterval` / `_sequence_interval` and `CoroutineEndCheck` waits through - the completion the
// plate cascade closes on is what the turn's coroutine resumes on, and item 59's arm list is the
// measurement of what happens when a result screen slider shortens it. What does reach it is
// `training_plate_speed`, a lever with no pair to price (C58, ledger item 74), and C62's floor is the
// guard on how short it may hand the beat. The door stays armed for two
// reasons: the census line `TrainingParamChangeUI.InitializePlateList=N` is one of the training doors a
// run reads (C51), and `TrainingCuttProbe` cannot hook an address twice, so its plate cascade clocks are
// fed from here (`note_plate_call`). The door used to stand in the probe itself, which installs nothing
// unless debug_mode is on; it is back where a player who never turns that switch can still see it.
type PlateInitializeListFn = extern "C" fn(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32);
def_detour! {
    TrainingParamChangeUI_InitializePlateListSpeed(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32) {
            let scaled = plate_cascade_handoff(interval);

        hit(12, "TrainingParamChangeUI.InitializePlateList", interval, scaled);
        TrainingCuttProbe::note_plate_call(this, interval);

        get_orig_fn!(TrainingParamChangeUI_InitializePlateListSpeed, PlateInitializeListFn)(this, list, scaled);
    }
}

// One float argument in a fixed position, so the wrapper cannot misread it. This is the
// grand result screen's own entry point for its hardcoded DURATION constant.
type GrandResultFadeInFromRightFn = extern "C" fn(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32);
def_detour! {
    TeamStadiumGrandResult_FadeInContentFromRight(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32) {
            let scaled = scale_paced(duration, Group::Screens, PACE_GRAND_RESULT_FADE);
        hit(6, "TeamStadiumGrandResultViewController.FadeInContentFromRight", duration, scaled);

        get_orig_fn!(TeamStadiumGrandResult_FadeInContentFromRight, GrandResultFadeInFromRightFn)(this, content, scaled);
    }
}

// The two ends of the count-up bracket, on the doors the client's own dump names: `Gallop.CountupModifier::
// OnPlay/0 -> void()` and `OnComplete/0 -> void()` (`introspect.log:15918-15919`). These are the reading
// `PACE_COUNTUP` says the count-up door owes, and they are the only doors in this tree that say what one
// count-up's completion took in wall clock. Observe only: each wrapper hands the game its own call, the
// barrier's `Panicked` answer owes it as well, and the bracket writes no game value, so the measurement cannot
// change the thing it measures (the shape `WaitProbe`'s two yield doors already hold to).
type CountupVoidFn = extern "C" fn(this: *mut Il2CppObject);

def_detour! {
    CountupModifier_OnPlay(this: *mut Il2CppObject) {
            countup_play(this, countup_now_ns());

        get_orig_fn!(CountupModifier_OnPlay, CountupVoidFn)(this);
    }
    bail {
        get_orig_fn!(CountupModifier_OnPlay, CountupVoidFn)(this)
    }
}

def_detour! {
    CountupModifier_OnComplete(this: *mut Il2CppObject) {
            countup_complete(this, countup_now_ns());

        get_orig_fn!(CountupModifier_OnComplete, CountupVoidFn)(this);
    }
    bail {
        get_orig_fn!(CountupModifier_OnComplete, CountupVoidFn)(this)
    }
}

// Every class named below has to be resolved before the getters are installed, because
// the install macro looks the class up by name. `SingleModeMainViewTrainingFooter`,
// `StoryTimelineTrainingCuttClipData` and `SingleModeUtils` are gone: their hooks printed no call line
// in four runs, and run 11 put the training scaling points on two other classes (C51).
const GETTER_CLASSES: &[&str] = &[
    "CountupModifier", "TextModifier", "StoryTimelineController",
    "TeamStadiumGrandResultViewController", "SingleModeMainViewTrainingCutStatus", "SingleModeMainViewHpGauge",
    // Not a getter class: this one is looked up for the plate cascade door below.
    "TrainingParamChangeUI",
];

fn install_getters(umamusume: *const Il2CppImage) {
    let mut classes: HashMap<&str, *mut Il2CppClass> = HashMap::new();

    for name in GETTER_CLASSES {
        let name_const = CString::new(*name).unwrap();
        let class = il2cpp_class_from_name(umamusume, c"Gallop".as_ptr(), name_const.as_ptr());

        if class.is_null() {
            debug!("AnimationSpeed: Gallop.{} not present in this build", name);
            continue;
        }

        classes.insert(name, class);
    }

    install_getter!(classes, CountupModifier_getDuration, CountupModifier, get_Duration);
    install_getter!(classes, TextModifier_getDuration, TextModifier, get_Duration);
    install_getter!(classes, TextModifier_getDelay, TextModifier, get_Delay);
    install_getter!(classes, StoryTimeline_getTimeScaleEventWipe, StoryTimelineController, get_TimeScaleEventWipe);
    install_getter!(classes, StoryTimeline_getTimeScaleAfterEndStory, StoryTimelineController, get_TimeScaleAfterEndStory);

    // The count-up bracket, armed beside the door it measures. Two `new_hook!` calls rather than one helper
    // standing on both doors, because `new_hook!` keys `disabled_hooks` on the site's own id and one helper
    // would give the two ends of one bracket the same key (C27): putting one door down would put both down.
    // Both ends are needed for a reading, so the census line says which of them resolved.
    if let Some(class) = classes.get("CountupModifier").copied() {
        let on_play = unsafe { resolve_method(class, "OnPlay", &[], Il2CppTypeEnum_IL2CPP_TYPE_VOID) };
        let on_complete = unsafe { resolve_method(class, "OnComplete", &[], Il2CppTypeEnum_IL2CPP_TYPE_VOID) };

        if on_play != 0 || on_complete != 0 {
            // The bracket's clock starts here, on the install path, so a completion is never timed against a
            // base that does not exist yet.
            let _ = COUNTUP_BRACKET_START.set(Instant::now());
        }

        if on_play != 0 {
            new_hook!(on_play, CountupModifier_OnPlay);
        }

        if on_complete != 0 {
            new_hook!(on_complete, CountupModifier_OnComplete);
        }

        info!(
            "AnimationSpeed: count-up completion bracket on CountupModifier: OnPlay {}, OnComplete {}; a completion closed on the instance the door handed is what moves the door off the pair bound",
            bracket_door_state(on_play),
            bracket_door_state(on_complete),
        );
    }

    if let Some(class) = classes.get("TeamStadiumGrandResultViewController").copied() {
        let addr = unsafe { resolve_method(
            class, "FadeInContentFromRight",
            &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4],
            Il2CppTypeEnum_IL2CPP_TYPE_VOID,
        ) };

        if addr != 0 { new_hook!(addr, TeamStadiumGrandResult_FadeInContentFromRight); }
    }

    // `SingleModeMainViewTrainingCutStatus.PlayIn/4` is not hooked any more. Run 26 is the reason, written out
    // at `TRAINING_HIT_SLOTS`: cutting that float from 2.4 s to 0.12 s was measured in the same session that
    // held the status panel off for 7866, 8182 and 7866 ms, and the two cuts after the human turned this group
    // off closed hole free. Nothing else in this pass needed the class, so the lookup went with the hook.
    if let Some(class) = classes.get("SingleModeMainViewHpGauge").copied() {
        let addr = unsafe { resolve_method(
            class, "SetProgressbarBlendTime",
            &[Il2CppTypeEnum_IL2CPP_TYPE_R4],
            Il2CppTypeEnum_IL2CPP_TYPE_VOID,
        ) };

        if addr != 0 { new_hook!(addr, HpGauge_SetBlendTimeSpeed); }
    }

    // The plate cascade door. `InitializePlateList/2 -> void(generic<System.Collections.Generic.List<
    // Gallop.TrainingParamChangeUI.ChangeParameterInfo>>, float)` needs the generic resolver, the same
    // one the probe used before this hook took the address over (C48). One door, one scaling: the probe
    // is told about the call so its plate clocks keep working, and it is not hooked a second time.
    if let Some(class) = classes.get("TrainingParamChangeUI").copied() {
        let addr = unsafe { resolve_generic_ref_method(
            class, "InitializePlateList",
            &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4],
            Il2CppTypeEnum_IL2CPP_TYPE_VOID,
        ) };

        if addr != 0 { new_hook!(addr, TrainingParamChangeUI_InitializePlateListSpeed); }
    }

    // The training cut-in clock. `SingleModeUtils::GetTrainingCutTimeScale/1 -> static float(float)`
    // (`introspect.log:24381`) carries no `this`, so it resolves through the static matcher and its wrapper
    // declares the dumped float alone (A3), and the class is reached the way the probe reaches it, by splitting
    // the dumped label into namespace and name (A27). The probe stood on this address until now and reports the
    // call through `note_cut_clock` instead, so nothing here hooks one address twice.
    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.SingleModeUtils") {
        let addr = unsafe { resolve_static_method(
            class, "GetTrainingCutTimeScale",
            &[Il2CppTypeEnum_IL2CPP_TYPE_R4],
            Il2CppTypeEnum_IL2CPP_TYPE_R4,
        ) };

        if addr != 0 { new_hook!(addr, SingleModeUtils_GetTrainingCutTimeScaleSpeed); }
    }

    // The training tag cut-in effect, the leg run 40 measured at 1214 and 1223 ms between `PlayCutIn` and its
    // `done` action coming back as `PlayCutInOut`. `CreateLineEffect/1 -> class<UnityEngine.Animator>(class<Transform>)`
    // (`introspect.log:23805`) hands the effect's Animator back, and `PlayLineEffect/0 -> void()`
    // (`introspect.log:23809`) is the door the class plays its own `_topLineAnimator` / `_bottomLineAnimator`
    // through, so both doors hand the lever a live object instead of an address guessed from a field. Nothing is
    // armed unless `UnityEngine.Animator`'s getter and setter pair resolved as well: a lever that cannot read the
    // speed it is about to raise cannot honour the raise-only rule, and 0 is never called (AGENTS section 2).
    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.SingleModeMainViewTagTrainingCutInPlayer") {
        let (get_addr, set_addr) = unsafe {
            match crate::il2cpp::symbols::get_assembly_image(c"UnityEngine.AnimationModule.dll") {
                Ok(image) if !image.is_null() => {
                    let animator = il2cpp_class_from_name(image, c"UnityEngine".as_ptr(), c"Animator".as_ptr());

                    if animator.is_null() {
                        (0usize, 0usize)
                    } else {
                        (
                            resolve_method(animator, "get_speed", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4),
                            resolve_method(animator, "set_speed", &[Il2CppTypeEnum_IL2CPP_TYPE_R4], Il2CppTypeEnum_IL2CPP_TYPE_VOID),
                        )
                    }
                }
                Ok(_) => (0usize, 0usize),
                Err(_) => (0usize, 0usize),
            }
        };

        ANIMATOR_GET_SPEED_ADDR.store(get_addr, Ordering::Release);
        ANIMATOR_SET_SPEED_ADDR.store(set_addr, Ordering::Release);

        let top = il2cpp_class_get_field_from_name(class, c"_topLineAnimator".as_ptr());
        let bottom = il2cpp_class_get_field_from_name(class, c"_bottomLineAnimator".as_ptr());
        TAG_CUT_TOP_ANIMATOR_FIELD.store(top as usize, Ordering::Release);
        TAG_CUT_BOTTOM_ANIMATOR_FIELD.store(bottom as usize, Ordering::Release);

        if get_addr == 0 || set_addr == 0 {
            warn!(
                "AnimationSpeed: tag cut-in lever stays inert: UnityEngine.Animator get_speed {} and set_speed {} did not resolve, so {} and {} are not armed",
                get_addr, set_addr, TAG_CUT_DOOR, TAG_CUT_LINE_DOOR
            );
        } else {
            debug!(
                "AnimationSpeed: tag cut-in lever armed on {} and {}: Animator get_speed {get_addr} set_speed {set_addr}, line animator fields {top:p} / {bottom:p}",
                TAG_CUT_DOOR, TAG_CUT_LINE_DOOR
            );

            unsafe { note_tag_cut_statics(class) };

            let create = unsafe { resolve_method(class, "CreateLineEffect", &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS], Il2CppTypeEnum_IL2CPP_TYPE_CLASS) };
            let play = unsafe { resolve_method(class, "PlayLineEffect", &[], Il2CppTypeEnum_IL2CPP_TYPE_VOID) };

            if create != 0 { new_hook!(create, TagCutInPlayer_CreateLineEffectSpeed); }
            if play != 0 { new_hook!(play, TagCutInPlayer_PlayLineEffectSpeed); }
        }
    }

    // The clock a training cut-in effect actually runs on. `AnimateToUnity.AnMotion` lives in `Plugins.dll`, an image
    // the dump scope does not walk but the game loads, and run 45 reached it through `Gallop.FlashPlayer._motion`
    // (`introspect.log:28743`). Nothing is armed unless the motion's own getter and setter pair resolved: a lever
    // that cannot read the speed it is about to raise cannot honour the raise-only rule, and 0 is never called.
    let flash_player = TrainingCuttProbe::class_for_label(umamusume, "Gallop.FlashPlayer");
    let motion_class = crate::il2cpp::symbols::get_assembly_image(c"Plugins.dll").ok().and_then(|image| {
        if image.is_null() {
            return None;
        }

        let class = il2cpp_class_from_name(image, c"AnimateToUnity".as_ptr(), c"AnMotion".as_ptr());

        if class.is_null() { None } else { Some(class) }
    });

    match (flash_player, motion_class) {
        (Some(player), Some(class)) => {
            let (get_addr, set_addr) = unsafe {
                (
                    resolve_method(class, "get_MotionSpeed", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4),
                    resolve_method(class, "SetMotionSpeed", &[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_VOID),
                )
            };
            let field = il2cpp_class_get_field_from_name(player, c"_motion".as_ptr());

            MOTION_GET_SPEED_ADDR.store(get_addr, Ordering::Release);
            MOTION_SET_SPEED_ADDR.store(set_addr, Ordering::Release);
            FLASH_MOTION_FIELD.store(field as usize, Ordering::Release);

            if get_addr == 0 || set_addr == 0 {
                warn!(
                    "AnimationSpeed: cut-in motion lever stays inert: AnMotion get_MotionSpeed {} and SetMotionSpeed {} did not resolve, so {} is not armed",
                    get_addr, set_addr, MOTION_SET_SPEED_DOOR
                );
            } else {
                debug!(
                    "AnimationSpeed: cut-in motion lever armed on {} and {}: AnMotion get_MotionSpeed {get_addr} SetMotionSpeed {set_addr}, FlashPlayer field _motion {field:p}",
                    FLASH_PLAY_INT_DOOR, MOTION_SET_SPEED_DOOR
                );

                let play_int = unsafe { resolve_method(player, "Play", &[Il2CppTypeEnum_IL2CPP_TYPE_STRING, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID) };
                let play_float = unsafe { resolve_method(player, "Play", &[Il2CppTypeEnum_IL2CPP_TYPE_STRING, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4], Il2CppTypeEnum_IL2CPP_TYPE_VOID) };

                if play_int != 0 { new_hook!(play_int, FlashPlayer_PlayLabelActionIntSpeed); }
                if play_float != 0 { new_hook!(play_float, FlashPlayer_PlayLabelActionFloatSpeed); }

                // The game's own speed door is armed on the same address the lever writes through, which is one hook
                // on one address (A4): a call this fork makes lands in the wrapper, and the wrapper's bound leaves an
                // already raised value where it is.
                new_hook!(set_addr, AnMotion_SetMotionSpeed);
            }
        }
        (player, motion) => {
            debug!(
                "AnimationSpeed: cut-in motion lever not armed: Gallop.FlashPlayer {} and AnMotion in Plugins.dll {}",
                if player.is_some() { "found" } else { "not found" },
                if motion.is_some() { "found" } else { "not found" }
            );
        }
    }
}

// The tag cut-in player's own `static readonly` cut-in constants, read once at init so a run knows whether the
// effect's length is a constant this fork could rewrite or a cue length it cannot reach. This client spells them
// `CUT_IN_MIN [static const int]` and `CUT_IN_MAX [public static const int]` (`introspect.log:23816`), so the read
// takes the type the field actually reports instead of assuming a float. Read only; nothing here writes a static.
unsafe fn note_tag_cut_statics(class: *mut Il2CppClass) {
    for name in [c"CUT_IN_MIN", c"CUT_IN_MAX"] {
        let field = il2cpp_class_get_field_from_name(class, name.as_ptr());

        if field.is_null() {
            debug!("AnimationSpeed: tag cut-in constant {} is not in this client", name.to_string_lossy());
            continue;
        }

        let field_type = il2cpp_field_get_type(field);

        if field_type.is_null() {
            debug!("AnimationSpeed: tag cut-in constant {} has no type this build can read", name.to_string_lossy());
            continue;
        }

        let type_enum = (*field_type).type_();

        if type_enum == Il2CppTypeEnum_IL2CPP_TYPE_R4 {
            let mut value: f32 = 0.0;
            il2cpp_field_static_get_value(field, &mut value as *mut f32 as *mut c_void);

            debug!("AnimationSpeed: tag cut-in constant {} reads {} as a float", name.to_string_lossy(), value);
        } else if type_enum == Il2CppTypeEnum_IL2CPP_TYPE_I4 {
            let mut value: i32 = 0;
            il2cpp_field_static_get_value(field, &mut value as *mut i32 as *mut c_void);

            debug!("AnimationSpeed: tag cut-in constant {} reads {} as an int", name.to_string_lossy(), value);
        } else {
            debug!("AnimationSpeed: tag cut-in constant {} is a type this build does not read", name.to_string_lossy());
        }
    }
}

pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Release);
}

/// Called from the game tick in `GameSystem::GameSystem_Update`. Nothing pending is the
/// common case and it stays free: no lock, no config, no game field. What a factor change
/// pays for is the pass below, once.
pub fn apply_if_dirty() {
    if !DIRTY.swap(false, Ordering::AcqRel) {
        return;
    }

    apply();
}

pub fn apply() {
    // The pass a run counts. It is charged before anything else so a caller that reaches this
    // 300 times pays for 300 arrivals, which is what `apply_if_dirty` is there to keep down.
    let pass = APPLY_PASSES.fetch_add(1, Ordering::Relaxed) + 1;

    // The pass reads the config and reaches the game's own settings. Before the singleton exists
    // `Hachimi::instance()` ends the process, and there is no mirrored config to plan from and no
    // game state to write, so the pass stops here instead of at its first config read. It is also
    // the seam that lets `cargo test` drive `apply_if_dirty` and read the counters the game's own
    // pass charges, rather than a test module's copy of them.
    if !Hachimi::is_initialized() {
        return;
    }

    // The game's own High Speed settings travel with the speed groups, so a change made in the
    // Config Editor lands at the same point either way. Each reads the config, and each charges
    // that read to this pass.
    super::HighSpeedSetting::apply();
    super::StoryTimelineController::apply_config();

    // Mirrored first, and even when there is nothing to rewrite: the detours that scale
    // arguments read these values, and a build with no rewritable duration field would
    // otherwise ignore every option. The write loop below reads the mirrors, not the config.
    refresh_config_mirrors();
    let factors = mirrored_factors();

    // Applied markers: a group is rewritten when its configured factor differs from the
    // factor its fields currently sit at. A field is therefore written once per factor
    // change instead of once per tick, and a group at its neutral factor with nothing ever
    // written is left exactly as the game has it.
    let rewrite = plan_pass(factors);

    // Nothing to rewrite has to reach the bail out before the entry lock.
    if !rewrite.iter().any(|needed| *needed) {
        report_pass(pass);
        return;
    }

    APPLY_ENTRY_LOCKS.fetch_add(1, Ordering::Relaxed);

    let mut entries = ENTRIES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if entries.is_empty() {
        // The table is what decides whether a pass has any field to touch: 0 of 61 duration
        // fields resolve on the current client (C13). A group that is due for a rewrite then
        // costs this pass its config reads and one lock, and no field call at all.
        report_pass(pass);
        return;
    }

    APPLY_TABLE_PASSES.fetch_add(1, Ordering::Relaxed);

    let mut scaled = 0usize;

    // A group whose marker stays unset had a field with no baseline in this pass, so the
    // next pass looks at it again: a class whose static constructor has not run reads as
    // zero, and its shipped constant only appears once the scene that uses it is loaded.
    // That retry is one pass per view change or config change, not one per frame.
    let mut no_baseline = [0usize; 4];

    for entry in entries.iter_mut() {
        let group = group_index(entry.group);

        if !rewrite[group] {
            continue;
        }

        let factor = factors[group];
        let field = entry.info as *mut FieldInfo;
        let current = read_static(field, entry.kind);

        // Anything the game put here since our last write is the new baseline, and
        // comparing against our own last write is what stops a factor from compounding
        // against a value the game reassigns at runtime.
        entry.original = baseline(current, entry.last_written, entry.original);

        if entry.original == 0.0 {
            // Class not initialised yet, or the shipped constant really is zero.
            no_baseline[group] += 1;
            continue;
        }

        let written = write_static(field, entry.kind, entry.original, factor);
        entry.last_written = written;

        if current != written {
            scaled += 1;
            debug!("AnimationSpeed: {}.{} {} -> {} (x{})", entry.class, entry.field, current, written, factor);
        }
    }

    finish_pass(rewrite, factors, no_baseline);

    if scaled > 0 {
        info!("AnimationSpeed: shortened {} animation duration fields", scaled);
    }

    report_pass(pass);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;

    #[test]
    fn the_plate_cascade_floor_never_runs_slower_than_the_game_handed_in() {
        // The pair run 31 reached: the game's 1.0 s and this fork's 20x.
        assert_eq!(plate_cascade_interval(1.0, 1.0 / 20.0), MIN_PLATE_INTERVAL_SEC);
        assert_eq!(plate_cascade_interval(5.0, 5.0 / 20.0), MIN_PLATE_INTERVAL_SEC);

        // A game interval already under the floor stays where the game put it. A floor that lifts 0.1 s
        // to 0.25 s would be a lever slowing the game down on the door that was only meant to speed it.
        for interval in [0.02, 0.05, 0.1, 0.2, 0.249999] {
            assert_eq!(plate_cascade_interval(interval, interval / 20.0), interval, "the floor slowed the cascade");
        }

        // At the shipped defaults scale_duration hands the value back, and the door has to stay inert.
        assert_eq!(plate_cascade_interval(1.0, 1.0), 1.0);
        assert_eq!(plate_cascade_interval(0.1, 0.1), 0.1);

        // What scale_duration declined to touch stays declined, including a zero the game handed in.
        assert_eq!(plate_cascade_interval(1.0, 0.0), 1.0);
        assert_eq!(plate_cascade_interval(1.0, f32::NAN), 1.0);
        assert_eq!(plate_cascade_interval(0.0, 0.0), 0.0, "a zero interval is not this lever's to invent");
    }

    #[test]
    fn the_plate_cascade_floor_sits_between_the_run31_reading_and_the_game_spacing() {
        // C62: the stall reading is 0.05 s per plate and the design is 1.0 s. The floor has to be above
        // the number the stalled run handed the game and below the number the game asked for, or it is
        // either the same condition again or no change at all.
        assert!(MIN_PLATE_INTERVAL_SEC > 0.05, "the floor sits on top of the reading that stalled");
        assert!(MIN_PLATE_INTERVAL_SEC < 1.0, "the floor is faster than the game's own plate spacing");
        assert_eq!(plate_cascade_interval(1.0, 1.0 / 20.0), MIN_PLATE_INTERVAL_SEC, "run 31's pair lands on the floor");
    }

    // The pass reads and writes process wide mirrors, markers and counters, and `cargo test`
    // runs cases on several threads, so the cases that drive it take turns and hand the state
    // back when they finish. A turn starts with every applied marker at NAN, the state a process
    // that has never written a group holds, which is also the state a build at the shipped
    // defaults is in. The turn is the one `speed_mirror_turn` hands out, because `Time.rs`'s
    // produced time scale write moves a mirror this module's ceiling reads.

    // The count-up bracket is process state too: a case that reads a verdict into the door's lane has to hand
    // the lane, the samples behind it and the door's last handed value back, so that every case starts where a
    // fresh launch does and the door's lane cannot leak between tests running on other threads. An open bracket
    // is not saved: a bracket is a count-up in flight, and the state a case hands back is the state a launch
    // that is not mid-count-up holds.
    struct CountupBracketState {
        lane: u8,
        raw: u32,
        handed: u32,
        off_samples: usize,
        tween_samples: usize,
        completions: usize,
        unmatched: usize,
    }

    fn take_countup_bracket() -> CountupBracketState {
        let state = CountupBracketState {
            lane: COUNTUP_LANE.load(Ordering::Relaxed),
            raw: COUNTUP_RAW.load(Ordering::Relaxed),
            handed: COUNTUP_HANDED.load(Ordering::Relaxed),
            off_samples: COUNTUP_OFF_SAMPLES.load(Ordering::Relaxed),
            tween_samples: COUNTUP_TWEEN_SAMPLES.load(Ordering::Relaxed),
            completions: COUNTUP_COMPLETIONS.load(Ordering::Relaxed),
            unmatched: COUNTUP_UNMATCHED.load(Ordering::Relaxed),
        };

        COUNTUP_LANE.store(COUNTUP_LANE_UNPROVEN, Ordering::Release);
        COUNTUP_RAW.store(0, Ordering::Release);
        COUNTUP_HANDED.store(0, Ordering::Release);
        COUNTUP_OFF_SAMPLES.store(0, Ordering::Release);
        COUNTUP_TWEEN_SAMPLES.store(0, Ordering::Release);
        COUNTUP_COMPLETIONS.store(0, Ordering::Release);
        COUNTUP_UNMATCHED.store(0, Ordering::Release);

        for slot in COUNTUP_OPEN_AT_NS.iter() {
            slot.store(0, Ordering::Release);
        }

        for instance in COUNTUP_OPEN_INSTANCE.iter() {
            instance.store(std::ptr::null_mut(), Ordering::Release);
        }

        state
    }

    fn restore_countup_bracket(state: &CountupBracketState) {
        COUNTUP_LANE.store(state.lane, Ordering::Release);
        COUNTUP_RAW.store(state.raw, Ordering::Release);
        COUNTUP_HANDED.store(state.handed, Ordering::Release);
        COUNTUP_OFF_SAMPLES.store(state.off_samples, Ordering::Release);
        COUNTUP_TWEEN_SAMPLES.store(state.tween_samples, Ordering::Release);
        COUNTUP_COMPLETIONS.store(state.completions, Ordering::Release);
        COUNTUP_UNMATCHED.store(state.unmatched, Ordering::Release);
    }

    struct PassTurn {
        _turn: MutexGuard<'static, ()>,
        saved_mirrors: [u32; 11],
        saved_applied: [u32; 4],
        saved_training_note: bool,
        saved_countup: CountupBracketState,
    }

    fn pass_turn() -> PassTurn {
        let turn = speed_mirror_turn();

        let saved_mirrors = [
            TRANSITION_FACTOR.load(Ordering::Relaxed),
            SCREENS_FACTOR.load(Ordering::Relaxed),
            STORY_FACTOR.load(Ordering::Relaxed),
            TIME_SCALE.load(Ordering::Relaxed),
            UI_ANIMATION_SCALE.load(Ordering::Relaxed),
            STORY_CHOICE_AUTO_SELECT_MULT.load(Ordering::Relaxed),
            TIME_SCALE_RAISE.load(Ordering::Relaxed),
            TIME_SCALE_PRODUCED.load(Ordering::Relaxed),
            PLATE_FACTOR.load(Ordering::Relaxed),
            TRAINING_CUT_FACTOR.load(Ordering::Relaxed),
            TAG_CUT_FACTOR.load(Ordering::Relaxed),
        ];
        let saved_applied = [
            APPLIED_FACTORS[0].load(Ordering::Relaxed),
            APPLIED_FACTORS[1].load(Ordering::Relaxed),
            APPLIED_FACTORS[2].load(Ordering::Relaxed),
            APPLIED_FACTORS[3].load(Ordering::Relaxed),
        ];
        // The training gate note is a once per process latch, so a case that asks whether the line is
        // owed has to start from "nothing has been said yet" like a fresh launch does.
        let saved_training_note = TRAINING_DOORS_NOTE_LOGGED.swap(false, Ordering::AcqRel);

        // A turn starts where a launch that has never written `Time.timeScale` starts: the state every
        // recorded run was in, and the one the completion ceiling has to stay inert in.
        TIME_SCALE_RAISE.store(1.0f32.to_bits(), Ordering::Release);
        TIME_SCALE_PRODUCED.store(1.0f32.to_bits(), Ordering::Release);

        for marker in APPLIED_FACTORS.iter() {
            marker.store(f32::NAN.to_bits(), Ordering::Release);
        }

        PassTurn {
            _turn: turn,
            saved_mirrors,
            saved_applied,
            saved_training_note,
            saved_countup: take_countup_bracket(),
        }
    }

    impl Drop for PassTurn {
        fn drop(&mut self) {
            TRANSITION_FACTOR.store(self.saved_mirrors[0], Ordering::Release);
            SCREENS_FACTOR.store(self.saved_mirrors[1], Ordering::Release);
            STORY_FACTOR.store(self.saved_mirrors[2], Ordering::Release);
            TIME_SCALE.store(self.saved_mirrors[3], Ordering::Release);
            UI_ANIMATION_SCALE.store(self.saved_mirrors[4], Ordering::Release);
            STORY_CHOICE_AUTO_SELECT_MULT.store(self.saved_mirrors[5], Ordering::Release);
            TIME_SCALE_RAISE.store(self.saved_mirrors[6], Ordering::Release);
            TIME_SCALE_PRODUCED.store(self.saved_mirrors[7], Ordering::Release);
            PLATE_FACTOR.store(self.saved_mirrors[8], Ordering::Release);
            TRAINING_CUT_FACTOR.store(self.saved_mirrors[9], Ordering::Release);
            TAG_CUT_FACTOR.store(self.saved_mirrors[10], Ordering::Release);
            TRAINING_DOORS_NOTE_LOGGED.store(self.saved_training_note, Ordering::Release);
            restore_countup_bracket(&self.saved_countup);

            for group in 0..self.saved_applied.len() {
                APPLIED_FACTORS[group].store(self.saved_applied[group], Ordering::Release);
            }
        }
    }

    fn marker_of(group: Group) -> f32 {
        f32::from_bits(APPLIED_FACTORS[group_index(group)].load(Ordering::Relaxed))
    }

    // How many fields one working pass reaches, read straight off the table `init` walks. This is
    // the number the pass counters print on a client whose fields resolve, and the reason the per
    // field half of C36's number is a run measurement rather than a test measurement: the il2cpp
    // read and write need the game's own FieldInfo, and 0 of these 61 resolve on this client
    // (C13), so nothing here can perform them.
    fn field_count(group: Group) -> usize {
        FIELDS.iter().filter(|spec| spec.group == group).count()
    }

    // One pass through the two functions `apply` runs around its write loop: `plan_pass` decides
    // whether the table is due, `finish_pass` records the groups it wrote. `no_baseline` is the
    // per group count the write loop hands back, so a test says which fields had no baseline
    // instead of pretending it can read them.
    fn run_planned_pass(no_baseline: [usize; 4]) -> bool {
        let factors = mirrored_factors();
        let rewrite = plan_pass(factors);

        finish_pass(rewrite, factors, no_baseline);

        rewrite.iter().any(|needed| *needed)
    }

    #[test]
    fn only_the_generic_resolvers_widen_a_reference_slot() {
        // The scaling hooks resolve through the exact profiles. Widening is what let a probe reach a
        // `List<...>` signature, and a scaling wrapper must never get one (C48).
        assert!(!MATCH_INSTANCE_VALUE.generic_slots);
        assert!(!MATCH_GETTER_EITHER.generic_slots);
        assert!(!MATCH_REFERENCE.generic_slots);
        assert!(!MATCH_STATIC_VALUE.generic_slots);
        assert!(!MATCH_STATIC_VALUES_ONLY.generic_slots);
        assert!(MATCH_GENERIC_REF.generic_slots);
        assert!(MATCH_STATIC_GENERIC_REF.generic_slots);

        // A wrapper that declares only the real arguments still has to be told its target is static,
        // or it misreads every argument of an instance method.
        assert!(MATCH_STATIC_VALUE.require_static);
        assert!(MATCH_STATIC_GENERIC_REF.require_static);
        assert!(!MATCH_GENERIC_REF.require_static);
        assert!(!MATCH_GENERIC_REF.allow_static);
    }

    #[test]
    fn a_scaling_point_the_game_reaches_without_changing_anything_is_still_counted() {
        // The A21 blind spot. Five training hooks printed an install line and no call line across two
        // career runs because `hit` only reported a value that moved, and a duration the option cannot
        // move prints nothing. Slot 19 is a test slot: the installed hooks use 6 through 16.
        let slot = HIT_SLOTS - 1;

        hit(slot, "test scaling point", 0.0, 0.0);
        hit(slot, "test scaling point", 0.0, 0.0);
        assert_eq!(hit_calls(slot), 2);

        // A call that does change the value counts on the same counter, so a run can add the two
        // kinds of reach together.
        hit(slot, "test scaling point", 0.6, 0.03);
        assert_eq!(hit_calls(slot), 3);

        // A slot outside the table is ignored rather than indexing past it.
        hit(HIT_SLOTS, "test scaling point", 0.6, 0.03);
        assert_eq!(hit_calls(HIT_SLOTS), 0);
    }

    #[test]
    fn the_game_tick_path_reaches_the_pass_once_per_config_change() {
        let _turn = pass_turn();

        let before = APPLY_PASSES.load(Ordering::Relaxed);

        // gui::save_and_reload_config -> AnimationSpeed::mark_dirty(), the Config Editor's Save.
        mark_dirty();

        for _ in 0..300 {
            // GameSystem::GameSystem_Update -> AnimationSpeed::apply_if_dirty(), five seconds of
            // menu at 60 fps. In this process `apply` stops at its uninitialized guard, which is
            // what the counter is read for: the number is the arrivals, and the gate that keeps
            // them down is the shipped swap.
            apply_if_dirty();
        }

        let arrivals = APPLY_PASSES.load(Ordering::Relaxed) - before;

        assert_eq!(arrivals, 1, "the per frame path reached apply {arrivals} times for one config change");
    }

    #[test]
    fn a_build_at_its_neutral_default_asks_for_no_field_rewrite_at_all() {
        let _turn = pass_turn();

        mirror_config(&Config::default());
        let factors = mirrored_factors();

        assert_eq!(factors, [1.0, 1.0, 1.0, 1.0], "a shipped default mirrored to a speed up");

        // This is the exact decision `apply` bails on, before the entry lock and before the table.
        assert_eq!(plan_pass(factors), [false, false, false, false],
            "a build with every option at its neutral default asked to rewrite a game field");

        for _ in 0..300 {
            assert!(!run_planned_pass([0; 4]), "a pass at the shipped defaults reached the field table");
        }

        for group in [Group::Transition, Group::Screens, Group::Story, Group::Training] {
            assert!(marker_of(group).is_nan(), "a pass at the shipped defaults marked group {} as written", group_index(group));
        }
    }

    #[test]
    fn one_factor_change_asks_for_one_pass_across_300_ticks() {
        let _turn = pass_turn();

        let mut config = Config::default();
        config.transition_speed = 2.0;
        mirror_config(&config);

        let transition = field_count(Group::Transition);
        let mut working = 0;

        for _ in 0..300 {
            if run_planned_pass([0; 4]) {
                working += 1;
            }
        }

        assert_eq!(working, 1, "the plan asked for a rewrite pass {} times in 300 ticks", working);
        assert_eq!(marker_of(Group::Transition), 2.0, "the pass did not mark its own factor as applied");
        assert!(marker_of(Group::Screens).is_nan(), "a group nobody changed was marked as written");

        println!(
            "C36, measured by the shipped plan: 300 ticks at transition x2 ask for one write pass, so that pass reaches at most {} fields instead of 300 passes over all {}. The counts are read off FIELDS, not off a call: the il2cpp read and write need the game's own FieldInfo, none of them resolve on this client (C13), and the apply line a run prints is what reports the calls really made.",
            transition,
            field_count(Group::Transition) + field_count(Group::Screens) + field_count(Group::Story) + field_count(Group::Training)
        );
    }

    #[test]
    fn a_factor_change_asks_only_for_its_own_group() {
        let _turn = pass_turn();

        let mut config = Config::default();
        mirror_config(&config);

        config.result_screen_speed = 2.0;
        mirror_config(&config);
        assert_eq!(plan_pass(mirrored_factors()), [false, true, false, false], "the wrong group was due");
        assert!(run_planned_pass([0; 4]));
        assert_eq!(marker_of(Group::Screens), 2.0);
        // C58 / item 59: the option that moves the result screen group is the one that used to move the
        // training gates with it. Raising it now asks for one group, and the training group's marker stays
        // "this module has never written it".
        assert!(marker_of(Group::Training).is_nan(), "result_screen_speed reached the training group");

        config.transition_speed = 3.0;
        mirror_config(&config);
        assert_eq!(plan_pass(mirrored_factors()), [true, false, false, false], "the screens group was due again");
        assert!(run_planned_pass([0; 4]));
        assert_eq!(marker_of(Group::Transition), 3.0);

        config.story_speed = 2.0;
        mirror_config(&config);
        assert_eq!(plan_pass(mirrored_factors()), [false, false, true, false], "the story group was not due");
        assert!(run_planned_pass([0; 4]));

        // A career run's worth of view changes after that: three factor changes, three passes.
        for _ in 0..40 {
            assert!(!run_planned_pass([0; 4]), "an unchanged config re-planned a rewrite");
        }
    }

    #[test]
    fn the_apply_totals_line_covers_a_short_run_and_a_long_one() {
        // The shape a run reads: the first `PASS_DETAIL_LIMIT` passes in full, then a totals line
        // every `PASS_CHUNK`. A run that only reached a handful of view changes still prints.
        assert!((1..=PASS_DETAIL_LIMIT).all(|pass| pass_is_worth_logging(pass)),
            "a run of a few view changes would have printed no apply line at all");

        assert!(!pass_is_worth_logging(PASS_DETAIL_LIMIT + 1), "every pass would have logged");
        assert!(pass_is_worth_logging(PASS_CHUNK), "a long session stopped printing totals");

        let lines = (1..=(4 * PASS_CHUNK)).filter(|pass| pass_is_worth_logging(*pass)).count();
        assert_eq!(lines, PASS_DETAIL_LIMIT + 4, "the apply line is not the fork's first N then every N pattern");
    }

    #[test]
    fn dropping_a_factor_to_one_asks_for_the_restore_once() {
        let _turn = pass_turn();

        let mut config = Config::default();
        config.story_speed = 4.0;
        mirror_config(&config);

        // The field holds the shipped value and the pass shortens it. The write half is the
        // shipped `scale_value` on the baseline the loop remembers.
        assert!(run_planned_pass([0; 4]));
        assert_eq!(scale_value(0.4, 4.0, Il2CppTypeEnum_IL2CPP_TYPE_R4), (0.4f32 / 4.0) as f64);
        assert_eq!(marker_of(Group::Story), 4.0);

        config.story_speed = 1.0;
        mirror_config(&config);

        assert!(plan_pass(mirrored_factors())[2], "turning the option off did not ask for the restore");
        assert!(run_planned_pass([0; 4]));

        // The restore is the remembered baseline divided by 1.0, so it puts the shipped value
        // back instead of writing 1.0 into a field that never held 1.0.
        assert_eq!(scale_value(0.4, 1.0, Il2CppTypeEnum_IL2CPP_TYPE_R4), (0.4f32 / 1.0) as f64, "the shipped value was not put back");
        assert_eq!(marker_of(Group::Story), 1.0);

        for _ in 0..100 {
            assert!(!run_planned_pass([0; 4]), "the restore pass ran more than once");
        }
    }

    #[test]
    fn the_write_scales_the_games_value_not_our_own_write() {
        let _turn = pass_turn();

        // The shipped pair the write loop runs: `baseline` picks what to scale, `scale_value`
        // does the arithmetic. A second factor applied to our own last write would compound.
        let written = scale_value(0.4, 2.0, Il2CppTypeEnum_IL2CPP_TYPE_R4);
        assert_eq!(written, (0.4f32 / 2.0) as f64);

        // The next pass reads 0.2 back out of the field. It is our own write, so the remembered
        // 0.4 stays the baseline and 1.5 divides the shipped value, not 0.2.
        let base = baseline(written, written, 0.4);
        assert_eq!(base, 0.4, "our own write became the new baseline");
        assert_eq!(scale_value(base, 1.5, Il2CppTypeEnum_IL2CPP_TYPE_R4), (0.4f32 / 1.5f32) as f64);

        // And when the game wrote the field itself, that value is the baseline.
        assert_eq!(baseline(0.75, written, 0.4), 0.75, "the game's own value was ignored");
    }

    #[test]
    fn a_group_whose_fields_had_no_baseline_is_asked_again() {
        let _turn = pass_turn();

        let mut config = Config::default();
        config.transition_speed = 2.0;
        mirror_config(&config);

        // No class in the group has run its static constructor, so the write loop counted a field
        // with no baseline and wrote nothing: `finish_pass` must leave that group's marker alone.
        assert!(run_planned_pass([field_count(Group::Transition); 4]));
        assert!(marker_of(Group::Transition).is_nan(),
            "the group was marked applied at a factor none of its fields ever sat at");

        // The scene loads, the static constructor puts the shipped value in, and the next view
        // change pass finds it. That retry is one pass per view change, not one per frame.
        assert!(run_planned_pass([0; 4]), "the retry pass did not look at the group again");
        assert_eq!(marker_of(Group::Transition), 2.0);
        assert!(!run_planned_pass([0; 4]), "the same factor was written a third time");
    }

    #[test]
    fn plan_group_leaves_a_group_alone_when_its_factor_is_already_applied() {
        assert!(!plan_group(1.0, f32::NAN), "the neutral default would have rewritten a field");
        assert!(plan_group(2.0, f32::NAN), "the option was never applied");
        assert!(!plan_group(2.0, 2.0), "the same factor was applied twice");
        assert!(plan_group(1.0, 2.0), "turning the option off did not restore the shipped value");
        assert!(plan_group(1.5, 2.0), "a lower factor was skipped");
    }

    #[test]
    fn baseline_keeps_the_remembered_value_over_our_own_write() {
        assert_eq!(baseline(0.5, 0.5, 1.0), 1.0, "our own write became the new baseline");
        assert_eq!(baseline(2.0, 0.5, 1.0), 2.0, "the value the game wrote itself was ignored");
        assert_eq!(baseline(0.5, f64::NAN, 0.0), 0.5, "nothing written yet");
    }

    #[test]
    fn scale_value_shortens_once_and_never_below_one_frame() {
        assert_eq!(scale_value(1.0, 2.0, Il2CppTypeEnum_IL2CPP_TYPE_R4), 0.5);
        assert_eq!(scale_value(1.0, 3.0, Il2CppTypeEnum_IL2CPP_TYPE_R8), 1.0 / 3.0);
        assert_eq!(scale_value(20.0, 20.0, Il2CppTypeEnum_IL2CPP_TYPE_I4), 1.0, "a frame count hit zero");
        assert_eq!(scale_value(1.0, 20.0, Il2CppTypeEnum_IL2CPP_TYPE_I4), 1.0, "a frame count hit zero");
        assert_eq!(scale_value(-4.0, 2.0, Il2CppTypeEnum_IL2CPP_TYPE_I4), -2.0, "an offset moved away from zero");
        assert_eq!(scale_value(0.0, 2.0, Il2CppTypeEnum_IL2CPP_TYPE_I4), 0.0);
    }

    // C5's reproduction spelled out on the number the game receives: `DOTween/TweenManager.rs`
    // multiplies the delta of a 60 fps frame by `ui_animation_scale`, and the first time setup
    // wizard offers 0.1..=1000.0. Before the clamp the slider's maximum was the multiplier.
    fn dotween_delta(scale: f32) -> f32 { (1.0 / 60.0) * scale }

    #[test]
    fn ui_animation_scale_is_bounded_before_it_reaches_dotween() {
        let _turn = pass_turn();

        // What the detour used to be handed at the wizard's maximum: 16 s of tween clock per frame.
        assert!(dotween_delta(1000.0) > 16.0, "the unclamped multiplier is no longer measurable");

        // What the mirror publishes for the same setting now.
        let clamped = normalize_ui_animation_scale(1000.0);
        assert_eq!(clamped, MAX_UI_ANIMATION_SCALE, "the slider ceiling reached the game clock");
        assert!(dotween_delta(clamped) < 0.34, "one tick still removed more than MAX_FACTOR allows");

        println!(
            "ui_animation_scale at the wizard maximum: DOTween delta per 60 fps frame {} s unclamped, {} s behind MAX_UI_ANIMATION_SCALE {}",
            dotween_delta(1000.0), dotween_delta(clamped), MAX_UI_ANIMATION_SCALE
        );

        // The range the sliders offer stays usable; the values a hand edited config can hold do not.
        assert_eq!(normalize_ui_animation_scale(1.0), 1.0, "the neutral default is not a speed-up");
        assert_eq!(normalize_ui_animation_scale(2.0), 2.0, "an in range setting was changed");
        assert_eq!(normalize_ui_animation_scale(0.1), MIN_UI_ANIMATION_SCALE);
        assert_eq!(normalize_ui_animation_scale(0.0), MIN_UI_ANIMATION_SCALE, "a hand edited 0 froze every tween");
        assert_eq!(normalize_ui_animation_scale(-5.0), MIN_UI_ANIMATION_SCALE);
        assert_eq!(normalize_ui_animation_scale(f32::NAN), 1.0, "NAN reached a tween");
        assert_eq!(normalize_ui_animation_scale(f32::INFINITY), 1.0);

        // Nothing wrote the mirror in this test process, so a build at the defaults is inert.
        assert_eq!(ui_animation_scale(), 1.0, "the default mirrored to a speed-up");
    }

    // C5's open half, on the arithmetic the shipped detour performs. `DOTween/TweenManager.rs` hands
    // `DG.Tweening.Core.TweenManager::Update` the pair `tween_clocks` returns, and DOTween then picks
    // the channel per tween: `float tDeltaTime = (t.isIndependentUpdate ? independentTime : deltaTime)
    // * t.timeScale;`. The lever is a clock on the delta channel; the other one is the game's own real
    // time, and it is how UI keeps animating while `Time.timeScale` is 0. Scaling it was the defect: a
    // 1 s tween the game marked `SetUpdate(true)` completed in 50 ms at `ui_animation 20`, pause
    // included.
    #[test]
    fn the_tween_clock_layer_scales_the_delta_channel_and_not_the_independent_one() {
        let _turn = pass_turn();

        let frame = 1.0 / 60.0;
        // `DOTweenComponent.Update` computes the third argument from `Time.unscaledDeltaTime`, so at the
        // shipped DOTween options a tick arrives with the same 1/60 the delta channel carries.
        let real_time_tick = 1.0 / 60.0;

        // The shipped defaults: a tween tick passes through, no lever applied.
        assert_eq!(
            tween_clocks(frame, real_time_tick),
            (frame, real_time_tick),
            "the neutral clock changed a tween tick",
        );

        // `ui_animation 20`, the ceiling the mirror publishes for the wizard's 1000.0 (C5).
        mirror_config(&timing_config(1.0, 1.0, 1.0, MAX_UI_ANIMATION_SCALE));
        let (delta, independent) = tween_clocks(frame, real_time_tick);
        assert_eq!(delta, frame * MAX_UI_ANIMATION_SCALE, "the delta channel stopped being the clock this lever speeds up");
        assert_eq!(independent, real_time_tick, "the independent channel was scaled: a time scale independent tween ran on the mod's clock");

        // The wall clock a 1 s tween takes on each channel, 60 ticks to a wall second: the scaled
        // channel keeps the 50 ms the option asks for, the independent channel still takes its second.
        // Before this fix both came out 50 ms, which is a real time UI tween ending in the time it takes
        // the game to draw three frames.
        let wall_ms_for_one_second_of_tween = |channel: f32| 1000.0 / (channel * 60.0);
        assert!(ms_is(wall_ms_for_one_second_of_tween(delta), 1000.0 / MAX_UI_ANIMATION_SCALE), "the scaled clock no longer completes a second 20x faster");
        assert!(ms_is(wall_ms_for_one_second_of_tween(independent), 1000.0), "the real time clock ran faster than real time");

        // The slider's floor: the scaled clock slows down, the game's real time channel is untouched by
        // a lever that is not about it.
        mirror_config(&timing_config(1.0, 1.0, 1.0, MIN_UI_ANIMATION_SCALE));
        let (slower_delta, slower_independent) = tween_clocks(frame, real_time_tick);
        assert_eq!(slower_delta, frame * MIN_UI_ANIMATION_SCALE);
        assert_eq!(slower_independent, real_time_tick, "the pause clock took the slider's slow down");

        // A mirror that is not a number - which `mirror_config` never stores - is the neutral clock on
        // both channels, the same reading `duration_factor` gives it.
        UI_ANIMATION_SCALE.store(f32::NAN.to_bits(), Ordering::Release);
        assert_eq!(
            tween_clocks(frame, real_time_tick),
            (frame, real_time_tick),
            "a broken mirror reached a tween",
        );

        // The pair ceiling (C58) still bounds the channel this lever speeds up, and it is now a ceiling
        // that over-counts a time scale independent tween rather than the number it runs at.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));
        assert!(
            tween_speed(Group::Screens) <= MAX_TWEEN_SPEED_PRODUCT,
            "the pair left its ceiling once the independent channel came out of it",
        );
        let (delta, independent) = tween_clocks(frame, real_time_tick);
        assert_eq!(delta, frame * MAX_UI_ANIMATION_SCALE);
        assert_eq!(independent, real_time_tick);

        println!(
            "tween clock at ui_animation {}x: delta tick {} s -> {} s, independent tick {} s -> {} s (MAX_UI_ANIMATION_SCALE {})",
            ui_animation_scale(), frame, delta, real_time_tick, independent, MAX_UI_ANIMATION_SCALE,
        );
    }

    // C58 / ledger item 62, spelled on the quantity the game waits on. Each layer has its own
    // `MAX_FACTOR` ceiling and each was checked on its own, but a tween's real completion time is the
    // duration handed to the game divided by the clock `TweenManager::Update` is handed, so the pair
    // is what shortens the wait. The three rows are the ones the C58 concept measured in runs 17, 20
    // and 26 beside `ui_animation 20` in the same config snapshot, all three scaled on `Group::Screens`
    // when they were read. Since item 59 only the count up row is still a door in that group: the play in
    // row is the hook run 26 took out, and the plate row is a training gate that takes no group factor.
    // The rows are the arithmetic the bounded lanes actually perform. A door names its pace in
    // `ARMED_DOOR_PACES` and scales through `scale_paced`, so the count up row is the same call the installed
    // hook makes at slot 8 through `countup_screen_duration`; what that door hands at the pair state is
    // asserted on the wrapper itself by
    // `the_count_up_door_a_career_session_reaches_hands_the_trim_bounded_value`.
    const C58_TRAINING_ROWS: [(&str, f32); 3] = [
        ("SingleModeMainViewTrainingCutStatus.PlayIn", 2.4),
        ("TrainingParamChangeUI.InitializePlateList", 1.0),
        ("CountupModifier.get_Duration", 0.16),
    ];

    // The frame every completion assertion in this module holds. `target_fps 200` is the fastest setting any
    // C58 run was played at: run 17's snapshot carried it and its own clock measured 88,728 frames over
    // 499,199 ms, 5.6 ms a frame, and the session log copied to `run log/hachimi.log` line 14 carried it too.
    // The bar is the nominal 200 fps budget, so it is stricter than the frame it is quoted from and stricter
    // than the 16.7 ms budget run 20, run 26 and run 31 were all played at. No completion is priced against a
    // frame looser than the one it ran on.
    const FRAME_MS_AT_200_FPS: f32 = 1000.0 / 200.0;

    // Run 31's own frame, for the claims about the run that stalled 26 min 43 s on `view 1501`. Its snapshot
    // reads `target_fps 60` (`run log/hachimi-run31.log` line 15) and a preset arm never moves `target_fps`
    // (`core::settings_preset`), so the `All levers` arm that log records at line 1068 left it there. Its
    // stalled screen counted 28,662 frames over 484,037 ms (that log line 1690) and its frame totals read
    // 16.8 ms a frame, so 16.9 ms is the frame that stall sat in. It never ran at `target_fps 200`.
    const RUN31_FRAME_MS: f32 = 484037.0 / 28662.0;

    /// Wall clock a completion takes on the mirrors as they stand: what the door hands the game, over the
    /// channel the shipped detour actually advances a tween by. That channel is what `tween_clocks` multiplies
    /// the `deltaTime` argument by, times the `Time.timeScale` that argument already carries into it, so it is
    /// read off the arithmetic the game runs and not off the module's own model: a ceiling priced on the wrong
    /// multiplier is precisely what a test that divides by that model cannot see (C58, item 62).
    fn completion_ms(raw: f32, group: Group) -> f32 {
        scale_duration(raw, group) / shipped_delta_channel() * 1000.0
    }

    /// The multiplier a delta channel tween advances at, taken from `tween_clocks` itself and the mirror
    /// `Time.rs` writes. It has to be the same number `delta_clock()` prices, and the frame sweep says so.
    fn shipped_delta_channel() -> f32 {
        let (tick, _) = tween_clocks(1.0, 1.0);

        tick * time_scale_produced()
    }

    /// The model and the shipped channel, named together where they have to agree.
    fn assert_clock_is_the_shipped_channel(what: &str) {
        let channel = shipped_delta_channel();

        assert!(ms_is(channel, delta_clock()), "{what}: the detour advances a delta channel tween at {channel}x while the ceiling prices {}x", delta_clock());
    }

    fn ms_is(value: f32, expected: f32) -> bool { (value - expected).abs() < 0.01 }

    /// A config with the four timing levers that make up a pair, everything else shipped.
    fn timing_config(transition: f32, screens: f32, story: f32, ui: f32) -> Config {
        Config {
            transition_speed: transition,
            result_screen_speed: screens,
            story_speed: story,
            ui_animation_scale: ui,
            ..Config::default()
        }
    }

    #[test]
    fn the_two_speed_layers_that_reach_one_tween_are_bounded_as_a_pair() {
        let _turn = pass_turn();

        // The pair run 17 and run 20 ran on: `result 20` and `ui_animation 20`.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));

        // The bound is on the pair, not on either lever. A test that only reads the levers is the
        // shape of blindness this item exists because of.
        assert_eq!(factor(Group::Screens), MAX_FACTOR, "the group lever was rewritten instead of bounded");
        assert_eq!(ui_animation_scale(), MAX_UI_ANIMATION_SCALE, "the clock lever was rewritten instead of bounded");

        // What the two ceilings multiplied to before anything looked at the other one.
        let unbounded = MAX_FACTOR * MAX_UI_ANIMATION_SCALE;
        assert_eq!(unbounded, 400.0, "the pair no longer reaches the magnitude C58 recorded");

        // What the pair does to each completion, printed before the assertions so a failure says the
        // numbers the defect was measured in rather than only that something differed.
        let handed: Vec<f32> = C58_TRAINING_ROWS.iter().map(|(_, raw)| scale_duration(*raw, Group::Screens)).collect();
        let completions: Vec<f32> = handed.iter().map(|value| value / ui_animation_scale() * 1000.0).collect();

        println!(
            "C58 pair at result 20 / ui_animation 20: the two ceilings multiply to {unbounded}x; duration handed to the game {} {} {}, completion on a {}x tween clock {} ms, {} ms, {} ms against a {FRAME_MS_AT_200_FPS} ms frame",
            handed[0], handed[1], handed[2],
            ui_animation_scale(), completions[0], completions[1], completions[2],
        );

        // What it reaches now.
        assert_eq!(tween_speed(Group::Screens), MAX_TWEEN_SPEED_PRODUCT, "the pair still multiplies");

        // The defect as C58 measured it: two of the three training durations land under one frame
        // when the pair is unbounded, and none of them does once the pair is bounded.
        let unbounded_under_one_frame = C58_TRAINING_ROWS
            .iter()
            .filter(|(_, raw)| raw / unbounded * 1000.0 < FRAME_MS_AT_200_FPS)
            .count();
        assert_eq!(unbounded_under_one_frame, 2, "the unbounded pair no longer lands two of the three rows under a frame");

        let bounded_under_one_frame = completions.iter().filter(|ms| **ms < FRAME_MS_AT_200_FPS).count();
        assert_eq!(bounded_under_one_frame, 0, "{bounded_under_one_frame} of the three completions are still under one {FRAME_MS_AT_200_FPS} ms frame");

        for ((door, raw), completion) in C58_TRAINING_ROWS.iter().zip(completions.iter()) {
            let completion = *completion;

            assert!(
                ms_is(completion, raw / MAX_TWEEN_SPEED_PRODUCT * 1000.0),
                "{door} completes in {completion} ms, not at the {}x the pair ceiling states", MAX_TWEEN_SPEED_PRODUCT
            );
            assert!(
                completion >= FRAME_MS_AT_200_FPS,
                "{door} still completes in {completion} ms, under one {FRAME_MS_AT_200_FPS} ms frame"
            );
        }
    }

    #[test]
    fn a_group_factor_only_shortens_what_the_tween_clock_layer_left() {
        let _turn = pass_turn();

        // The whole ladder both sliders cover. Every step hands the game at most MAX_TWEEN_SPEED_PRODUCT
        // on one completion, and the shorter the clock already runs the less the group may take out of
        // the duration, down to handing it on unchanged.
        for group in [Group::Transition, Group::Screens, Group::Story] {
            for configured in [1.0, 2.0, 5.0, MAX_FACTOR] {
                for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 4.0, 5.0, 10.0, MAX_UI_ANIMATION_SCALE] {
                    mirror_config(&timing_config(configured, configured, configured, ui));

                    let expected = configured.min((MAX_TWEEN_SPEED_PRODUCT / ui).max(1.0));
                    assert_eq!(
                        duration_factor(group), expected,
                        "configured {configured} on a {ui} clock did not leave the group its headroom"
                    );
                    assert!(
                        tween_speed(group) <= MAX_TWEEN_SPEED_PRODUCT,
                        "{configured} against a {ui} clock reached {}x on one completion", tween_speed(group)
                    );
                    assert!(
                        duration_factor(group) >= 1.0,
                        "the bound turned a speed up into a slower animation than the game asked for"
                    );

                    // The fourth group is deliberately off the ladder: nothing writes it, so a training
                    // gate hands the game its own duration at every rung (C58, item 59).
                    assert_eq!(duration_factor(Group::Training), 1.0, "a training gate took a factor at {configured}/{ui}");
                    assert_eq!(training_gate_duration(1.0), 1.0, "a training duration moved at {configured}/{ui}");
                }
            }
        }

        // At the shipped options every door is inert and the group still gets its full ceiling with
        // nothing on the clock: this is the state the transition gap baselines in LEDGER section A
        // were measured on.
        mirror_config(&Config::default());
        assert_eq!(duration_factor(Group::Transition), 1.0);
        assert_eq!(scale_duration(1.0, Group::Transition), 1.0, "the neutral default changed a duration");
        assert_eq!(tween_speed(Group::Transition), 1.0);

        mirror_config(&timing_config(MAX_FACTOR, 1.0, 1.0, 1.0));
        assert_eq!(duration_factor(Group::Transition), MAX_FACTOR, "a neutral clock cut the group below its own ceiling");
        assert_eq!(scale_duration(1.0, Group::Transition), 1.0 / MAX_FACTOR);

        // The clock slider's floor is a slow down of the tween clock. It is not headroom the group may
        // spend twice: the group stays capped at its own MAX_FACTOR.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MIN_UI_ANIMATION_SCALE));
        assert_eq!(duration_factor(Group::Screens), MAX_FACTOR, "the clock floor handed the group a wider ceiling");
        assert!(tween_speed(Group::Screens) < MAX_TWEEN_SPEED_PRODUCT, "the pair left the ceiling through the clock floor");

        // A mirror that is not a number is read as the neutral clock. This module never stores one;
        // the bound must not turn one into a duration handed on at an invented factor.
        UI_ANIMATION_SCALE.store(f32::NAN.to_bits(), Ordering::Release);
        assert_eq!(duration_factor(Group::Screens), MAX_FACTOR, "a NAN clock changed a duration factor");
    }

    // C58 / ledger item 62, on the multiplier a completion actually runs at. `tween_clocks` multiplies the
    // `deltaTime` argument, and that argument is what `DOTweenComponent.Update` computed from `Time.deltaTime`,
    // which is `Time.timeScale` times real elapsed time. The shipped `set_timeScale` detour writes that number
    // through `apply_time_scale` up to `MAX_TIME_SCALE`, and what it writes is the whole product: a game request
    // of 2.0 under a lever of 5 hands the setter 5.0. Pricing the channel with the 2.5x the layer *added* left
    // the other 2.0x of it unpriced, so the ceiling trimmed the ui lever to 8x, believed the channel was 20x, and
    // the 0.16 s count-up it just handed on closed in 4 ms on a state the same test asserted closed in 8 ms.
    // Stated as the runs have it: every `set_timeScale` call line in every run log this ledger holds is `1 -> 1`,
    // so the state below is reachable in a shipped config and is not a number a run measured.
    #[test]
    fn the_scale_the_write_layer_leaves_in_the_game_is_the_scale_the_completion_runs_on() {
        let _turn = pass_turn();

        let frame = 1.0 / 60.0;
        let lever = MAX_TIME_SCALE;
        let game_value = 2.0;

        // The pair the state is reached with: `result 20`, `ui_animation 20`, and a game that wrote its own 2.0
        // fast forward while the lever was 5. The rung is built the way the write layer builds it, through the
        // same `apply_time_scale` the shipped detour calls, so the pair recorded is a pair that writer can produce.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));

        let produced = apply_time_scale(game_value, lever);
        assert!(ms_is(produced, MAX_TIME_SCALE), "the shipped writer no longer takes a 2.0 request to the ceiling: {produced}");
        assert!(note_time_scale_write(game_value, produced), "a scale that bound the ceiling owed no line");

        assert!(ms_is(time_scale_produced(), produced), "the mirror does not hold what the layer handed the setter: {}", time_scale_produced());
        assert!(ms_is(time_scale_raise(), 2.5), "the factor this fork added is not the one the line names: {}", time_scale_raise());
        assert!(ms_is(delta_clock(), MAX_TWEEN_SPEED_PRODUCT), "the delta clock is not the ui cap times the scale under it: {}", delta_clock());

        // The state the re-aimed ceiling priced, kept as arithmetic so it cannot come back: cap the ui lever by
        // the 2.5x the layer added and the channel it leaves behind carries 8 x 5.0.
        let raise_cap = MAX_TWEEN_SPEED_PRODUCT / time_scale_raise();
        let raise_channel = raise_cap * time_scale_produced();
        let raise_ms = 0.16 / raise_channel * 1000.0;
        let priced_ms = completion_ms(0.16, Group::Screens);

        println!(
            "C58 item 62, the scale under the clock: a ui clock trimmed by the {}x this fork added ran {}x over the {}x Unity is holding, closing the 0.16 s count-up in {raise_ms} ms, under one {FRAME_MS_AT_200_FPS} ms frame, while the same state printed a {}x ceiling; priced on the scale the channel carries, the ui clock runs {}x, the delta clock {}x, and the completion runs {priced_ms} ms",
            time_scale_raise(), raise_channel, time_scale_produced(), MAX_TWEEN_SPEED_PRODUCT,
            ui_clock_of(MAX_UI_ANIMATION_SCALE, produced), delta_clock(),
        );

        assert!(raise_ms < FRAME_MS_AT_200_FPS, "the state the bound exists to stop is no longer measurable here: {raise_ms} ms");
        assert!(raise_channel > MAX_TWEEN_SPEED_PRODUCT, "the channel the old ceiling left unpriced is no longer past the ceiling it was written for: {raise_channel}x");

        // The clock the DOTween detour is handed on that state: the ui lever trimmed to a quarter, because the
        // channel it multiplies already carries 5.0.
        let (delta, independent) = tween_clocks(frame, frame);
        assert!(ms_is(delta, frame * (MAX_TWEEN_SPEED_PRODUCT / MAX_TIME_SCALE)), "the ui clock was not capped to what the scale under it leaves: {delta} per frame");
        assert!(ms_is(delta * produced, frame * MAX_TWEEN_SPEED_PRODUCT), "the channel still multiplied past the pair ceiling on the number both layers sit on");
        assert_eq!(independent, frame, "the cap reached the channel this fork does not multiply");

        // The completion, and the number the ceiling speaks about, now agree.
        assert!(ms_is(priced_ms, 160.0 / MAX_TWEEN_SPEED_PRODUCT), "the count up completion is not the pair ceiling's 8 ms: {priced_ms} ms");
        assert!(priced_ms >= FRAME_MS_AT_200_FPS, "the count up completion lands under one {FRAME_MS_AT_200_FPS} ms frame at a config the ceiling was written to stop");
        assert_eq!(tween_speed(Group::Screens), MAX_TWEEN_SPEED_PRODUCT, "the pair still multiplies once the scale is inside it");
        assert_eq!(duration_factor(Group::Screens), 1.0, "a group took headroom the clock levers had already spent");

        // The fork's own share of that channel: the ui clock it is allowed to run, times the factor it added.
        assert!(ui_clock_of(MAX_UI_ANIMATION_SCALE, produced) * time_scale_raise() <= MAX_TWEEN_SPEED_PRODUCT, "this fork's two levers still multiplied past the ceiling between them");

        // The armed door on that state, read through the wrapper the installed hook is built by: at the ceiling
        // it hands the game its own 0.16 s, and on the channel this fork holds that completion is the ceiling's
        // 8 ms rather than the fortieth of a frame run 17 printed.
        let door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOnThePairTrim;
        let handed_by_door = door(std::ptr::null_mut());
        assert!(ms_is(handed_by_door, 0.16), "the count-up door stopped handing the game its own 0.16 s at the pair ceiling");

        let door_ms = handed_by_door / shipped_delta_channel() * 1000.0;
        assert!(ms_is(door_ms, 160.0 / MAX_TWEEN_SPEED_PRODUCT), "the door's completion runs {door_ms} ms, not the pair ceiling's 8 ms");
        assert!(door_ms >= FRAME_MS_AT_200_FPS, "the count-up door hands a completion that closes in {door_ms} ms, under one {FRAME_MS_AT_200_FPS} ms frame");
        assert!(ms_is(door_ms, priced_ms), "the armed door closes in {door_ms} ms while the pair bound prices {priced_ms} ms");

        // The trim is on the channel, not on a group: the same scale beside a neutral ui slider leaves the group
        // the headroom the channel has left, because that is what the ceiling prices.
        mirror_config(&timing_config(MAX_FACTOR, MAX_FACTOR, MAX_FACTOR, 1.0));
        assert!(ms_is(duration_factor(Group::Screens), MAX_TWEEN_SPEED_PRODUCT / produced), "the scale the game is holding did not take headroom off the group factors");
        assert_eq!(tween_speed(Group::Screens), MAX_TWEEN_SPEED_PRODUCT, "the scale under a neutral ui clock still left a group at the ceiling");
    }

    // The half that keeps this a bound and not a slowdown: a write that reached nothing reaches no completion,
    // and a cap that only ever lowers a lever cannot turn the slider's 0.1 floor into a faster clock, nor trim
    // the lever under the neutral 1.0 to hold down a scale the game reached on its own.
    #[test]
    fn a_time_scale_that_raised_nothing_leaves_the_clock_at_what_the_runs_measured() {
        let _turn = pass_turn();

        // A process that has never written `Time.timeScale` is in the state every recorded run was in.
        assert!(ms_is(time_scale_produced(), 1.0), "a build that has written nothing holds a scale");
        assert!(ms_is(time_scale_raise(), 1.0), "a build that has written nothing holds a raise");

        // The two write rules, with no state involved: only a value the game wrote above 1.0 is raised (C12),
        // and a scale the game holds past the ceiling passes through. `produced` is what the layer hands the
        // setter and what the completion is divided by; `raise` is the share of it this fork added.
        for (game_value, lever, produced_expected, raise_expected) in [
            (0.0f32, MAX_TIME_SCALE, 1.0f32, 1.0f32),   // the game's pause, read as no speed-up
            (0.5, MAX_TIME_SCALE, 1.0, 1.0),            // its slow motion, the same
            (1.0, MAX_TIME_SCALE, 1.0, 1.0),            // every `1 -> 1 (lever x5)` line in the run logs
            (1.0, 1.0, 1.0, 1.0),                       // the shipped default
            (1.5, MAX_TIME_SCALE, MAX_TIME_SCALE, MAX_TIME_SCALE / 1.5),
            (2.0, MAX_TIME_SCALE, MAX_TIME_SCALE, 2.5),
            (4.0, MAX_TIME_SCALE, MAX_TIME_SCALE, 1.25),
            (5.0, MAX_TIME_SCALE, MAX_TIME_SCALE, 1.0),  // the ceiling already reached: nothing added
            (8.0, MAX_TIME_SCALE, 8.0, 1.0),             // run 19's cut at its own 8.0000, passed through
            (2.0, 1.0, 2.0, 1.0),                        // a neutral lever on a fast forward
            (f32::NAN, MAX_TIME_SCALE, 1.0, 1.0),
        ] {
            let handed = apply_time_scale(game_value, lever);
            note_time_scale_write(game_value, handed);

            assert!(ms_is(time_scale_produced(), produced_expected), "a {game_value} write with lever {lever} left {} in the game's clock, not {produced_expected}", time_scale_produced());
            assert!(ms_is(time_scale_raise(), raise_expected), "a {game_value} write with lever {lever} added {raise_expected}x and the mirror holds {}", time_scale_raise());
            assert!(ms_is(time_scale_raise_of(game_value, lever), raise_expected), "the pure write rule disagrees with what the mirror recorded for {game_value}/{lever}");
            assert!(time_scale_produced() >= 1.0, "a scale at or below the neutral one became a completion multiplier");

            // The fork's own share of the channel stays inside the ceiling at the widest ui lever it can be
            // handed, whatever the game asked for underneath it.
            assert!(ui_clock_of(MAX_UI_ANIMATION_SCALE, produced_expected) * raise_expected <= MAX_TWEEN_SPEED_PRODUCT, "this fork's share of the clock reached {}x at {game_value}/{lever}", ui_clock_of(MAX_UI_ANIMATION_SCALE, produced_expected) * raise_expected);
        }

        // The state every recorded run was in, rung by rung: Unity holding 1.0, so the clock and every headroom
        // are what the transition gap baselines and the 20x pair were measured on.
        for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 4.0, 5.0, 10.0, MAX_UI_ANIMATION_SCALE] {
            assert_eq!(ui_clock_of(ui, 1.0), ui, "a neutral scale changed the ui clock at {ui}");
            assert_eq!(delta_clock_of(ui, 1.0), ui, "a neutral scale changed the completion clock at {ui}");
        }

        // Any pair of the ui lever and a scale the write layer can hand the game, including the shipped
        // maximums: the channel is at most the pair ceiling, the ui half is never raised above what the config
        // asked for, never trimmed under the neutral 1.0 by this fork, and the completion is never slower than
        // the ui lever alone.
        for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 4.0, 5.0, 10.0, MAX_UI_ANIMATION_SCALE] {
            for scale in [1.0f32, 1.25, 2.0, 2.5, MAX_TIME_SCALE] {
                let clock = delta_clock_of(ui, scale);
                let cap = ui_clock_of(ui, scale);

                assert!(clock <= MAX_TWEEN_SPEED_PRODUCT, "{ui} under a Unity holding {scale} reached {clock}x on one completion");
                assert!(cap <= ui, "the cap raised the ui clock above the config at {ui}/{scale}");
                assert!(cap >= ui.min(1.0), "the cap trimmed the ui lever under where the config or the neutral clock put it at {ui}/{scale}");
                assert!(clock >= ui, "the scale under the clock made the completion slower than the ui lever alone at {ui}/{scale}");
            }
        }

        // A scale past the ceiling can only be there because the game asked for it, and the bound says what it
        // will not do about that: the cap floors at the neutral 1.0, the fork's share is nothing, and the scale
        // is priced as the speed the completion really ran at rather than hidden from the ceiling.
        assert!(ms_is(ui_clock_of(MAX_UI_ANIMATION_SCALE, 8.0), MAX_TWEEN_SPEED_PRODUCT / 8.0), "a game 8.0 did not take its share out of the ui clock: {}", ui_clock_of(MAX_UI_ANIMATION_SCALE, 8.0));
        assert!(ms_is(delta_clock_of(MAX_UI_ANIMATION_SCALE, 8.0), MAX_TWEEN_SPEED_PRODUCT), "a 20x ui lever under a game 8.0 is not priced as the {}x it runs at", delta_clock_of(MAX_UI_ANIMATION_SCALE, 8.0));
        assert!(ms_is(ui_clock_of(1.0, 40.0), 1.0), "the cap trimmed the ui lever to hold down a scale the game reached on its own: {}", ui_clock_of(1.0, 40.0));
        assert!(delta_clock_of(1.0, 40.0) >= 40.0, "a game scale past the ceiling disappeared from the clock the completion ran on");

        // The slider's floor is a slow down of the clock, and the cap never touches it.
        assert_eq!(ui_clock_of(MIN_UI_ANIMATION_SCALE, MAX_TIME_SCALE), MIN_UI_ANIMATION_SCALE, "the cap turned a slowed clock into a faster one");

        // A mirror that is not a number is the neutral clock on both halves.
        assert_eq!(ui_clock_of(f32::NAN, MAX_TIME_SCALE), 1.0, "a NAN ui mirror became a multiplier");
        assert_eq!(delta_clock_of(f32::NAN, 1.0), 1.0, "a NAN ui mirror became a clock with a scale in it");
        assert_eq!(ui_clock_of(20.0, f32::NAN), MAX_UI_ANIMATION_SCALE, "a NAN scale capped a clock it cannot price");
    }

    // The attribution of that frame, kept executable. 5 ms is what `target_fps 200` costs and what run 17 ran
    // at; it is not run 31's frame. Run 31 ran at `target_fps 60` and its own census counted 28,662 frames over
    // 484,037 ms on the screen it stalled on. A bar re-aimed at the stalled run's frame would flatter every
    // completion this module prices, so the strict bar stays and this test names where each figure comes from.
    #[test]
    fn the_frame_the_completion_assertions_hold_is_the_strictest_frame_any_c58_run_measured() {
        // 5.0 ms is the budget of a 200 fps frame, not a number read off a run.
        assert!(ms_is(FRAME_MS_AT_200_FPS, 1000.0 / 200.0), "{FRAME_MS_AT_200_FPS} is no longer the budget of a 200 fps frame");

        // Run 17's clock: 88,728 frames over 499,199 ms, 5.6 ms a frame, on `target_fps 200`. The bar is
        // stricter than the frame it is quoted from.
        let run17 = 499199.0 / 88728.0;
        assert!(run17 > FRAME_MS_AT_200_FPS && run17 < 6.0, "run 17's measured frame is {run17} ms, not the 5.6 ms its clock read");

        // And stricter than the 60 fps budget run 20, run 26 and run 31 were each played at, so no C58 reading
        // is priced against a frame looser than the one it ran on.
        assert!(FRAME_MS_AT_200_FPS < 1000.0 / 60.0, "the completion bar is looser than a 60 fps frame");

        // Run 31: `target_fps 60` in its snapshot, 28,662 frames over 484,037 ms on `view 1501`. It is a 16.9 ms
        // run, so a 5 ms claim about it names a setting that run never had.
        assert!(RUN31_FRAME_MS > 1000.0 / 60.0, "run 31's {RUN31_FRAME_MS} ms frame is faster than the 60 fps its snapshot reads");
        assert!(RUN31_FRAME_MS < 20.0, "run 31's frame is {RUN31_FRAME_MS} ms, not the 16.9 ms its census read");
        assert!(FRAME_MS_AT_200_FPS < RUN31_FRAME_MS, "the completion bar fell to the stalled run's own {RUN31_FRAME_MS} ms frame");

        let stricter = RUN31_FRAME_MS / FRAME_MS_AT_200_FPS;
        assert!(stricter > 3.0, "the bar is only {stricter}x stricter than run 31's frame");

        println!(
            "C58 item 62, the frame the completion assertions hold: {FRAME_MS_AT_200_FPS} ms at `target_fps 200`, run 17's measured {run17} ms at that setting, and run 31's {RUN31_FRAME_MS} ms at `target_fps 60` ({stricter}x stricter than the frame the stall sat in)",
        );
    }

    // The invariant the whole item exists to hold, over every config the three levers can be left in: no
    // completion an armed door prices lands under one frame, and no door runs slower than its own option asked.
    // The frame is the 5 ms budget of `target_fps 200`, the setting run 17 ran at, which is 3.4x stricter than
    // the 16.9 ms frame run 31 stalled on at `target_fps 60`. The rungs are written the way the
    // `Time.timeScale` write layer writes them - a value the game asked for and the lever on top of it, through
    // the shipped `apply_time_scale` - because a rung the writer cannot produce prices nothing:
    // `note_time_scale_write(1.0, 5.0)` is a pair `apply_time_scale` never hands the setter, it returns a value
    // at or below 1.0 unchanged, which is what `1 -> 1 (lever x5)` in every run log says.
    #[test]
    fn no_config_of_the_three_levers_hands_a_completion_under_one_frame() {
        let _turn = pass_turn();

        // A game_value above 1.0 is in every rung set: that is the state the ceiling was written for, and the
        // one a sweep of levers only never reaches.
        const RUNGS: [(f32, f32); 7] = [
            (1.0f32, 1.0f32),                 // the shipped default
            (1.0, MAX_TIME_SCALE),            // every `1 -> 1 (lever x5)` line in the run logs
            (2.0, 1.0),                       // the game's own fast forward, lever neutral
            (1.25, 2.0),                      // 2.5 produced, 2.0x of it added
            (2.0, MAX_TIME_SCALE),            // the C58 state: 5.0 produced, 2.5x of it added
            (4.0, MAX_TIME_SCALE),            // 5.0 produced, 1.25x of it added
            (5.0, MAX_TIME_SCALE),            // the game already at the ceiling: 5.0, nothing added
        ];

        let mut fastest = f32::MAX;

        for group in [Group::Transition, Group::Screens, Group::Story] {
            for configured in [1.0, 2.0, 5.0, MAX_FACTOR] {
                for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 4.0, 5.0, 10.0, MAX_UI_ANIMATION_SCALE] {
                    for (game_value, lever) in RUNGS {
                        mirror_config(&timing_config(configured, configured, configured, ui));

                        // Recorded the way the write layer records it: the game's own request and the number the
                        // layer handed the setter.
                        let produced = apply_time_scale(game_value, lever);
                        note_time_scale_write(game_value, produced);

                        assert!(ms_is(time_scale_produced(), produced.max(1.0)), "a {game_value} write at lever {lever} handed the setter {produced} and the mirror holds {}", time_scale_produced());
                        assert!(ms_is(time_scale_raise(), time_scale_raise_of(game_value, lever)), "the rung asked for the raise of a {game_value}/{lever} write and the mirror holds {}", time_scale_raise());

                        // The fork's own share of the channel, and the whole channel, both inside what the
                        // ceiling claims about them.
                        assert!(ui_clock_of(ui, produced) * time_scale_raise() <= MAX_TWEEN_SPEED_PRODUCT, "{configured} on a {ui} clock under a {game_value}/{lever} write put {}x of this fork's own levers on one completion", ui_clock_of(ui, produced) * time_scale_raise());
                        assert!(
                            completion_speed(group, Pace::Unproven) <= MAX_TWEEN_SPEED_PRODUCT,
                            "{configured} on a {ui} clock under a {produced} scale reached {}x on one completion",
                            completion_speed(group, Pace::Unproven),
                        );
                        assert!(
                            duration_factor(group) >= 1.0,
                            "the bound turned a speed up into a slower animation than the game asked for at {configured}/{ui}/{game_value}x{lever}",
                        );

                        // The number the ceiling prices and the number the detour runs are one fact: the ui cap
                        // `tween_clocks` applies, times the `Time.timeScale` the delta argument carries.
                        assert_clock_is_the_shipped_channel("the rung");

                        let raw = C58_TRAINING_ROWS[2].1;
                        let completion = completion_ms(raw, group);
                        assert!(
                            completion >= FRAME_MS_AT_200_FPS,
                            "the count up completion closed in {completion} ms under one {FRAME_MS_AT_200_FPS} ms frame at {configured}/{ui}/{game_value}x{lever}",
                        );
                        // The shape of the bound: a completion never runs faster than one `MAX_TWEEN_SPEED_PRODUCT`
                        // of the duration the game handed in, whatever Unity is holding underneath the ui lever.
                        assert!(
                            completion + 0.01 >= raw / MAX_TWEEN_SPEED_PRODUCT * 1000.0,
                            "a completion ran {completion} ms, faster than {raw} s divided by the {MAX_TWEEN_SPEED_PRODUCT}x ceiling, at {configured}/{ui}/{game_value}x{lever}",
                        );
                        fastest = fastest.min(completion);

                        // A door the clock layer is measured not to reach is untouched by all three levers.
                        assert_eq!(
                            duration_factor_at(group, Pace::OffTweenClock("rung walk")), factor(group),
                            "a door off the tween clock was trimmed at {configured}/{ui}/{game_value}x{lever}",
                        );

                        // The training gates have no factor, so the whole of what reaches them is the clock,
                        // and the clock is bounded: the game's own 1.0 s plate beat stays a beat.
                        assert_eq!(plate_cascade_handoff(1.0), 1.0, "a training gate moved its own interval at {configured}/{ui}/{game_value}x{lever}");
                        assert!(
                            completion_ms(1.0, Group::Training) >= FRAME_MS_AT_200_FPS,
                            "a 1.0 s training beat closed in {} ms at {configured}/{ui}/{game_value}x{lever}", completion_ms(1.0, Group::Training),
                        );
                    }
                }
            }
        }

        assert!(fastest >= C58_TRAINING_ROWS[2].1 / MAX_TWEEN_SPEED_PRODUCT * 1000.0 - 0.01, "the fastest completion any rung handed is {fastest} ms");

        println!(
            "C58 item 62, the three levers together: the fastest count-up completion any rung hands is {fastest} ms ({}x, ui_animation {MAX_UI_ANIMATION_SCALE} under the {MAX_TIME_SCALE} Unity holds after a 2.0 write at lever {MAX_TIME_SCALE}), against a {FRAME_MS_AT_200_FPS} ms frame",
            MAX_TWEEN_SPEED_PRODUCT,
        );
    }

    // C58 / ledger item 62, on the door a career session actually reached. The bound prices a group lever
    // against the clock layer, and the price is that a value which is only the length of a coroutine's wait is
    // trimmed by a clock that never reaches it. Paying that price is the shipped state, because the
    // alternative is the state the item was written to stop: a `Group::Screens` door handing out its whole
    // `MAX_FACTOR` beside a clock already at its own ceiling. A door leaves the trim on a measurement a run
    // reads, not on a classification taken from a signature dump.
    #[test]
    fn the_count_up_door_a_career_session_reaches_hands_the_trim_bounded_value() {
        let _turn = pass_turn();

        // The state run 17's snapshot recorded: `result 20`, `ui_animation 20`.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));

        // A completion the clock measures is already at the ceiling, so the door hands 1.0 on.
        assert_eq!(scale_duration(1.0, Group::Screens), 1.0, "a door took a factor the clock had already spent");
        assert!(ms_is(completion_ms(1.0, Group::Screens), 1000.0 / MAX_TWEEN_SPEED_PRODUCT), "a bounded completion no longer runs at the pair ceiling");

        // The armed door, built by the same macro the installed hook uses, driven with the 0.16 s the game
        // answers with. A test that only read `scale_duration` would still pass with this door on a scale that
        // skips the trim; this one reads the door.
        let door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOnThePairTrim;
        let handed = door(std::ptr::null_mut());

        // What the same door hands with the trim taken off it: the row C58's table prints.
        let unbounded = 0.16 / MAX_FACTOR;
        let bounded_ms = handed / ui_animation_scale() * 1000.0;
        let unbounded_ms = unbounded / ui_animation_scale() * 1000.0;

        println!(
            "C58 item 62, count-up door at result 20 / ui_animation 20: the door hands {handed} s, {bounded_ms} ms on a {}x clock. An untrimmed lever hands {unbounded} s, {unbounded_ms} ms, a fortieth of a {FRAME_MS_AT_200_FPS} ms frame.",
            ui_animation_scale(),
        );

        assert_eq!(handed, 0.16, "the count-up door stopped handing the game its own 0.16 s at the pair ceiling");
        assert!(ms_is(bounded_ms, 160.0 / MAX_TWEEN_SPEED_PRODUCT), "the bounded count-up no longer completes at the pair ceiling");
        assert!(bounded_ms >= FRAME_MS_AT_200_FPS, "the bounded count-up lands under one {FRAME_MS_AT_200_FPS} ms frame");

        // The state the item exists to stop, kept as arithmetic so the door cannot drift back into it without
        // a test saying so.
        assert!(ms_is(unbounded * 1000.0, 8.0), "the untrimmed reading no longer matches what run 17 printed");
        assert!(unbounded_ms < FRAME_MS_AT_200_FPS, "the untrimmed pair no longer lands the count-up under a frame");
    }

    #[test]
    fn the_count_up_completion_the_pair_bound_prices_never_lands_inside_a_frame_at_any_rung() {
        let _turn = pass_turn();

        for group in [Group::Transition, Group::Screens, Group::Story] {
            for configured in [1.0, 2.0, 5.0, MAX_FACTOR] {
                for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 4.0, 5.0, 10.0, MAX_UI_ANIMATION_SCALE] {
                    mirror_config(&timing_config(configured, configured, configured, ui));

                    assert!(
                        tween_speed(group) <= MAX_TWEEN_SPEED_PRODUCT,
                        "{configured} against a {ui} clock reached {}x on one completion", tween_speed(group)
                    );
                    assert!(
                        duration_factor(group) >= 1.0,
                        "the bound turned a speed up into a slower animation than the game asked for at {configured}/{ui}"
                    );

                    // The invariant an unproven exception broke: the count-up length the runs read is never
                    // handed to a completion that lands inside a frame, whatever either slider says.
                    let completion = completion_ms(0.16, group);
                    assert!(
                        completion >= FRAME_MS_AT_200_FPS,
                        "the count up completion is {completion} ms at {configured}/{ui}, under one {FRAME_MS_AT_200_FPS} ms frame"
                    );

                    if group == Group::Screens {
                        // Read through the door. The ladder that priced only `scale_duration` kept passing
                        // while this door handed the game its whole group factor, which is the half of item
                        // 62 that went unread: an armed door and the bound it is supposed to sit under have
                        // to agree at every rung.
                        let door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOnThePairTrim;
                        let through_door = door(std::ptr::null_mut()) / ui_animation_scale() * 1000.0;
                        assert!(
                            ms_is(through_door, completion),
                            "the count-up door hands a {through_door} ms completion at {configured}/{ui} while the pair bound prices {completion} ms"
                        );
                        assert!(
                            through_door >= FRAME_MS_AT_200_FPS,
                            "the count-up door hands a {through_door} ms completion at {configured}/{ui}, under one {FRAME_MS_AT_200_FPS} ms frame"
                        );
                    }

                    // Training gates hold: a group with no lever hands the game its own value at every rung
                    // (C58, item 59).
                    assert_eq!(scale_duration(1.0, Group::Training), 1.0, "a training gate took a factor at {configured}/{ui}");
                }
            }
        }

        // At the shipped options the door is inert: the same value a build without this item handed out, and
        // the values `scale_duration` already declines to touch stay declined.
        mirror_config(&Config::default());
        assert_eq!(scale_duration(0.16, Group::Screens), 0.16, "the shipped default changed a duration door");
        assert_eq!(scale_duration(0.0, Group::Screens), 0.0, "a zero the game handed in became an invented duration");
        assert!(scale_duration(f32::NAN, Group::Screens).is_nan(), "NAN reached a duration door");
    }

    // C58 / ledger item 62, the half the first fix of this item got backwards. A ceiling on the product of two
    // layers is only a ceiling on a completion both layers reach. `duration_factor` priced a `Group`, and a
    // group names which option wrote a lever: it never said what the value paces, so the trim reached a door
    // whose completion the clock layer never advances, and there it is not a ceiling at all but a slowdown -
    // the door hands on, the completion takes the game's own duration, and the wall clock is `MAX_FACTOR` times
    // what the same door handed every run in this ledger. This is the door built by the shipped macro on the
    // lane a measurement moves a door to, driven with the same 0.16 s the count-up door is reached with.
    #[test]
    fn a_door_the_tween_clock_never_reaches_keeps_its_groups_whole_factor() {
        let _turn = pass_turn();

        // The state run 17's snapshot recorded: `result 20`, `ui_animation 20`.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));

        let off_clock_door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOffTheTweenClock;
        let bounded_door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOnThePairTrim;

        let off_clock_handed = off_clock_door(std::ptr::null_mut());
        let bounded_handed = bounded_door(std::ptr::null_mut());

        // Wall clock of each completion. The off clock completion is not divided by the tween clock, because
        // the tween clock is not on it: that is the whole content of the pace.
        let off_clock_ms = off_clock_handed * 1000.0;
        let bounded_ms = bounded_handed / ui_animation_scale() * 1000.0;

        // What the trim would have cost this door: it hands 1.0 on, and with no clock under the duration the
        // completion takes the game's own 160 ms instead of the 8 ms its option asked for.
        let trimmed_off_clock_ms = scale_duration(0.16, Group::Screens) * 1000.0;

        println!(
            "C58 item 62, the 0.16 s at result 20 / ui_animation 20: off the tween clock the door hands {off_clock_handed} s and completes in {off_clock_ms} ms; on it the door hands {bounded_handed} s and completes in {bounded_ms} ms on a {}x clock. Trimmed as if the pair composed where it does not, the same door waits {trimmed_off_clock_ms} ms, {}x slower than the speed its own option asked for.",
            ui_animation_scale(), trimmed_off_clock_ms / off_clock_ms,
        );

        // The lane hands the group's whole ask, and the ceiling is held by that factor alone.
        assert!(ms_is(off_clock_handed, 0.16 / MAX_FACTOR), "a door off the tween clock did not hand its group's whole factor");
        assert!(ms_is(off_clock_ms, 160.0 / MAX_FACTOR), "the off clock completion is not the {}x its own layer allows", MAX_FACTOR);

        // And the bound it replaces is not the same door run slower: the trimmed reading is the state this
        // half of the item exists to stop, kept as arithmetic so the door cannot drift back into it.
        assert!(ms_is(trimmed_off_clock_ms, 160.0), "a trimmed off clock door no longer waits the game's own duration");
        assert!(ms_is(trimmed_off_clock_ms / off_clock_ms, MAX_FACTOR), "the trim is no longer a twenty fold slowdown on a completion the clock never reaches");

        // The bounded lane is unchanged by this: the pair is real on it, so the door still hands on and the
        // completion still runs at the ceiling.
        assert_eq!(bounded_handed, 0.16, "the bounded lane stopped holding the count-up door at the pair ceiling");
        assert!(ms_is(bounded_ms, 160.0 / MAX_TWEEN_SPEED_PRODUCT), "the bounded completion no longer runs at the pair ceiling");

        // At every rung of both sliders: a door off the clock is never trimmed by it, never slower than its
        // own option asked, and never faster than the ceiling either lane is written to hold.
        for group in [Group::Transition, Group::Screens, Group::Story] {
            for configured in [1.0, 2.0, 5.0, MAX_FACTOR] {
                for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 4.0, 5.0, 10.0, MAX_UI_ANIMATION_SCALE] {
                    mirror_config(&timing_config(configured, configured, configured, ui));

                    let pace = Pace::OffTweenClock("rung walk");

                    assert_eq!(
                        duration_factor_at(group, pace), factor(group),
                        "a door off the tween clock was trimmed to {}/{ui}'s headroom at {configured}/{ui}", duration_factor_at(group, pace)
                    );
                    assert!(
                        completion_speed(group, pace) <= MAX_TWEEN_SPEED_PRODUCT,
                        "the off clock lane reached {}x on a completion its own layer already bounds", completion_speed(group, pace)
                    );
                    assert!(
                        completion_speed(group, pace) >= factor(group),
                        "the bound slowed a completion the clock layer never reaches below what its option asked at {configured}/{ui}"
                    );
                    assert!(
                        completion_speed(group, pace) >= 1.0,
                        "a door off the tween clock ran slower than the game's own duration at {configured}/{ui}"
                    );
                    assert!(
                        completion_speed(group, Pace::Unproven) <= MAX_TWEEN_SPEED_PRODUCT,
                        "the bounded lane reached {}x on one completion at {configured}/{ui}", completion_speed(group, Pace::Unproven)
                    );
                }
            }
        }

        // Inert at the shipped options, on both lanes.
        mirror_config(&Config::default());
        assert_eq!(scale_paced(0.16, Group::Screens, Pace::Unproven), 0.16, "the shipped default changed a bounded door");
        assert_eq!(scale_paced(0.16, Group::Screens, Pace::OffTweenClock("inert")), 0.16, "the shipped default changed an off clock door");
    }

    // The bound reaches a door through the pace written at that door. The table is what `note_pair_ceiling`
    // prints door by door in `hachimi.log`, so the lanes a career run reads and the lanes the shipped code
    // trims on are one fact, and a door that is armed but not written down is a door no bound reaches.
    #[test]
    fn every_armed_duration_door_states_the_pace_its_bound_reaches_it_on() {
        let _turn = pass_turn();

        let countup = ARMED_DOOR_PACES
            .iter()
            .find(|(door, _, _)| *door == COUNTUP_DOOR)
            .expect("CountupModifier.get_Duration is not in ARMED_DOOR_PACES");

        // The one `Group::Screens` duration door a career session has been measured reaching starts bounded, and
        // it starts bounded because nothing has measured it otherwise - not because a signature was read as a
        // classification.
        assert_eq!(countup.1, Group::Screens, "the count up door is no longer in the group result_screen_speed writes");
        assert!(countup.2.bounds_the_pair(), "the count up door left the pair bound");
        assert!(countup.2.evidence().is_none(), "an unproven pace carries evidence it does not have");

        let lanes = armed_door_lanes();

        println!("C58 item 62, the lanes hachimi.log prints at a binding clock: {lanes}");

        // A door off the bound is not a forbidden place, it is an earned one, and `note_countup_completion` is
        // the only writer of a measured lane. So the two facts a run could otherwise see drift apart - the lane
        // the log prints and the lane the door scales on - are asserted to be one fact, in either state. A gate
        // that asserted the off clock lane stays empty forever is what made this bound unprovable: no door could
        // leave it without editing the gate. A gate that asserts only a measurement can fill it keeps the bound
        // and leaves the door movable.
        // A door standing in the off clock lane is not a gate failure, it is a door a reading put there. What
        // this asserts is that the lane the log prints is the lane the door scales on, for the door whose lane a
        // run can move, and that every door standing off the bound names the run reading that put it there.
        let countup_lane = pace_lane(armed_door_pace(COUNTUP_DOOR, countup.2));

        assert!(
            lanes.contains(COUNTUP_DOOR) && lanes.contains(countup_lane),
            "the lane hachimi.log prints and the lane the door scales on are different facts: {lanes}"
        );

        if matches!(countup_pace(), Pace::Unproven) {
            assert!(
                lanes.contains(PACE_LANE_UNPROVEN) && lanes.contains(COUNTUP_DOOR),
                "the log does not say the count up door is bounded: {lanes}"
            );
        }
        else {
            let measured_lane = pace_lane(countup_pace());
            let unproven: Vec<&str> = ARMED_DOOR_PACES
                .iter()
                .filter(|(door, _, listed)| pace_lane(armed_door_pace(door, *listed)) == PACE_LANE_UNPROVEN)
                .map(|(door, _, _)| *door)
                .collect();

            assert!(lanes.contains(measured_lane) && lanes.contains(COUNTUP_DOOR), "a door a run measured is missing from the lane line: {lanes}");
            assert!(!unproven.contains(&COUNTUP_DOOR), "a door a run measured is still printed as unproven: {lanes}");
        }

        for (door, _, listed) in ARMED_DOOR_PACES {
            // A door off the bound in the shipped state carries the reading that put it there in the pace
            // itself, not in a comment beside it.
            if let Pace::OffTweenClock(proof) = armed_door_pace(door, *listed) {
                assert!(
                    proof.contains("run "),
                    "{door} stands off the pair bound without naming the run reading that put it there: {proof}"
                );
            }
        }

        for (door, group, listed) in ARMED_DOOR_PACES {
            // The door as it actually stands, not as the table was written.
            let pace = armed_door_pace(door, *listed);

            // A door only leaves the bound with the reading that puts it there, carried in the pace itself.
            if !pace.bounds_the_pair() {
                assert!(
                    matches!(pace.evidence(), Some(line) if !line.is_empty()),
                    "{door} stands off the pair bound with nothing saying why"
                );
            }

            for configured in [1.0, MAX_FACTOR] {
                for ui in [1.0, 2.0, MAX_UI_ANIMATION_SCALE] {
                    mirror_config(&timing_config(configured, configured, configured, ui));

                    let speed = completion_speed(*group, pace);

                    assert!(
                        speed <= MAX_TWEEN_SPEED_PRODUCT,
                        "{door} reached {speed}x on one completion at {configured}/{ui}",
                    );
                    assert!(speed >= 1.0, "{door} ran slower than the game's own value at {configured}/{ui}");

                    if !pace.bounds_the_pair() {
                        assert!(ms_is(speed, factor(*group)), "{door} was trimmed by a clock that is not on its completion at {configured}/{ui}");
                    }
                }
            }
        }

        // Every door the training census counts by name is in the table, so a run reading
        // `TrainingParamChangeUI.InitializePlateList=36` can find the lane its bound reaches it on.
        for (_, name) in TRAINING_HIT_SLOTS {
            assert!(
                ARMED_DOOR_PACES.iter().any(|(door, _, _)| *door == name),
                "{name} is armed and names no pace",
            );
        }
    }

    // C58 / item 62, the armed `Group::Screens` doors that were once written on the measured lane. The dump
    // citation that put them there named `introspect.log:16004`, and 16004 is `field <TimeScale>k__BackingField`,
    // the backing field of the component's own scale; `_tweener [class<DG.Tweening.Tweener>]` is 16000. The block
    // that was cited names both clocks, `field _tweener` (16000) built by `GetTweener/0` (15972) and the
    // component's own step `LateUpdate/0` + `UpdateTime/0` (15979-15980) over `_totalTime`/`_internalTime`/
    // `_lastInternalTime`/`_frameCount` (15991-15995) under its own `TimeScale` (15961-15962, 16004), and a field
    // being present says what a class holds, not which half measures a completion. So these doors are `Unproven`:
    // bounded, and carrying no lane claim.
    #[test]
    fn a_door_whose_class_block_names_both_clocks_is_bounded_without_claiming_a_lane() {
        let _turn = pass_turn();

        const TEXT_MODIFIER_DOORS: [&str; 2] = ["TextModifier.get_Duration", "TextModifier.get_Delay"];

        for door in TEXT_MODIFIER_DOORS {
            let row = ARMED_DOOR_PACES
                .iter()
                .find(|(name, _, _)| *name == door)
                .expect("a TextModifier duration door is not in ARMED_DOOR_PACES");

            assert_eq!(row.1, Group::Screens, "{door} is no longer in the group result_screen_speed writes");
            assert!(row.2.bounds_the_pair(), "{door} left the pair bound on a reading nobody measured");
            assert!(row.2.evidence().is_none(), "{door} states a lane the dump does not show");
        }

        // The bound still reaches both doors, and it reaches them with the number the claimed lane produced: no
        // door moved out of the trim, so the 400x state item 62 exists to bound stays closed and nothing a run
        // measured at these doors got slower.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));
        assert_clock_is_the_shipped_channel("the text modifier doors at result 20 / ui_animation 20");

        let claimed = Pace::TweenMeasured("the lane these doors were written on until this finding");

        assert_eq!(
            duration_factor_at(Group::Screens, PACE_TEXT_MODIFIER_TIMELINE),
            duration_factor_at(Group::Screens, claimed),
            "naming the lane unproven changed what the doors hand the game",
        );

        let handed = scale_paced(1.0, Group::Screens, PACE_TEXT_MODIFIER_TIMELINE);
        let completion = completion_speed(Group::Screens, PACE_TEXT_MODIFIER_TIMELINE);
        let ms = completion_ms(1.0, Group::Screens);

        println!(
            "C58 item 62, the text modifier doors: at result 20 / ui_animation 20 a 1 s timeline length is handed {handed} s and closes in {ms} ms, {completion}x, against the 400x the two ceilings multiply to unbounded.",
        );

        assert_eq!(handed, 1.0, "the pair stopped being held at these doors");
        assert!(ms_is(completion, MAX_TWEEN_SPEED_PRODUCT), "these completions run {completion}x, not the {}x the ceiling they sit under states", MAX_TWEEN_SPEED_PRODUCT);
        assert!(ms >= FRAME_MS_AT_200_FPS, "a bounded completion lands under one {FRAME_MS_AT_200_FPS} ms frame: {ms} ms");

        // The line a career run reads names both doors as bounded and names no channel for them.
        let lanes = armed_door_lanes();

        let bounded: Vec<&str> = ARMED_DOOR_PACES
            .iter()
            .filter(|(name, _, listed)| pace_lane(armed_door_pace(name, *listed)) == PACE_LANE_UNPROVEN)
            .map(|(name, _, _)| *name)
            .collect();

        for door in TEXT_MODIFIER_DOORS {
            assert!(bounded.contains(&door), "hachimi.log does not say {door} is bounded: {lanes}");
        }
    }

    // What a door needs before it leaves the bound: a completion wall time, read in a run. `CountupModifier`'s
    // `OnPlay/0` and `OnComplete/0` bracket one count-up on the same instance its `get_Duration` is called on,
    // and this is the arithmetic that turns that pair, the number the door's hit line printed, and the clock in
    // the same config snapshot into a verdict.
    #[test]
    fn a_measured_completion_wall_time_is_what_moves_a_door_off_the_bound() {
        // A 1 s fade handed 1 s and done in 50 ms ran through a 20x clock: composed, and the pair bound is the
        // right instrument on it.
        assert!(matches!(measured_pace(1.0, 0.05, 20.0), Some(Pace::TweenMeasured(_))));

        // The count-up door's own numbers: handed 0.16 s, closed 160 ms later. The 20x clock made no
        // difference to that completion, so it is on a channel the clock layer does not reach, and only a
        // reading like this puts the door there.
        let verdict = measured_pace(0.16, 0.16, 20.0);

        println!(
            "C58 item 62, the measurement the count up door owes: handed 0.16 s, closed in 160 ms at ui_animation 20 -> {verdict:?}. Handed 0.16 s, closed in 8 ms -> {:?}, which is a completion inside one 5 ms frame at target_fps 200 and says nothing.",
            measured_pace(0.16, 0.008, 20.0),
        );

        assert!(matches!(verdict, Some(Pace::OffTweenClock(_))));

        // A completion under the frame budget this fork measures at, a clock too near neutral to separate the
        // two candidates, a completion in between them, and inputs that are not numbers all say nothing, and
        // nothing is the door staying bounded.
        assert!(measured_pace(0.16, 0.008, 20.0).is_none(), "a completion inside a frame was read as a channel");
        assert!(measured_pace(1.0, 0.5, 2.0).is_none(), "a clock near neutral was read as a separation");
        assert!(measured_pace(1.0, 0.2, 20.0).is_none(), "a completion part of the way was read as a clean channel");
        assert!(measured_pace(0.0, 0.1, 20.0).is_none(), "a zero the game handed in became a classification");
        assert!(measured_pace(f32::NAN, 0.1, 20.0).is_none());
        assert!(measured_pace(0.16, 0.16, f32::NAN).is_none());

        // A wall time the handed duration does not account for is not a clean off clock reading either. A
        // 6 s `countUpDelay` (introspect.log:24189-24192) ahead of a 0.16 s count-up at `ui_animation 20` closes
        // 308 ms later, and calling that "off the tween clock" would hand a 20x speed-up to a completion the
        // run has not shown the clock is absent from.
        assert!(measured_pace(0.16, 0.48, 20.0).is_none(), "a completion three times the handed duration was read as a channel");
        assert!(measured_pace(0.16, 30.0, 20.0).is_none(), "a completion the door's duration does not account for was read as a channel");
        assert!(measured_pace(0.16, 0.20, 20.0).is_some(), "a 0.16 s yield polled one frame late was not read as off the clock");

        // The verdict a run reads is not a licence on its own: it names the lane, and the door moves by being
        // written on it with that line, which is what `Pace::OffTweenClock` carries.
        if let Some(Pace::OffTweenClock(line)) = verdict {
            assert!(!line.is_empty(), "a door moved off the bound carrying no evidence");
        }
    }

    // C58 / ledger item 62, the half the check log says the tree shipped no way to take: the bracket whose
    // reading the door's pace depends on is armed in the shipped tree (`CountupModifier_OnPlay`,
    // `CountupModifier_OnComplete`, on `introspect.log:15918-15919`), the armed door feeds it
    // (`countup_screen_duration` calls `note_countup_handed`), `measured_pace` decides, and the decision is
    // what the door scales with. Driven through the same three functions the two wrappers call, with the door
    // built by the shipped macro, at the pair state run 17 ran on.
    #[test]
    fn the_shipped_countup_bracket_moves_the_door_off_the_bound_it_starts_on() {
        let _turn = pass_turn();

        // `result 20`, `ui_animation 20`, the snapshot run 17 printed `0.16 -> 0.008` beside.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));

        let door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOnThePairTrim;

        assert_eq!(door(std::ptr::null_mut()), 0.16, "the count-up door was not bounded before any measurement");
        assert_eq!(countup_pace(), PACE_COUNTUP, "the door's pace did not start at the dump-backed default");

        // Instances the bracket only compares. Nothing here dereferences them: the bracket is a pairing of
        // addresses, not a read of an object.
        static INSTANCE_ONE: u8 = 1;
        static INSTANCE_TWO: u8 = 2;
        let one = &INSTANCE_ONE as *const u8 as *mut Il2CppObject;
        let two = &INSTANCE_TWO as *const u8 as *mut Il2CppObject;

        // One count-up on the instance the door handed: 0.16 s handed, closed 160 ms later. The 20x clock made
        // no difference to that completion, so it ran on a channel the clock layer does not reach.
        note_countup_handed(0.16, 0.16);
        assert!(countup_play(one, 1_000));

        let first = countup_complete(one, 1_000 + 160_000_000);

        assert!(matches!(first, Some(Pace::OffTweenClock(_))), "a completion that ran at the duration it was handed did not read as off the clock");
        assert_eq!(countup_pace(), PACE_COUNTUP, "one completion moved a door the dump puts consumers of on both channels");
        assert_eq!(door(std::ptr::null_mut()), 0.16, "the door left the pair bound on a single sample");

        // A second completion, on another instance, saying the same thing.
        note_countup_handed(0.16, 0.16);
        assert!(countup_play(two, 5_000));
        assert!(matches!(countup_complete(two, 5_000 + 160_000_000), Some(Pace::OffTweenClock(_))));

        let pace = countup_pace();

        assert!(matches!(pace, Pace::OffTweenClock(line) if !line.is_empty()), "a repeated reading never reached the door's pace");

        // The same door, nothing else about it changed, hands its group's whole ask: the 0.008 s run 17 read out
        // of the client, and off the multiplied clock that completion is 8 ms - the speed the option asked for,
        // not the 160 ms the trim had it waiting.
        let handed = door(std::ptr::null_mut());

        println!(
            "C58 item 62, the count-up door after two off clock completions: it hands {handed} s and that completion runs {} ms. Bounded, the same door handed 0.16 s and the same completion waited 160 ms, {}x slower than its own option asked.",
            handed * MS_PER_SECOND, 160.0 / (handed * MS_PER_SECOND),
        );

        assert!(ms_is(handed, 0.16 / MAX_FACTOR), "the measured door did not hand its group's whole factor");
        assert!(ms_is(handed * MS_PER_SECOND, 160.0 / MAX_FACTOR), "the measured completion is not the {}x the door's own layer allows", MAX_FACTOR);
        assert!(handed * MS_PER_SECOND >= FRAME_MS_AT_200_FPS, "the measured completion lands inside one {FRAME_MS_AT_200_FPS} ms frame");

        // The lane a career run reads moves with the door: one fact, read from `countup_pace`.
        let lanes = armed_door_lanes();

        println!("C58 item 62, the lanes before and after the measurement: {lanes}");

        assert!(lanes.contains(PACE_LANE_OFF_CLOCK) && lanes.contains(COUNTUP_DOOR), "the lane line does not say the door moved: {lanes}");

        // And with the door moved, the bound it left is still the ceiling on the completion it now runs at: the
        // group factor alone, never more.
        assert!(completion_speed(Group::Screens, pace) <= MAX_TWEEN_SPEED_PRODUCT, "the off clock door left the ceiling it was bounded to hold");
        assert!(completion_speed(Group::Screens, pace) >= factor(Group::Screens), "the bound slowed the measured door below what its own option asked");
    }

    // The other side of the same instrument: what the bracket refuses to call a channel, because a door that is
    // not cleanly on one channel is a door that keeps its bound. Every one of these is a reading the shipped
    // bracket produces and `measured_pace` declines.
    #[test]
    fn a_countup_bracket_that_is_not_cleanly_on_one_channel_keeps_the_door_bounded() {
        let _turn = pass_turn();

        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE));

        static INSTANCE_ONE: u8 = 1;
        static INSTANCE_TWO: u8 = 2;
        let one = &INSTANCE_ONE as *const u8 as *mut Il2CppObject;
        let two = &INSTANCE_TWO as *const u8 as *mut Il2CppObject;

        // No door call yet, so no duration to price a completion with, and no bracket opens. Same for a bracket
        // with no clock behind it, which is what a build that never armed the bracket holds.
        assert!(!countup_play(one, 1_000), "a bracket opened with no duration handed behind it");
        assert!(countup_complete(one, 2_000).is_none());
        note_countup_handed(0.16, 0.16);
        assert!(!countup_play(two, 0), "a bracket timed against no clock opened");

        let door: extern "C" fn(*mut Il2CppObject) -> f32 = CountupDoorOnThePairTrim;

        // A completion inside one 5 ms frame at `target_fps 200`: 0.16 s handed, 8 ms of wall clock. That is the
        // pair ceiling doing its job, and it says nothing about which channel it ran on.
        assert!(countup_play(one, 3_000));
        assert!(countup_complete(one, 3_000 + 8_000_000).is_none(), "a completion inside a frame was read as a channel");

        // A completion three times the duration it was handed: a start delay ahead of the animation is inside
        // that wall time and not inside the door's duration.
        assert!(countup_play(two, 4_000));
        assert!(countup_complete(two, 4_000 + 480_000_000).is_none(), "a completion three times the handed duration was read as a channel");

        // A closure that found no open bracket on its instance: two count-ups finished out of the order the
        // single bracket could pair, and neither became a reading.
        assert!(countup_complete(two, 5_000).is_none(), "a closure with no open bracket behind it became a reading");

        let (completions, unmatched) = countup_bracket_counts();

        println!("C58 item 62, a bracket that says nothing: {completions} completions closed, {unmatched} closures with no open bracket on that instance");

        assert_eq!(countup_pace(), PACE_COUNTUP, "a reading that said nothing moved the door");
        assert_eq!(door(std::ptr::null_mut()), 0.16, "the door left the pair bound on a reading that said nothing");

        // A clean off clock reading, then two clean clock readings: a run that answers both ways has named a
        // count-up consumer, not this door, and the door stays on the side that cannot re-open 400x. The clock
        // readings are a 1.0 s count-up length handed at `ui_animation 20` closing 50 ms later, which is that
        // duration over the clock the fork handed the tween library.
        note_countup_handed(0.16, 0.16);
        assert!(countup_play(one, 10_000));
        assert!(matches!(countup_complete(one, 10_000 + 160_000_000), Some(Pace::OffTweenClock(_))));

        for at in [20_000, 30_000] {
            note_countup_handed(1.0, 1.0);
            assert!(countup_play(two, at));
            assert!(
                matches!(countup_complete(two, at + 50_000_000), Some(Pace::TweenMeasured(_))),
                "a completion at the speed the clock was set to did not read as clock measured",
            );
        }

        assert_eq!(countup_pace(), PACE_COUNTUP, "a door moved off the bound on readings that contradict each other");
        assert_eq!(door(std::ptr::null_mut()), 0.16, "the door handed its whole group factor on a contradicted reading");

        // Two clean clock readings with nothing contradicting them hold the door at the ceiling, on a
        // measurement: the bound stays, and the log says it is now held on a reading rather than on an absence.
        COUNTUP_OFF_SAMPLES.store(0, Ordering::Release);
        COUNTUP_TWEEN_SAMPLES.store(0, Ordering::Release);

        for at in [40_000, 50_000] {
            note_countup_handed(1.0, 1.0);
            assert!(countup_play(one, at));
            assert!(matches!(countup_complete(one, at + 50_000_000), Some(Pace::TweenMeasured(_))));
        }

        let pace = countup_pace();

        println!(
            "C58 item 62, a clock measured verdict: the door stands on {pace:?} and hands {} s for the game's own 0.16 s",
            door(std::ptr::null_mut()),
        );

        assert!(matches!(pace, Pace::TweenMeasured(_)), "a clock measured reading did not name the lane it names");
        assert!(pace.bounds_the_pair(), "a clock measured verdict left the door unbounded");
        assert_eq!(door(std::ptr::null_mut()), 0.16, "the door stopped holding the pair ceiling it was measured onto");
    }

    // The install line a run reads when one end of the bracket is not there. One door of a bracket measures
    // nothing, and that has to be a fact the log states rather than one a reader infers from the absence of a
    // completion line (A4).
    #[test]
    fn a_bracket_with_one_end_unresolved_says_so_on_its_own_line() {
        assert_eq!(bracket_door_state(0x1_0000), "armed");
        assert_eq!(bracket_door_state(0), "is there with no method this wrapper can stand on");

        // Two doors, two hook ids: the two ends of one bracket are not armed through one shared helper, which
        // would give them one `disabled_hooks` key and put both ends down on a key meant for one (C27).
        assert_ne!(
            CountupModifier_OnPlay as *const (),
            CountupModifier_OnComplete as *const (),
            "the two ends of the count-up bracket are one wrapper",
        );
    }

    // C58 / ledger item 59, spelled on the doors. The three rows are the ones the runs read beside
    // `result 20`: `PlayIn 2.4 -> 0.120000005` (run 17, run 26), `InitializePlateList 1 -> 0.05` (run 20,
    // run 31) and `CountupModifier_getDuration 0.16 -> 0.008` (run 17). Two of them are training gates and
    // are no longer scaling points at all; the third is a reward screen count-up and stays in the group its
    // option names.
    #[test]
    fn no_speed_option_the_config_editor_offers_reaches_a_training_gate() {
        let _turn = pass_turn();

        // Every state that puts the result screen group at its ceiling, over the clock ladder the sliders
        // and the preset arms cover: the `All levers` pair (`result 20 / ui_animation 20`), which is the arm
        // run 31 clicked at 09:43:30, five seconds before its plate door printed `1 -> 0.05`
        // (`run log/hachimi-run31.log` lines 1068 and 1184), the pair with the clock back at neutral, and the
        // shipped clock.
        for screens in [1.0, 2.0, 5.0, MAX_FACTOR] {
            for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 10.0, MAX_UI_ANIMATION_SCALE] {
                mirror_config(&timing_config(MAX_FACTOR, screens, 10.0, ui));

                assert_eq!(factor(Group::Training), 1.0, "an option wrote the training group");
                assert_eq!(duration_factor(Group::Training), 1.0, "the training group took a share of the clock");

                // The two armed doors, on the numbers the runs handed them: the gauge blend time (the
                // screen run 11 spent 63,731 frames on) and the plate interval (the 1.0 s run 31's door took
                // to 0.05 s, a raw peak of 1.5 in run 18). These are the calls the armed detours make, so a
                // door put back on a lever fails here rather than somewhere in a test's own arithmetic.
                assert_eq!(training_gate_duration(1.0), 1.0, "result_screen_speed {screens} scaled the gauge blend time");
                assert_eq!(training_gate_duration(1.5), 1.5, "the plate interval peak was shortened");
                assert_eq!(
                    plate_cascade_handoff(1.0),
                    1.0,
                    "the plate door handed the cascade something other than the beat the game asked for",
                );
                assert_eq!(plate_cascade_handoff(1.5), 1.5, "the plate door moved a raw peak");

                // The pair ceiling still binds the group the option does write, so the fix is a separation
                // and not a loosening: a result screen completion is no faster than it was on `58dc914`.
                assert!(tween_speed(Group::Screens) <= MAX_TWEEN_SPEED_PRODUCT, "the result screen pair left its ceiling");

                // What a training completion runs at is the tween clock alone, which is the state the C58
                // concept's plain-Hachimi reading calls fast and still in order: a 1.0 s plate beat closes
                // 50 ms after it opens at `ui_animation 20`, and 1.0 s with nothing on the clock.
                assert!(
                    ms_is(completion_ms(1.0, Group::Training), 1000.0 / ui_animation_scale()),
                    "a training gate completed faster than the tween clock it sits on",
                );
                assert_eq!(tween_speed(Group::Training), ui_animation_scale(), "a training gate took a group factor as well as the clock");
            }
        }

        // The shipped default is inert on the training door too - this fork does not pace the cascade at
        // all unless a player asks for a lever, and no player can ask for this one.
        mirror_config(&Config::default());
        assert_eq!(plate_cascade_handoff(1.0), 1.0, "the shipped default changed the cascade's beat");
        assert_eq!(training_gate_duration(2.4), 2.4, "the play in length a run measured moved on a training gate");
    }

    #[test]
    fn the_plate_door_hands_its_own_interval_to_every_lever_but_its_own() {
        let _turn = pass_turn();

        // The `All levers` arm: `result 20` and `ui_animation 20` (core::settings_preset). Item 62 bounded
        // the pair and item 59 took the result screen lever off this door, so a plate call hands 1.0 here at
        // every clock, including the neutral one C62's floor was written for, until the door's own lever is
        // raised.
        for ui in [MIN_UI_ANIMATION_SCALE, 1.0, 2.0, 10.0, MAX_UI_ANIMATION_SCALE] {
            mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, ui));

            let handed = plate_cascade_handoff(1.0);
            assert_eq!(handed, 1.0, "the door shortened the cascade on a {ui}x clock");
            assert!(
                ms_is(completion_ms(handed, Group::Training), 1000.0 / ui_animation_scale()),
                "the plate completion is not the game's own interval over the clock it is measured on",
            );
        }

        // C62's floor is a guard on this door, not a lever, and it still guards: whatever is ever scaled
        // here has to clear a quarter of the game's own spacing before the door will hand it over.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, 1.0));
        assert_eq!(plate_cascade_interval(1.0, 1.0 / MAX_FACTOR), MIN_PLATE_INTERVAL_SEC);
    }

    // The lever ledger item 74 installed, on the door the three runs that reached it priced off the tween clock:
    // 0.05 s handed closed a cascade in 338.2 ms (run 31), 0.25 s in 396.9 ms (run 32), and 1.0 s handed on
    // unchanged in 1242.1 ms while `ui_animation 20` held a 20x delta clock (run 33). A completion the multiplied
    // clock advanced would have closed that third one in 50 ms.
    #[test]
    fn the_plate_lever_hands_the_cascade_floor_at_its_ceiling_and_never_past_it() {
        let _turn = pass_turn();

        // The `All levers` arm as it now stands: the lever at `MAX_FACTOR` on a 20x tween clock. The floor is
        // where the ceiling lands, so the door hands a quarter of the game's own beat and not the 0.05 s run 31
        // read before any floor existed, in the session that stalled on the career end event.
        let mut arm = timing_config(1.0, MAX_FACTOR, 1.0, MAX_UI_ANIMATION_SCALE);
        arm.training_plate_speed = MAX_FACTOR;
        mirror_config(&arm);

        println!(
            "C58 item 74, one plate call at the arm: 1.0 s handed, {} s on the lever, {} s after C62's floor.",
            plate_interval_duration(1.0),
            plate_cascade_handoff(1.0),
        );

        assert_eq!(plate_factor(), MAX_FACTOR);
        assert_eq!(plate_cascade_handoff(1.0), MIN_PLATE_INTERVAL_SEC, "the plate lever never reached the cascade");
        assert_eq!(plate_cascade_handoff(1.5), MIN_PLATE_INTERVAL_SEC, "a raw peak went past the floor");
        assert_eq!(plate_cascade_handoff(0.1), 0.1, "the floor raised an interval the caller wanted shorter");
        assert_eq!(plate_cascade_handoff(0.0), 0.0, "the door invented an interval the game never handed it");

        // Capped the way every other lever is. `normalize` is the one place the slider's 1000.0 or a hand edited
        // config.json reaches the door, and a value that is not a number falls back to doing nothing.
        let mut hand = Config::default();
        hand.training_plate_speed = 1000.0;
        mirror_config(&hand);
        assert_eq!(plate_factor(), MAX_FACTOR, "the plate lever reached past MAX_FACTOR from a hand edited config");

        let mut broken = Config::default();
        broken.training_plate_speed = f32::NAN;
        mirror_config(&broken);
        assert_eq!(plate_factor(), 1.0, "a plate lever that is not a number reached the cascade");
        assert_eq!(plate_cascade_handoff(1.0), 1.0, "a broken mirror moved the cascade's beat");
    }

    #[test]
    fn the_plate_lever_reaches_the_plate_door_and_no_other_training_duration() {
        let _turn = pass_turn();

        let mut arm = timing_config(MAX_FACTOR, MAX_FACTOR, 10.0, MAX_UI_ANIMATION_SCALE);
        arm.training_plate_speed = MAX_FACTOR;
        mirror_config(&arm);

        // The HP gauge blend time keeps item 59's separation and item 62's bound: a progress bar blend is a tween
        // duration, it has no lever of its own, and the plate lever does not reach it.
        assert_eq!(training_gate_duration(1.0), 1.0, "the plate lever reached the HP gauge blend time");
        assert_eq!(training_gate_duration(2.4), 2.4, "a group factor reached the training gate");
        assert_eq!(factor(Group::Training), 1.0, "the plate lever wrote the training group");
        assert_eq!(plate_cascade_handoff(1.0), MIN_PLATE_INTERVAL_SEC, "the plate lever did not reach its own door");

        // And the separation holds the other way: the lever at its shipped 1.0 with every other lever at its
        // ceiling hands the game its own beat, which is the reading run 33 printed four times.
        mirror_config(&timing_config(MAX_FACTOR, MAX_FACTOR, 10.0, MAX_UI_ANIMATION_SCALE));
        assert_eq!(plate_cascade_handoff(1.0), 1.0, "another lever reached the plate cascade");
    }

    #[test]
    fn the_cut_in_lever_raises_the_cut_clock_up_to_the_ceiling_the_runs_read() {
        let _turn = pass_turn();

        // The state runs 33 and 34 measured: this door handing 6.080 with nothing on it. At the shipped 1.0 the
        // lever is the identity, which is what every recorded run measured with.
        assert_eq!(training_cut_factor(), 1.0, "a launch armed the cut-in lever");
        assert_eq!(training_cut_time_scale(6.080), 6.080, "the shipped lever moved the cut clock");

        // The `All levers` arm: the lever at `MAX_TIME_SCALE` on the number the game computed. Runs 36 and 37
        // measured the pair under the old 12.0 ceiling, where the cap was doing the multiplying (5.680 offered
        // 28.4 and the door handed 12.0). At 30.0 both scales that pair saw go through at the full lever.
        let mut arm = Config::default();
        arm.training_cut_speed = MAX_TIME_SCALE;
        mirror_config(&arm);

        println!("item 77, the cut clock on the all levers arm: 6.080 handed, {} offered", training_cut_time_scale(6.080));

        assert_eq!(training_cut_factor(), MAX_TIME_SCALE, "the arm's cut lever did not mirror");
        assert_eq!(training_cut_time_scale(6.080), MAX_TRAINING_CUT_TIME_SCALE, "the cut lever ran past the ceiling the pair bounded");
        assert_eq!(training_cut_time_scale(1.0), MAX_TIME_SCALE, "the lever left the game's own baseline unraised");

        // The two scales the pair actually measured on this door, now inside the ceiling rather than clipped by
        // it: the next run is meant to read a 5x door and not a 12.0 one.
        assert!((training_cut_time_scale(4.800) - 24.0).abs() < 0.05, "the cut door clipped a scale the pair saw handed");
        assert!((training_cut_time_scale(5.680) - 28.4).abs() < 0.05, "the cut door clipped the largest scale the pair saw handed");

        // A scale only goes up (AGENTS section 5), and the ceiling is a cap on what the door may hand, not a
        // trim on what the game asked for: run 11 read 11.280 arrive on this door by itself, and a value the
        // game already put past the cap stays where the game put it.
        assert_eq!(training_cut_time_scale(11.280), MAX_TRAINING_CUT_TIME_SCALE, "the cap replaced the game's own 11.280 with a smaller number");
        assert_eq!(training_cut_time_scale(20.0), MAX_TRAINING_CUT_TIME_SCALE, "the cut door handed more than its ceiling on a value the game put above it");
        assert_eq!(training_cut_time_scale(40.0), 40.0, "the cap pulled a value the game asked for back down");

        // Below `MIN_TIME_SCALE` the game chose a pause or a slow motion, and a mirror that is not a number
        // does nothing at all.
        assert_eq!(training_cut_time_scale(0.0), 0.0, "the lever raised a pause the game asked for");
        assert_eq!(training_cut_time_scale(0.5), 0.5, "the lever raised a slow motion the game asked for");

        let mut hand = Config::default();
        hand.training_cut_speed = 1000.0;
        mirror_config(&hand);
        assert_eq!(training_cut_factor(), MAX_TRAINING_CUT_LEVER, "the cut lever reached past MAX_TRAINING_CUT_LEVER from a hand edited config");

        // At that reach the scale door is the one doing the capping: a 6.080 the game computed goes to the door's
        // own ceiling, and a 2.0 the game computed goes to 20.0 rather than past it.
        assert_eq!(training_cut_time_scale(6.080), MAX_TRAINING_CUT_TIME_SCALE, "the scale door let the full lever reach past its own ceiling");
        assert_eq!(training_cut_time_scale(2.0), 20.0, "the scale door compounded past its ceiling on a smaller value");

        let mut broken = Config::default();
        broken.training_cut_speed = f32::NAN;
        mirror_config(&broken);
        assert_eq!(training_cut_factor(), 1.0, "a cut lever that is not a number reached the cut-in engine");
        assert_eq!(training_cut_time_scale(6.080), 6.080, "a broken mirror moved the cut clock");
    }

    #[test]
    fn the_tag_cut_lever_writes_nothing_at_neutral_and_stops_at_the_pair_bound_above_it() {
        let _turn = pass_turn();

        // Run 40 measured 1214 and 1223 ms between `PlayCutIn` and its `done` action with this lever absent, so
        // the neutral case is the shape every recorded run was in: the doors write nothing on an Animator.
        assert_eq!(tag_cut_factor(), 1.0, "a launch armed the tag cut-in lever");
        assert_eq!(tag_cut_animator_speed(), 1.0, "the tag cut-in lever wrote an Animator speed at its neutral 1.0");

        // With `Time.timeScale` at the neutral 1.0 the lever reaches its own ceiling.
        let mut arm = Config::default();
        arm.training_tag_cut_speed = MAX_TIME_SCALE;
        mirror_config(&arm);

        assert_eq!(tag_cut_factor(), MAX_TIME_SCALE, "the arm's tag cut lever did not mirror");
        assert_eq!(tag_cut_animator_speed(), MAX_TAG_CUT_ANIMATOR_SPEED, "the tag cut lever passed the ceiling on an Animator speed");

        // The pair is real: an Animator advances on the scale this layer hands Unity's setter, so with 5.0 written
        // into `Time.timeScale` the speed stops at the product bound over it rather than at its own ceiling.
        TIME_SCALE_PRODUCED.store(5.0f32.to_bits(), Ordering::Release);

        assert_eq!(tag_cut_animator_speed(), MAX_TWEEN_SPEED_PRODUCT / 5.0, "the tag cut speed and the time-scale lever went past the pair ceiling together");

        TIME_SCALE_PRODUCED.store(1.0f32.to_bits(), Ordering::Release);

        // A hand edited config cannot ask for more than the slider allows, and a mirror that is not a number does
        // nothing at all. A rate never goes down, so 0.1 is not a slow motion request.
        let mut hand = Config::default();
        hand.training_tag_cut_speed = 1000.0;
        mirror_config(&hand);
        assert_eq!(tag_cut_factor(), MAX_TIME_SCALE, "a hand edited config reached past MAX_TIME_SCALE on the Animator lever");

        let mut slow = Config::default();
        slow.training_tag_cut_speed = 0.1;
        mirror_config(&slow);
        assert_eq!(tag_cut_animator_speed(), 1.0, "the lever turned a training cut-in into a slow motion");

        let mut broken = Config::default();
        broken.training_tag_cut_speed = f32::NAN;
        mirror_config(&broken);
        assert_eq!(tag_cut_animator_speed(), 1.0, "a tag cut lever that is not a number reached an Animator");
    }

    #[test]
    fn both_tag_cut_doors_report_where_a_run_reads_them() {
        let _turn = pass_turn();

        // Named as `TrainingCuttProbe` names these doors, so a run can match the write lines to the census counts,
        // and each on its own slot because a door the game never reached prints nothing at all (A4).
        assert_eq!(TAG_CUT_DOOR, "SingleModeMainViewTagTrainingCutInPlayer.CreateLineEffect");
        assert_eq!(TAG_CUT_LINE_DOOR, "SingleModeMainViewTagTrainingCutInPlayer.PlayLineEffect");
        assert!(TAG_CUT_SLOT < HIT_SLOTS && TAG_CUT_LINE_SLOT < HIT_SLOTS, "a tag cut door counts on a slot nothing has");

        note_tag_cut_write(TAG_CUT_DOOR, TAG_CUT_SLOT, Some((1.0, true)), 4.0);
        note_tag_cut_write(TAG_CUT_LINE_DOOR, TAG_CUT_LINE_SLOT, None, 4.0);

        assert_eq!(hit_calls(TAG_CUT_SLOT), 1, "the CreateLineEffect door is not counted");
        assert_eq!(hit_calls(TAG_CUT_LINE_SLOT), 1, "the PlayLineEffect door is not counted");
    }

    #[test]
    fn the_cut_in_motion_lever_stops_at_the_pair_bound_and_never_lowers_a_speed() {
        let _turn = pass_turn();

        // Run 45 named the object a cut-in effect runs on (`AnimateToUnity.AnMotion`, `introspect.log:28743`) and its
        // speed door `SetMotionSpeed/2`. At the shipped 1.0 the lever writes nothing anywhere.
        assert_eq!(flash_motion_speed(), 1.0, "a launch armed the cut-in motion lever");

        let mut arm = Config::default();
        arm.training_cut_speed = MAX_TRAINING_CUT_LEVER;
        mirror_config(&arm);

        assert_eq!(flash_motion_speed(), MAX_MOTION_SPEED, "the motion lever passed its own ceiling");

        // The control is the rate on this channel too, so the point the measurement landed on (run 46 at 5.0, both
        // training cut walls halved) is one slider step below the ceiling and not the ceiling itself.
        let mut measured = Config::default();
        measured.training_cut_speed = MAX_TIME_SCALE;
        mirror_config(&measured);
        assert_eq!(flash_motion_speed(), MAX_TIME_SCALE, "the motion lever moved the slider position the runs measured");

        let mut half = Config::default();
        half.training_cut_speed = MAX_TIME_SCALE / 2.0;
        mirror_config(&half);
        assert_eq!(flash_motion_speed(), MAX_TIME_SCALE / 2.0, "the motion lever ignored where the slider stood");

        let mut nudge = Config::default();
        nudge.training_cut_speed = 1.5;
        mirror_config(&nudge);
        assert_eq!(flash_motion_speed(), 1.5, "the motion lever rounded a small slider move up to its ceiling");

        // AnimateToUnity advances its own time on Unity's clock, so with the time-scale lever written the pair stops
        // at the product bound rather than at the lever's ceiling, the way C58 prices the ui clock. On the full arm
        // that means 4.0 and not the 10.0 this channel can reach.
        mirror_config(&arm);
        TIME_SCALE_PRODUCED.store(5.0f32.to_bits(), Ordering::Release);
        assert_eq!(flash_motion_speed(), MAX_TWEEN_SPEED_PRODUCT / 5.0, "the motion lever and the time-scale lever went past the pair ceiling together");
        TIME_SCALE_PRODUCED.store(1.0f32.to_bits(), Ordering::Release);

        let bound = MAX_MOTION_SPEED;

        // What the scaling door hands the game: a 1.0 raised to the bound, a speed under the bound raised to it, and a
        // speed the game already set past the bound left where the game put it. A motion the game paused or slowed
        // under the lever is untouched.
        assert_eq!(raise_speed_argument(1.0, bound), bound, "the door left the game's own 1.0 unraised");
        assert_eq!(raise_speed_argument(8.0, bound), bound, "the door left a speed under its bound unraised");
        assert_eq!(raise_speed_argument(MAX_MOTION_SPEED + 2.0, bound), MAX_MOTION_SPEED + 2.0, "the cap pulled a speed the game set past the bound back down");
        assert_eq!(raise_speed_argument(0.0, bound), 0.0, "the lever raised a motion the game paused");
        assert_eq!(raise_speed_argument(0.5, 1.0), 0.5, "the neutral lever reached a motion speed");
        assert_eq!(raise_speed_argument(-1.0, bound), -1.0, "the lever invented a speed the game did not hand");

        assert!(
            FLASH_PLAY_INT_SLOT < HIT_SLOTS && FLASH_PLAY_FLOAT_SLOT < HIT_SLOTS && MOTION_SET_SPEED_SLOT < HIT_SLOTS,
            "a cut-in motion door counts on a slot nothing has"
        );

        note_motion_write(FLASH_PLAY_INT_DOOR, FLASH_PLAY_INT_SLOT, Some((1.0, true)), bound);
        note_motion_write(MOTION_SET_SPEED_DOOR, MOTION_SET_SPEED_SLOT, None, bound);

        assert_eq!(hit_calls(FLASH_PLAY_INT_SLOT), 1, "the flash play door is not counted");
        assert_eq!(hit_calls(MOTION_SET_SPEED_SLOT), 1, "the motion speed door is not counted");
    }

    #[test]
    fn the_cut_in_lever_reaches_the_cut_clock_door_and_no_other_training_value() {
        let _turn = pass_turn();

        let mut arm = timing_config(MAX_FACTOR, MAX_FACTOR, 10.0, MAX_UI_ANIMATION_SCALE);
        arm.training_cut_speed = MAX_TIME_SCALE;
        mirror_config(&arm);

        // The lever is on the cut-in's scale and on nothing else the training turn waits on: the plate beat keeps
        // its own lever, the HP gauge blend keeps item 59's separation, and `Group::Training` stays factor-less.
        assert_eq!(training_cut_time_scale(6.080), MAX_TRAINING_CUT_TIME_SCALE, "the cut lever did not reach its own door");
        assert_eq!(plate_cascade_handoff(1.0), 1.0, "the cut lever reached the plate cascade");
        assert_eq!(training_gate_duration(2.4), 2.4, "the cut lever reached the HP gauge blend time");
        assert_eq!(factor(Group::Training), 1.0, "the cut lever wrote the training group");

        // And the separation holds the other way: the plate lever at its ceiling with the cut lever at its
        // shipped 1.0 leaves the cut clock on the number the game computed.
        let mut plate_only = timing_config(1.0, 1.0, 1.0, 1.0);
        plate_only.training_plate_speed = MAX_FACTOR;
        mirror_config(&plate_only);

        assert_eq!(plate_cascade_handoff(1.0), MIN_PLATE_INTERVAL_SEC, "the plate lever stopped reaching its own door");
        assert_eq!(training_cut_time_scale(6.080), 6.080, "the plate lever reached the cut-in clock");
    }

    #[test]
    fn the_cut_clock_door_reports_where_a_run_reads_it_and_stays_out_of_the_duration_table() {
        let _turn = pass_turn();

        // The door hands the cut-in engine a scale, not a duration, so it has no completion for a `Pace` to speak
        // about and it is not one of the training census's scaling points. A run reads it three other ways: the
        // `hit` line the door writes, the probe's `SingleModeUtils::GetTrainingCutTimeScale(scale)` peak, which
        // `note_cut_clock` keeps fed with the game's own number, and the ceiling named on the training gate line.
        assert!(
            !TRAINING_HIT_SLOTS.iter().any(|(_, name)| *name == TRAINING_CUT_DOOR),
            "the cut clock door went into the duration census, which is the table a scale door does not belong to"
        );
        assert!(
            !ARMED_DOOR_PACES.iter().any(|(door, _, _)| *door == TRAINING_CUT_DOOR),
            "the cut clock door went into the armed duration table"
        );

        hit(TRAINING_CUT_SLOT, TRAINING_CUT_DOOR, 6.080, MAX_TRAINING_CUT_TIME_SCALE);
        assert_eq!(hit_calls(TRAINING_CUT_SLOT), 1, "the cut clock door scales on a slot nothing counts");

        assert!(
            MAX_TRAINING_CUT_TIME_SCALE > MAX_TIME_SCALE,
            "a cap at MAX_TIME_SCALE hands this client its own 6.080 back at every setting, which is a slider that does nothing"
        );
        assert!(
            note_training_gate_levers(),
            "a run was never told the cut clock carries training_cut_speed and stops at MAX_TRAINING_CUT_TIME_SCALE"
        );
    }

    #[test]
    fn the_plate_door_stands_off_the_pair_bound_on_the_readings_that_put_it_there() {
        let _turn = pass_turn();

        let (_, group, listed) = ARMED_DOOR_PACES
            .iter()
            .find(|(door, _, _)| *door == TRAINING_PLATE_DOOR)
            .expect("the plate door is not in the armed door table");

        assert_eq!(*group, Group::Training, "the plate door left the training group");
        assert!(!listed.bounds_the_pair(), "the plate door is back inside the pair bound");

        if let Pace::OffTweenClock(proof) = listed {
            assert!(
                proof.contains("run 31") && proof.contains("run 32") && proof.contains("run 33"),
                "the plate door left the bound without the three readings that put it there: {proof}"
            );
        }

        let lanes = armed_door_lanes();

        println!("C58 item 74, the lane hachimi.log prints for the plate door: {lanes}");

        assert!(
            lanes.contains(PACE_LANE_OFF_CLOCK) && lanes.contains(TRAINING_PLATE_DOOR),
            "the lane line does not name the plate door off the bound: {lanes}"
        );

        // The door beside it stays bounded: it has no readings, and a lane a run has not measured is the side
        // of the ceiling that keeps the pair from composing on a tween blend time.
        let gauge_lane = pace_lane(armed_door_pace(TRAINING_GAUGE_DOOR, PACE_TRAINING_GATE));

        assert_eq!(gauge_lane, PACE_LANE_UNPROVEN, "the gauge blend door left the bound it has no readings to leave: {lanes}");
        assert!(lanes.contains(TRAINING_GAUGE_DOOR), "the lane line does not name the gauge door: {lanes}");
    }

    // The defect this item closes, kept as an executable statement of what the door used to hand. The
    // numbers are the ones the run logs printed: run 31's `InitializePlateList 1 -> 0.05`
    // (`run log/hachimi-run31.log` line 1184) and run 20's `1 -> 0.05` beside `ui_animation 20`.
    #[test]
    fn a_result_screen_lever_on_the_plate_door_hands_the_number_run_31_stalled_with() {
        let _turn = pass_turn();

        // `result 20` on a neutral tween clock - the hand-built config this fork's own rules exist to clamp,
        // and the hand that put 0.05 s on this door in run 31 after the arm at its line 1068. That run's own
        // tween clock was the arm's 20x on top of the duration, which is the completion the pair tests price.
        mirror_config(&timing_config(1.0, MAX_FACTOR, 1.0, 1.0));

        let on_the_result_group = scale_duration(1.0, Group::Screens);
        let with_c62_floor = plate_cascade_interval(1.0, on_the_result_group);
        let as_shipped = plate_cascade_handoff(1.0);

        println!(
            "C58 item 59, one plate call: the Screens group hands the plate gate {on_the_result_group} s ({} ms), C62's floor lifts that to {with_c62_floor} s, the separated door hands {as_shipped} s.",
            on_the_result_group * 1000.0,
        );

        // The lever alone goes under the floor the fork wrote to catch it, which is the statement that a
        // floor on the door was holding the door up instead of the door being out of the lever's reach.
        assert!(on_the_result_group < MIN_PLATE_INTERVAL_SEC, "a result screen lever no longer reaches under the plate floor");
        assert_eq!(with_c62_floor, MIN_PLATE_INTERVAL_SEC);

        // And the shipped door is out of its reach: the game's own beat, whatever the sliders say.
        assert_eq!(as_shipped, 1.0, "the plate door is still a scaling point");
    }

    #[test]
    fn the_training_gates_are_out_of_the_group_the_result_screen_option_writes() {
        // The other half of the conflation: `Group::Screens` carried the plate classes' own durations. They
        // are `static const float` on this client - IL2CPP folds them into the call sites, `X is a
        // compile-time constant`, 0 of 61 fields resolve (C13) - so no run ever saw them move, but a client
        // where they have storage would have had them rewritten by a result screen slider.
        let training_classes: Vec<&str> =
            FIELDS.iter().filter(|spec| spec.group == Group::Training).map(|spec| spec.class).collect();

        for class in ["TrainingParamChangeA2U", "TrainingParamChangePlate", "SingleModeMainTrainingCuttController"] {
            assert!(training_classes.contains(&class), "{class} is not in the training group");
        }

        assert!(
            FIELDS.iter().all(|spec| spec.group != Group::Screens || !spec.class.contains("Training")),
            "a training class is still in the group result_screen_speed writes",
        );
        assert_eq!(FIELDS.len(), 61, "the field table moved entries; C13's 0 of 61 no longer says the same thing");
        assert_eq!(
            field_count(Group::Transition) + field_count(Group::Screens) + field_count(Group::Story) + field_count(Group::Training),
            FIELDS.len(),
            "a field spec is in no group",
        );
    }

    #[test]
    fn the_pair_ceiling_line_is_owed_once_per_binding_clock_setting() {
        let _turn = pass_turn();

        // The neutral clock owes nothing, and it clears what was owed, so a session that leaves the
        // `All levers` arm and comes back to it gets the line again in the second arm window (1516af6).
        assert!(!note_pair_ceiling(1.0, [1.0, 1.0, 1.0]), "a neutral clock was announced as bound");

        // A clock that binds the ceiling says it once. The same setting again is the same fact, and a
        // config pass runs once per view change on top of once per setting change.
        assert!(note_pair_ceiling(MAX_UI_ANIMATION_SCALE, [1.0, MAX_FACTOR, 1.0]), "the bound clock owed no line");
        assert!(!note_pair_ceiling(MAX_UI_ANIMATION_SCALE, [1.0, MAX_FACTOR, 1.0]), "the same clock value printed twice");

        assert!(!note_pair_ceiling(1.0, [1.0, 1.0, 1.0]));
        assert!(note_pair_ceiling(MAX_UI_ANIMATION_SCALE, [1.0, MAX_FACTOR, 1.0]), "an arm switch back to a bound clock is invisible in the log");

        // A different clock value is a different fact. The slider's floor is a slow down of the tween
        // clock and binds nothing on its own.
        assert!(note_pair_ceiling(5.0, [MAX_FACTOR, MAX_FACTOR, MAX_FACTOR]));
        assert!(!note_pair_ceiling(MIN_UI_ANIMATION_SCALE, [MAX_FACTOR, MAX_FACTOR, MAX_FACTOR]));

        // C58 / ledger item 62: the same composed clock under a different `Time.timeScale` is a different
        // sentence. `ui_animation 20` over a Unity holding 1.0 and over the 5.0 a lever of 5 wrote from a 2.0
        // request are both a 20x channel, at a 20x ui clock and a 4x one, and the 4x is a clamp a run has to see.
        UI_ANIMATION_SCALE.store(MAX_UI_ANIMATION_SCALE.to_bits(), Ordering::Release);
        assert!(note_time_scale_write(2.0, apply_time_scale(2.0, MAX_TIME_SCALE)), "the write that moved the ui cap owed no line");
        assert!(!note_pair_ceiling(MAX_UI_ANIMATION_SCALE, [1.0, MAX_FACTOR, 1.0]), "the config pass repeated a line the scale already said");
        TIME_SCALE_PRODUCED.store(1.0f32.to_bits(), Ordering::Release);

        // A mirror this module never stores still owes no line.
        assert!(!note_pair_ceiling(f32::NAN, [1.0, 1.0, 1.0]));
    }

    // The other half of the clock the ceiling prices: a scale this fork's write layer left in `Time.timeScale`
    // trims every bounded door even with `ui_animation` neutral, and a trim a run cannot see is a trim that did
    // not happen (AGENTS section 2). The line is owed for it, and the line names the scale and the share of it
    // the lever added.
    #[test]
    fn the_pair_ceiling_line_is_owed_for_a_time_scale_raise_at_a_neutral_ui_clock() {
        let _turn = pass_turn();

        UI_ANIMATION_SCALE.store(1.0f32.to_bits(), Ordering::Release);

        // Unity holding 1.0 and the ui slider neutral: no clamp is in force, so nothing is owed.
        assert!(!note_pair_ceiling(1.0, [MAX_FACTOR, MAX_FACTOR, MAX_FACTOR]), "a neutral pair was announced as bound");
        assert_eq!(delta_clock(), 1.0, "a neutral pair is not the neutral clock");

        // A scale the write layer left in the game trims every bounded door even with `ui_animation` neutral:
        // the headroom drops to 8x of 20. The rung is one the shipped writer produces - a 1.25 request under a
        // 2.0 lever - not a pair `apply_time_scale` refuses to hand the setter.
        let rung = apply_time_scale(1.25, 2.0);
        assert!(ms_is(rung, 2.5), "the rung this case is built on is no longer a produced 2.5: {rung}");
        assert!(note_time_scale_write(1.25, rung), "a scale that trimmed every group factor owed no line");
        assert_eq!(delta_clock(), 2.5, "a 2.5x scale under a neutral ui clock is not the clock the ceiling prices");
        assert!(ms_is(MAX_TWEEN_SPEED_PRODUCT / delta_clock(), 8.0), "a bounded door keeps {}x of headroom at that scale", MAX_TWEEN_SPEED_PRODUCT / delta_clock());
        assert!(!note_pair_ceiling(1.0, [MAX_FACTOR, MAX_FACTOR, MAX_FACTOR]), "the same clock printed on every config pass");

        // The scale does not have to arrive on a config pass to be said: the write that produced it is the moment
        // the clock changes, and the same latch answers for both callers.
        assert!(!note_time_scale_write(1.0, 1.0), "a write back to the neutral scale owed a line");
        assert!(!note_pair_ceiling(1.0, [1.0, 1.0, 1.0]), "a clock back at neutral still owed a line");
        assert!(note_time_scale_write(4.0, apply_time_scale(4.0, MAX_TIME_SCALE)), "a scale that changed the clock owed no line at the write that produced it");
        assert!(!note_time_scale_write(4.0, apply_time_scale(4.0, MAX_TIME_SCALE)), "the same clock printed on every game write");
        assert!(!note_pair_ceiling(1.0, [MAX_FACTOR, MAX_FACTOR, MAX_FACTOR]), "the config pass repeated a line the produced write already said");

        // A scale that comes back after going away is a new fact, and a run has to be able to see it.
        assert!(!note_time_scale_write(1.0, 1.0), "a produced write that raised nothing owed a line");
        assert!(note_time_scale_write(2.0, apply_time_scale(2.0, MAX_TIME_SCALE)), "a scale that came back is invisible in the log");
    }

    // The line is the closure evidence item 62 owes (AGENTS section 2), so the sentence a run reads is checked,
    // not just whether one was owed. Both states the `Time.timeScale` write layer has are read out of the log:
    // the rung the review caught, this fork's writer putting 2.5x into the game (a 1.25 request under a 2.0
    // lever) under the ui slider at its ceiling, and the state where the game holds a scale past `MAX_TIME_SCALE`
    // and this layer added nothing to it, which is run 19's 8.0000 cut clock read through the neutral lever fast
    // exit. The parenthetical is computed off the raise, so it is one sentence in the first state and the other
    // in the second, and putting the second's sentence on the first is the contradiction this case exists to
    // catch.
    #[test]
    fn the_pair_ceiling_line_names_the_raise_it_prices() {
        let _turn = pass_turn();

        // The turn starts at the neutral clock and a neutral clock clears the latch, so this case states its own
        // two sentences whatever an earlier case left in the marker.
        UI_ANIMATION_SCALE.store(1.0f32.to_bits(), Ordering::Release);
        assert!(!note_pair_ceiling(1.0, [1.0, 1.0, 1.0]), "the neutral clock owed a line");

        capture_log();

        // The note is computed from the raise the clock carries, because no printed sentence can hold a fixed
        // claim about it.
        assert_eq!(time_scale_raise_note(2.0), "2x of it the time_scale lever added");
        assert_eq!(time_scale_raise_note(1.0), "the time_scale lever raised nothing");

        // The rung: 1.25 the game asked for, 2.5 what the write layer handed the setter, so 2x of the scale in
        // Unity is this fork's. Over a 20x ui slider that is a 20x channel at an 8x ui clock with 1x left for a
        // group factor.
        UI_ANIMATION_SCALE.store(MAX_UI_ANIMATION_SCALE.to_bits(), Ordering::Release);

        let rung = apply_time_scale(1.25, 2.0);
        assert!(ms_is(rung, 2.5), "the rung this case is built on is no longer a produced 2.5: {rung}");
        assert!(note_time_scale_write(1.25, rung), "the write that produced a raise owed no line");

        let lines = captured_lines("AnimationSpeed: pair ceiling");
        assert_eq!(lines.len(), 1, "the clock the produced write left printed {} lines", lines.len());

        let produced_line = &lines[0];
        assert!(
            produced_line.contains("AnimationSpeed: pair ceiling 20x on one completion both layers reach: ui_animation 20x over the 2.5x Unity is holding in Time.timeScale from this fork's write layer (2x of it the time_scale lever added)"),
            "the line does not name the share the time_scale lever added: {produced_line}"
        );
        assert!(
            produced_line.contains("is a 20x delta clock, so the ui clock runs 8x and 1x is left for a group factor"),
            "the line does not price the channel the raise sits in: {produced_line}"
        );
        assert!(!produced_line.contains("raised nothing"), "the line calls a raise it produced a nothing: {produced_line}");

        // The pass-through: the game holding 8.0, the lever neutral, so the whole 8.0 is the game's and this
        // layer added none of it. The neutral sentence belongs here and nowhere else.
        UI_ANIMATION_SCALE.store(1.0f32.to_bits(), Ordering::Release);
        assert!(note_time_scale_write(8.0, 8.0), "a scale the game reached past the ceiling owed no line");

        let lines = captured_lines("AnimationSpeed: pair ceiling");
        assert_eq!(lines.len(), 2, "the clock the pass-through left printed {} lines", lines.len());

        let passed_line = &lines[1];
        assert!(
            passed_line.contains("ui_animation 1x over the 8x Unity is holding in Time.timeScale from this fork's write layer (the time_scale lever raised nothing) is a 8x delta clock, so the ui clock runs 1x and 2.5x is left for a group factor"),
            "the line does not say the lever raised nothing: {passed_line}"
        );
        assert!(!passed_line.contains("of it the time_scale lever added"), "the line named a share the lever did not add: {passed_line}");
    }

    // C58 / ledger item 59. The training doors keep their hooks after losing their lever, and a door that
    // hands the value it received looks like a hook the game never reached (A4), so the separation itself
    // has to reach `hachimi.log` - once per launch, from the config pass that mirrors the levers.
    #[test]
    fn the_training_gate_note_is_owed_once_per_launch() {
        let _turn = pass_turn();

        assert!(note_training_gate_levers(), "a run was never told which lever each training gate carries");
        assert!(!note_training_gate_levers(), "the same fact was printed on every config pass");
    }

    // egui 0.33.3 `Slider::set_value`: the value is clamped to the range, then snapped to
    // `start + round((value - start) / step) * step`, so the left end of a slider lands on the
    // range start itself. That is how a slider floor becomes a number the game is handed.
    fn egui_snap(range: std::ops::RangeInclusive<f64>, step: f64, value: f64) -> f64 {
        let start = *range.start();
        let value = value.clamp(start, *range.end());
        start + ((value - start) / step).round() * step
    }

    fn multiplier_is(mult: Option<f32>, expected: f32) -> bool {
        mult.is_some_and(|mult| (mult - expected).abs() < 1e-4)
    }

    // C24's reproduction on the number `StoryChoiceController::CheckChoiceAutoTap` divides the
    // delay into. The Config Editor slider used to start at 0.0001, so dragging it to the left end
    // was the defect's magnitude.
    #[test]
    fn story_choice_auto_select_multiplier_is_bounded_at_both_sites() {
        // The magnitude C24 measured: the old slider's left end, divided into the trigger time, is
        // what both story choice sites multiplied by.
        let old_left_end = egui_snap(0.0001..=10.0, 0.05, 0.0);
        assert_eq!(old_left_end, 0.0001, "the old slider's left end was not its range start");
        let c24_multiplier = 0.75 / old_left_end as f32;
        assert!(c24_multiplier > 7000.0, "C24's 7500x no longer reproduces");

        // The floor is on the grid the Config Editor slider snaps to, so its left end is the floor.
        let new_left_end = egui_snap(MIN_STORY_CHOICE_AUTO_SELECT_DELAY as f64..=10.0, 0.05, 0.0);
        assert_eq!(new_left_end, MIN_STORY_CHOICE_AUTO_SELECT_DELAY as f64, "the floor is off the slider's grid");

        // The floor binds first: below it, every delay is a floor sized delay.
        assert!(multiplier_is(story_choice_auto_select_multiplier(0.05), 7.5), "0.05 was not a floor sized delay");
        assert!(multiplier_is(story_choice_auto_select_multiplier(MIN_STORY_CHOICE_AUTO_SELECT_DELAY), 7.5), "the floor itself was changed");
        assert!(multiplier_is(story_choice_auto_select_multiplier(0.0001), 7.5), "the slider's old left end still reaches the clock");

        // The ceiling is the backstop under the floor: it caps the very divide the wait time site
        // performs, so no ordering of the two bounds hands the game's accumulator more than
        // MAX_FACTOR. It is not the bound the getter half answers to; that one is MAX_TIME_SCALE.
        assert_eq!(c24_multiplier.min(MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER), MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER, "the ceiling does not cap 0.75 / delay");
        assert!(MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE < MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER, "one ceiling for a duration and a time scale is how C24 reached 20x on the story clock");

        // Everything the config can hold, however absurd, comes out bounded or inert.
        for delay in [f32::MIN_POSITIVE, 1e-8, 1e-4, 0.01, 0.0999, 0.1, 0.75, 1.2, 10.0, 1e6, f32::MAX] {
            if let Some(mult) = story_choice_auto_select_multiplier(delay) {
                assert!(mult > 0.0 && mult <= MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER, "{delay} escaped the bounds: {mult}");
            }
        }

        assert_eq!(story_choice_auto_select_multiplier(0.0), None, "a hand edited 0 divided by zero");
        assert_eq!(story_choice_auto_select_multiplier(-1.0), None, "a negative delay flipped the story time scale");
        assert_eq!(story_choice_auto_select_multiplier(f32::NAN), None, "NAN reached the story clock");
        assert_eq!(story_choice_auto_select_multiplier(f32::INFINITY), None, "an infinite delay was a multiplier");

        // Every setting inside the range the option has always offered behaves as it did before.
        assert!(multiplier_is(story_choice_auto_select_multiplier(1.2), 0.625), "the neutral default became a speed-up");
        assert_eq!(story_choice_auto_select_multiplier(0.75), Some(1.0), "the game's own delay was rescaled");
        assert_eq!(story_choice_auto_select_multiplier(10.0), Some(0.075), "a slower delay was capped from below");

        println!(
            "story_choice_auto_select: slider left end {} -> mult {} | {} -> mult {} | C24's raw 0.75 / {} = {} capped at {}",
            new_left_end,
            story_choice_auto_select_multiplier(MIN_STORY_CHOICE_AUTO_SELECT_DELAY).unwrap_or(0.0),
            0.0001, story_choice_auto_select_multiplier(0.0001).unwrap_or(0.0),
            old_left_end, c24_multiplier, MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER,
        );
    }

    // The other half of the same number: `StoryViewController::GetTimeScaleByHighSpeedType` returns
    // the scale the story timeline steps its clips by, so capping it with the duration ceiling is
    // what left the story clock at 7.5 (game scale 1.0 times the floor multiplier) or 20 with a hand
    // edited config, against MAX_TIME_SCALE = 5.0.
    #[test]
    fn the_story_choice_getter_half_is_bounded_by_the_time_scale_ceiling() {
        let _turn = pass_turn();

        let floor_mult = story_choice_auto_select_multiplier(MIN_STORY_CHOICE_AUTO_SELECT_DELAY).unwrap();
        assert!(multiplier_is(Some(floor_mult), 7.5), "the floor is no longer 7.5 on the wait half");
        assert!(floor_mult > MAX_TIME_SCALE, "the story clock is no longer the tighter bound of the two");

        // Every multiplier the clamp can produce, on the game's own neutral playback scale.
        for delay in [0.0001f32, 0.05, 0.1, 0.2, 0.3, 0.375, 0.75, 1.2, 10.0, f32::MAX] {
            let mult = story_choice_auto_select_multiplier(delay).unwrap();
            let scale = story_choice_time_scale_from(1.0, mult);

            assert!(scale <= MAX_TIME_SCALE, "{delay} put {scale} on the story clock");
            assert!(scale >= 1.0, "{delay} lowered the story clock under the game's own 1.0");
        }

        // The magnitude the defect reported, and what each half is capped at now.
        assert_eq!(story_choice_time_scale_from(1.0, floor_mult), MAX_TIME_SCALE, "the Config Editor's left end still reaches the story clock past the ceiling");
        assert_eq!(story_choice_time_scale_from(1.0, MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER), MAX_TIME_SCALE, "MAX_FACTOR is still a 20x time scale");
        assert_eq!(story_choice_time_scale_from(1.0, c24_wait_mult()), MAX_TIME_SCALE, "C24's raw divide is not a story time scale either");

        // The game's own values are neither lowered nor compounded.
        assert_eq!(story_choice_time_scale_from(0.0, floor_mult), 0.0, "the game's pause became a fast forward");
        assert_eq!(story_choice_time_scale_from(0.5, floor_mult), 0.5, "slow motion was overwritten");
        assert_eq!(story_choice_time_scale_from(8.0, floor_mult), 8.0, "a scale the game holds above the ceiling was pulled down");
        assert_eq!(story_choice_time_scale_from(2.0, floor_mult), MAX_TIME_SCALE, "the game's own fast forward was multiplied past the ceiling");

        // A slower delay than the game's 0.75 is delivered by the wait time half alone.
        assert_eq!(story_choice_time_scale_from(1.0, 0.625), 1.0, "a slower delay slowed the story clock");
        assert_eq!(story_choice_time_scale_from(1.0, 1.0), 1.0, "the game's own delay rescaled the story clock");
        assert_eq!(story_choice_time_scale_from(1.0, f32::NAN), 1.0, "the inert setting wrote a scale");
        assert!(story_choice_time_scale_from(f32::NAN, floor_mult).is_nan(), "NAN reached the story clock");

        // A build at the shipped defaults: nothing has written the mirror, so both halves are inert.
        assert!(story_choice_wait_time_multiplier().is_nan(), "the default mirror was a multiplier");
        assert_eq!(story_choice_time_scale(1.0), 1.0, "the default raised the story clock");

        println!(
            "story_choice_auto_select at the slider left end: wait x{} -> story time scale {} -> {} (ceiling {}), one request capped at {}",
            floor_mult, 1.0, story_choice_time_scale_from(1.0, floor_mult), MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE,
            floor_mult * MAX_STORY_CHOICE_AUTO_SELECT_TIME_SCALE,
        );
    }

    // The pre-fix magnitude, spelled out so the two ceilings stay comparable to what C24 measured.
    fn c24_wait_mult() -> f32 {
        story_choice_auto_select_multiplier(0.0001).unwrap()
    }

    // The value proof the matcher never made: `get_method_overload` compares the type enum alone,
    // so `struct<StoryTimelineController.HighSpeedType:4B>` and a 24 byte struct were the same
    // answer to a wrapper declaring an `i32`. `payload` is the size measured from the class behind
    // the dumped type, `None` when that class could not be read.
    #[test]
    fn a_4_byte_enum_is_a_value_and_a_bigger_struct_is_not() {
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Some(4)), (ValueShape::Inline, 4), "the dumped HighSpeedType");
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_ENUM, Some(4)), (ValueShape::Inline, 4), "the same enum under the other spelling");
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Some(8)), (ValueShape::Unproven, 8), "an i32 read the first half of a struct");
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Some(24)), (ValueShape::Unproven, 24), "a struct the ABI moves through memory");
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, None), (ValueShape::Unproven, 0), "no class, no proof");
    }

    #[test]
    fn a_class_parameter_is_a_reference_not_the_number_the_wrapper_declared() {
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_CLASS, None), (ValueShape::Reference, 0));
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_OBJECT, None), (ValueShape::Reference, 0));
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_STRING, None), (ValueShape::Reference, 0));
        assert_eq!(value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST, None), (ValueShape::Unproven, 0));

        // The two spellings are two different installs, and only one of them is safe in front of a
        // wrapper that hands over an `i32`.
        assert!(!shape_is_allowed(ValueShape::Reference, true), "an i32 wrapper bound a pointer");
        assert!(shape_is_allowed(ValueShape::Reference, false), "a wrapper that declares a pointer holds one");
        assert!(shape_is_allowed(ValueShape::Inline, true), "the 4 byte enum these wrappers are sized for");
        assert!(!shape_is_allowed(ValueShape::Unproven, false), "nothing is hooked on an unproven shape");
    }

    // No hook that already takes a `class<...>` argument changes install behaviour: `resolve_method`
    // (the fades), `resolve_static_method` (the frame probe's
    // `IsStoryEndFrameOrGrandLiveWaitFrameSkipped`) and `resolve_getter` keep the permissive rule,
    // because their wrappers declare pointers. Only the value shaped matcher refuses a reference.
    #[test]
    fn the_permissive_match_kinds_still_hold_a_class_argument() {
        let class_argument = value_shape_for(Il2CppTypeEnum_IL2CPP_TYPE_CLASS, None).0;

        assert!(shape_is_allowed(class_argument, MATCH_INSTANCE_VALUE.values_only), "the result screen fades take class<...>");
        assert!(shape_is_allowed(class_argument, MATCH_STATIC_VALUE.values_only), "the static probe takes two");
        assert!(shape_is_allowed(class_argument, MATCH_GETTER_EITHER.values_only), "a getter may return a reference");
        assert!(!shape_is_allowed(class_argument, MATCH_STATIC_VALUES_ONLY.values_only), "the i32 HighSpeedType wrappers do not");
    }

    // C15. The question each wrapper profile asks the class table. A profile mapped onto the wrong
    // question is the C15 decision again: an instance wrapper asking as if it had no `this`, a
    // static-only wrapper asking as if it had one, or the return type the wrapper reads left out of
    // the ask so that any method under that name could answer it.
    #[test]
    fn every_wrapper_profile_asks_the_class_table_for_its_own_shape() {
        let instance = request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_CLASS], Il2CppTypeEnum_IL2CPP_TYPE_VOID, MATCH_INSTANCE_VALUE);
        assert_eq!((instance.allow_static, instance.require_static), (false, false), "an instance wrapper asked the table for a static method");
        assert_eq!(instance.ret, Some(Il2CppTypeEnum_IL2CPP_TYPE_VOID), "the request dropped the return type its wrapper reads");
        assert_eq!(instance.params, &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS], "the request dropped the parameter list its wrapper declares");
        assert!(!instance.generic_slots, "an ordinary wrapper may not be answered by a generic instantiation");

        // `StoryTimelineController::GetNextFrameCount_HighSpeed` is an instance method with `ref`
        // parameters, so it asks the table the same shape question `resolve_method` asks.
        let ref_method = request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID, MATCH_REFERENCE);
        assert_eq!((ref_method.allow_static, ref_method.require_static), (false, false), "the ref wrappers ask for an instance method");

        // A wrapper that declares only the real arguments has no `this` register, so the static bit
        // is part of the signature it asks for, not a check it runs afterwards.
        let static_values = request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN, MATCH_STATIC_VALUES_ONLY);
        assert_eq!((static_values.allow_static, static_values.require_static), (true, true), "a wrapper without `this` asked the table as if it had one");
        assert_eq!(static_values.ret, Some(Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), "the HighSpeedType getter's bool was not part of the ask");

        // A zero argument getter is safe either way: a static one ignores the `this` it is handed.
        let getter = request_for(&[], Il2CppTypeEnum_IL2CPP_TYPE_R4, MATCH_GETTER_EITHER);
        assert_eq!((getter.allow_static, getter.require_static), (true, false), "a getter refused a static method it could have taken");

        // C48: the wider walk belongs to the pointer declaring wrappers, and only to them.
        assert!(request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST, Il2CppTypeEnum_IL2CPP_TYPE_R4], Il2CppTypeEnum_IL2CPP_TYPE_VOID, MATCH_GENERIC_REF).generic_slots);
        assert!(request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE], Il2CppTypeEnum_IL2CPP_TYPE_VOID, MATCH_STATIC_GENERIC_REF).generic_slots);
    }

    // C15, the shape this client's dump prints at `introspect.log:1188-1189`: `GetBodyShader/2` is a
    // `static` overload and an instance one, under one name, one argument count and the same two
    // parameter enums. The matcher compares enums, so the two are one question; what the question
    // now carries is the static bit the wrapper's own shape needs, which is what tells them apart
    // (`symbols::select_overload` runs on candidates carrying their own return type and static bit).
    #[test]
    fn the_wrapper_shape_a_request_carries_is_what_separates_a_static_pair() {
        let instance_wrapper = request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE], Il2CppTypeEnum_IL2CPP_TYPE_CLASS, MATCH_INSTANCE_VALUE);
        let static_wrapper = request_for(&[Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE], Il2CppTypeEnum_IL2CPP_TYPE_CLASS, MATCH_STATIC_VALUE);

        assert!(!instance_wrapper.allow_static && !instance_wrapper.require_static);
        assert!(static_wrapper.require_static, "the two profiles asked the table the same question");
        assert_eq!(instance_wrapper.params, static_wrapper.params, "the parameter list alone is what the old walk asked with");
    }

    // C39. The read half of the time scale arithmetic, on the number a Story getter hands back:
    // a stored pause and a stored slow motion stay as the game stored them, the game's neutral
    // 1.0 is what the option exists to raise, the ceiling binds the raise, and a scale the game
    // already holds above the ceiling is not pulled down.
    #[test]
    fn a_getter_scales_the_games_neutral_time_scale_and_a_pause_never() {
        assert_eq!(scale_read_time_scale(1.0, 5.0), 5.0, "the game's 1.0 was left alone");
        assert_eq!(scale_read_time_scale(1.0, MAX_FACTOR), MAX_TIME_SCALE, "the raise was not capped");
        assert_eq!(scale_read_time_scale(2.0, 2.0), 4.0, "a scale above 1.0 was not raised");
        assert_eq!(scale_read_time_scale(0.0, 5.0), 0.0, "a stored pause became a fast forward");
        assert_eq!(scale_read_time_scale(0.5, 3.0), 0.5, "a stored slow motion was raised");
        assert_eq!(scale_read_time_scale(8.0, 2.0), 8.0, "the option slowed a scale the game already held");
        assert_eq!(scale_read_time_scale(1.0, 1.0), 1.0, "the neutral factor is not a speed-up");
        assert!(scale_read_time_scale(f32::NAN, 5.0).is_nan(), "NAN was scaled instead of passed on");

        // The factor side of that guard lives in the mirror: `normalize` is what writes
        // `STORY_FACTOR`, and a hand edited or unreadable config falls back to 1.0, so the
        // getter path never sees a factor it cannot multiply by.
        assert_eq!(normalize(f32::NAN), 1.0, "a NAN story_speed reached the getter path");
        assert_eq!(normalize(1000.0), MAX_FACTOR, "the mirror ceiling did not hold");
        assert_eq!(normalize(f32::INFINITY), 1.0, "an unreadable config reached the getter path");
    }

    // C39. The write half keeps the C12 line: the mod never raises a value the game is putting
    // into Time.timeScale at or below its neutral 1.0, C35's ceiling binds every raise the mod
    // itself makes, and the half never lowers a scale, so it agrees with the read half and with
    // AGENTS section 5 (time scales only go up, and are capped).
    #[test]
    fn the_write_half_still_refuses_the_games_neutral_time_scale() {
        assert_eq!(apply_time_scale(1.0, 2.0), 1.0, "a game sitting at 1.0 was pushed to the lever");
        assert_eq!(apply_time_scale(0.0, 2.0), 0.0, "a pause was raised");
        assert_eq!(apply_time_scale(0.5, 3.0), 0.5, "C35's slow motion came out 1.5 again");
        assert_eq!(apply_time_scale(4.0, 2.0), 5.0, "the game's own fast forward was not raised");
        assert_eq!(apply_time_scale(MAX_TIME_SCALE, 2.0), MAX_TIME_SCALE, "a second raise compounded past the ceiling");
        assert_eq!(apply_time_scale(8.0, 2.0), 8.0, "the write path pulled a scale the game held above the ceiling back to 5.0");
    }

    // C39, on the path the three installed Story getter hooks actually take: `def_getter_hook!`
    // calls `scale_time_scale(raw, Group::Story)`, and the raw value runs 1, 2 and 3 measured is
    // the game's 1.0 (`StoryTimeline_getTimeScaleAfterEndStory 1 -> 5`).
    #[test]
    fn story_speed_raises_the_story_time_scale_the_getters_measured_live() {
        let _turn = pass_turn();

        let mirror = STORY_FACTOR.load(Ordering::Relaxed);
        STORY_FACTOR.store(5.0f32.to_bits(), Ordering::Relaxed);

        assert_eq!(scale_time_scale(1.0, Group::Story), 5.0, "story_speed changed nothing: 1 -> 1");
        assert_eq!(scale_time_scale(2.0, Group::Story), 5.0, "the ceiling did not bind the getter");
        assert_eq!(scale_time_scale(0.0, Group::Story), 0.0, "a getter's pause was raised");
        assert_eq!(scale_time_scale(0.5, Group::Story), 0.5, "a getter's slow motion was raised");
        assert_eq!(scale_time_scale(8.0, Group::Story), 8.0, "the getter lowered the game's own scale");

        STORY_FACTOR.store(mirror, Ordering::Relaxed);
    }

    // C40. The inversion C35 was opened to prevent, reached from the lever side instead of the
    // game's side: `MIN_TIME_SCALE` let a configured lever down to 0.1, the Config Editor slider
    // offers that range, and the ceiling arithmetic only caps upward, so a lever of 0.1 turned the
    // game's own 4.0 fast forward into 0.4. The lever is a speed up, so its floor is its neutral
    // value, in the arithmetic and in the clamp every consumer of it reads through.
    #[test]
    fn a_time_scale_lever_below_one_never_lowers_a_time_scale() {
        let _turn = pass_turn();

        assert_eq!(apply_time_scale(4.0, 0.1), 4.0, "the write half slowed the game's fast forward to 0.4");
        assert_eq!(apply_time_scale(4.0, 0.5), 4.0, "a lever of 0.5 halved the game's 4.0");
        assert_eq!(apply_time_scale(2.0, 0.9), 2.0);
        assert_eq!(apply_time_scale(1.0, 0.1), 1.0, "a sub 1 lever pushed the game's 1.0 into slow motion");
        assert_eq!(apply_time_scale(0.5, 0.1), 0.5, "a slow motion was changed");
        assert_eq!(scale_read_time_scale(2.0, 0.5), 2.0, "the read half lowered a getter's scale");
        assert_eq!(scale_read_time_scale(1.0, 0.1), 1.0);

        // The clamp `refresh_time_scale` and `refresh_config_mirrors` run the config through
        // before it reaches `TIME_SCALE`, and the Config Editor slider range is these constants.
        assert_eq!(MIN_TIME_SCALE, 1.0, "the lever's floor sat below its neutral value");
        assert_eq!(normalize_time_scale(0.1), 1.0, "the Config Editor's left end was a slow down");
        assert_eq!(normalize_time_scale(0.0), 1.0, "a hand edited 0 reached Time.timeScale");
        assert_eq!(normalize_time_scale(-5.0), 1.0);
        assert_eq!(normalize_time_scale(1.0), 1.0, "the neutral default is not a speed-up");
        assert_eq!(normalize_time_scale(2.0), 2.0, "an in range lever was changed");
        assert_eq!(normalize_time_scale(1000.0), MAX_TIME_SCALE);
        assert_eq!(normalize_time_scale(f32::NAN), 1.0);

        // Dragging the slider to its left end, the way egui lands it on the range start.
        assert_eq!(egui_snap(MIN_TIME_SCALE as f64..=MAX_TIME_SCALE as f64, 0.1, 0.0), 1.0, "the slider's left end is not inert");

        // Nothing wrote the mirror in this test process, so the write path a build at the
        // defaults takes hands the game's own value back to it.
        assert_eq!(time_scale(), 1.0, "the default mirrored to a slow down");
        assert_eq!(scale_game_time_scale(4.0), 4.0, "the write path changed a value at the default lever");
    }

    // C2, at the five boundaries `def_getter_hook!` arms. The wrappers these tests call are written
    // by that macro, through the arm the five shipped instantiations delegate to, with the game half
    // supplied by the test: a unit test reaches no trampoline and no `Hachimi::instance()` (AGENTS
    // section 4), and the shipped body's registry lookup ends the test process on its cold branch.
    // The trip counters are process wide, so these cases take the barrier's one turn (AGENTS section
    // 4: the probe/barrier counters are shared by several modules' tests).

    #[test]
    fn a_getter_call_that_reaches_the_game_still_scales_the_value_it_hands_back() {
        // The migration must not have changed what a clean call does: the original is called, its
        // duration is divided by the group factor, and nothing tripped.
        let _turn = pass_turn();
        SCREENS_FACTOR.store(4.0f32.to_bits(), Ordering::Release);

        let call: extern "C" fn(*mut Il2CppObject) -> f32 = GetterThatScalesCleanly;

        assert_eq!(call(std::ptr::null_mut()), 0.6, "a migrated getter stopped scaling the duration it hands the game");
    }

    #[test]
    fn a_getter_with_no_trampoline_stays_inert_and_hands_the_game_no_zero_scale() {
        use crate::il2cpp::hook::guard;

        let _turn = guard::barrier_turn();
        let before_panics = guard::panic_trip_count();
        let before_invented = guard::invented_trip_count();

        let duration: extern "C" fn(*mut Il2CppObject) -> f32 = GetterWithNoOriginalForADuration;
        let scale: extern "C" fn(*mut Il2CppObject) -> f32 = GetterWithNoOriginalForATimeScale;

        assert_eq!(duration(std::ptr::null_mut()), 0.0,
            "the duration getter did not answer with the no-wait value its own site states");
        assert_eq!(scale(std::ptr::null_mut()), MIN_TIME_SCALE,
            "a missing trampoline handed the story timeline a 0 time scale, which is a pause");

        assert_eq!(guard::panic_trip_count(), before_panics);
        assert_eq!(guard::invented_trip_count(), before_invented,
            "the barrier had to make up a value the wrapper states at its call site");
    }

    #[test]
    #[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
    fn a_getter_that_has_no_original_takes_no_fault_because_it_calls_through_nothing() {
        use crate::il2cpp::hook::guard;

        let _turn = guard::barrier_turn();
        let before_faults = guard::fault_trip_count();
        let before_jumps_to_zero = guard::null_target_fault_trip_count();

        let duration: extern "C" fn(*mut Il2CppObject) -> f32 = GetterWithNoOriginalForADuration;
        let scale: extern "C" fn(*mut Il2CppObject) -> f32 = GetterWithNoOriginalForATimeScale;

        duration(std::ptr::null_mut());
        scale(std::ptr::null_mut());

        // C1, at the boundary this macro used to leave unguarded: the old body transmuted the
        // registry's 0 into a function pointer and called it, which is an execution at address 0 -
        // the signature `null_target_fault_trip_count` counts. `get_orig_fn_guarded!` is the same
        // cache answering `None`, so nothing is called and nothing is counted.
        assert_eq!(guard::fault_trip_count(), before_faults, "a call through 0 was still made here");
        assert_eq!(guard::null_target_fault_trip_count(), before_jumps_to_zero, "the barrier saw a jump to 0");
    }

    #[test]
    fn a_getter_trip_after_the_game_answered_hands_back_the_games_own_value() {
        use crate::il2cpp::hook::guard;

        let _turn = guard::barrier_turn();
        let before_panics = guard::panic_trip_count();
        let before_answered = guard::answered_trip_count();
        let before_invented = guard::invented_trip_count();

        let call: extern "C" fn(*mut Il2CppObject) -> f32 = GetterPanickingAfterTheGameAnswered;

        // 2.4 is what the game's method returned. The barrier stopped the mod half *after* that, and
        // the answer it hands back is the game's number - not a 0 duration the game then acts on.
        assert_eq!(call(std::ptr::null_mut()), 2.4, "the barrier threw away the answer the game had already given");

        assert_eq!(guard::panic_trip_count(), before_panics + 1);
        assert_eq!(guard::answered_trip_count(), before_answered + 1, "the trip was not reported as answered");
        assert_eq!(guard::invented_trip_count(), before_invented,
            "a trip with the game's answer in hand invented a value anyway");
    }

    #[test]
    #[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
    fn a_getter_fault_after_the_game_answered_hands_back_the_games_own_value_too() {
        use crate::il2cpp::hook::guard;

        let _turn = guard::barrier_turn();
        let before_faults = guard::fault_trip_count();
        let before_answered = guard::answered_trip_count();
        let before_invented = guard::invented_trip_count();

        let call: extern "C" fn(*mut Il2CppObject) -> f32 = GetterFaultingAfterTheGameAnswered;

        assert_eq!(call(std::ptr::null_mut()), 2.4,
            "a fault in the mod half cost the story timeline the scale the game had just produced");

        assert_eq!(guard::fault_trip_count(), before_faults + 1);
        assert_eq!(guard::last_fault_code(), 0xC0000005, "the C frame did not take this as an access violation");
        assert_eq!(guard::answered_trip_count(), before_answered + 1);
        assert_eq!(guard::invented_trip_count(), before_invented, "the barrier made up a time scale");
    }

    #[test]
    #[cfg(all(target_env = "msvc", target_arch = "x86_64"))]
    fn a_getter_fault_inside_the_game_method_is_taken_at_the_boundary_and_refused() {
        use crate::il2cpp::hook::guard;

        let _turn = guard::barrier_turn();
        let before_faults = guard::fault_trip_count();
        let before_jumps_to_zero = guard::null_target_fault_trip_count();
        let before_invented = guard::invented_trip_count();

        // The trip with nothing to answer: the game's own method faulted before it produced a value,
        // so no answer of the game's exists and the wrapper replaying the call would fault again. The
        // barrier's refusal - a zero, counted, and named in the log once for this wrapper - is the
        // honest answer, and the process surviving the call is the half that was missing before.
        let call: extern "C" fn(*mut Il2CppObject) -> f32 = GetterFaultingInsideTheOriginal;

        assert_eq!(call(std::ptr::null_mut()), 0.0, "the wrapper did not answer the trip itself");

        assert_eq!(guard::fault_trip_count(), before_faults + 1);
        assert_eq!(guard::last_fault_code(), 0xC0000005);
        assert_eq!(guard::null_target_fault_trip_count(), before_jumps_to_zero, "a read fault is not a call through 0");
        assert_eq!(guard::invented_trip_count(), before_invented + 1, "the refusal the barrier had to make was not counted");
    }

    // Run 26 measured 7866, 8182 and 7866 ms of status panel held off in the session whose first hit line read
    // `SingleModeMainViewTrainingCutStatus.PlayIn 2.4 -> 0.12`, and the two cuts after the group was turned off
    // closed hole free at 1792 and 2042 ms. The play in animation runs on `CoroutinePlayIn`, not on the float
    // `PlayIn` is handed, so this door must not return to the scaling list without a run that shows the hole
    // does not return with it.
    #[test]
    fn the_training_cut_status_play_in_is_not_a_scaling_point() {
        assert_eq!(TRAINING_HIT_SLOTS.len(), 2, "the training scaling points are the gauge blend time and the plate list interval");
        assert!(!TRAINING_HIT_SLOTS.iter().any(|(_, name)| name.contains("PlayIn")), "PlayIn went out of the list on run 26's numbers and stays out");
    }
}
