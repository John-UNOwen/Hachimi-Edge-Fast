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
use std::os::raw::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use once_cell::sync::Lazy;

use crate::core::Hachimi;
use crate::core::hachimi::Config;
use crate::il2cpp::hook::umamusume::TrainingCuttProbe;
use crate::il2cpp::{
    api::{
        il2cpp_class_from_name, il2cpp_class_get_field_from_name, il2cpp_class_get_method_from_name,
        il2cpp_class_instance_size,
        il2cpp_field_get_flags, il2cpp_field_get_type, il2cpp_field_is_literal,
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    // Screen-to-screen transitions: view change fades, wipes, loading overlays.
    Transition,
    // Training turn / race result screens: plates, count-ups, reward cascades.
    Screens,
    // Story cutscene timeline.
    Story,
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

    // --- Training plates and result screens ---
    spec!("TrainingParamChangeA2U", "ANIMATION_TIME_HIGH_SPEED", Group::Screens),
    spec!("TrainingParamChangePlate", "TYPEWRITE_DURATION", Group::Screens),
    spec!("TrainingParamChangePlate", "NEXT_WAIT_DURATION", Group::Screens),
    spec!("SingleModeMainTrainingCuttController", "FLASH_LABEL_SPEED_UP_SUCCESS_IN", Group::Screens),
    spec!("SingleModeMainTrainingCuttController", "FLASH_LABEL_SPEED_UP_FAILURE_IN", Group::Screens),
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
// Indexed by `group_index`: Transition, Screens, Story.
static APPLIED_FACTORS: [AtomicU32; 3] = [const { AtomicU32::new(f32::NAN.to_bits()) }; 3];

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
static STORY_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static TIME_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static UI_ANIMATION_SCALE: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

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

// The ceiling `ui_animation_scale` may reach in code, independent of what any slider offers.
// The DOTween `Update` detour multiplies the delta time it hands the tween library once per
// tween tick, so this lever removes animation time the same way a group factor does and gets
// the same bound: MAX_FACTOR. The Config Editor and the first time setup wizard both allow
// 0.1..=1000.0, and the wizard is the only place a new user ever touches the option (C5).
// The floor is the slider's own: config.json is deserialized unbounded, and a hand edited 0
// would freeze every tween the game animates on DOTween's clock.
pub const MAX_UI_ANIMATION_SCALE: f32 = MAX_FACTOR;
pub const MIN_UI_ANIMATION_SCALE: f32 = 0.1;

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
// of the live client.
const CHOICE_AUTO_SELECT_TRIGGER_TIME: f32 = 0.75;

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

// The factors are cached so a detour that runs per call reads one atomic instead of
// loading the whole config.
pub fn factor(group: Group) -> f32 {
    let bits = match group {
        Group::Transition => TRANSITION_FACTOR.load(Ordering::Relaxed),
        Group::Screens => SCREENS_FACTOR.load(Ordering::Relaxed),
        Group::Story => STORY_FACTOR.load(Ordering::Relaxed),
    };

    f32::from_bits(bits)
}

// Most of Gallop's durations are `const`, which IL2CPP folds into the call sites, so
// the value has no writable storage (see the init log). The methods that play the
// animation still receive it as a plain argument, so scaling the argument is the same
// adjustment applied one level earlier, and it does not care how the caller stores it.
pub fn scale_duration(value: f32, group: Group) -> f32 {
    let factor = factor(group);

    if factor == 1.0 || !value.is_finite() || value == 0.0 {
        return value;
    }

    // Dividing keeps the sign, so a negative offset moves towards zero instead of
    // flipping into a duration that did not exist before.
    value / factor
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

// The training scaling points as run 11 found them. The five these replaced installed in run 8 and
// printed no call line in four career runs (C51), so they were hooks on doors this client never opens.
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
    TRANSITION_FACTOR.store(normalize(config.transition_speed).to_bits(), Ordering::Release);
    SCREENS_FACTOR.store(normalize(config.result_screen_speed).to_bits(), Ordering::Release);
    STORY_FACTOR.store(normalize(config.story_speed).to_bits(), Ordering::Release);
    TIME_SCALE.store(normalize_time_scale(config.time_scale).to_bits(), Ordering::Release);
    UI_ANIMATION_SCALE.store(normalize_ui_animation_scale(config.ui_animation_scale).to_bits(), Ordering::Release);
    // Clamped here so neither story choice site divides by the delay itself: `CheckChoiceAutoTap`
    // runs while a choice is up and `GetTimeScaleByHighSpeedType` is a story path getter, so both
    // read this mirror instead of the config (C24).
    STORY_CHOICE_AUTO_SELECT_MULT.store(
        normalize_story_choice_auto_select_multiplier(config.story_choice_auto_select_delay).to_bits(),
        Ordering::Release,
    );
}

// The factors as the write loop sees them: the mirrors, not the config. Indexed by
// `group_index`.
fn mirrored_factors() -> [f32; 3] {
    [factor(Group::Transition), factor(Group::Screens), factor(Group::Story)]
}

// The three speed groups, in the order `FIELDS` declares them.
fn group_index(group: Group) -> usize {
    match group {
        Group::Transition => 0,
        Group::Screens => 1,
        Group::Story => 2,
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
fn plan_pass(factors: [f32; 3]) -> [bool; 3] {
    [
        plan_group(factors[0], f32::from_bits(APPLIED_FACTORS[0].load(Ordering::Acquire))),
        plan_group(factors[1], f32::from_bits(APPLIED_FACTORS[1].load(Ordering::Acquire))),
        plan_group(factors[2], f32::from_bits(APPLIED_FACTORS[2].load(Ordering::Acquire))),
    ]
}

/// Close a pass out. A group is marked as written at the factor it was written at unless a field
/// in it had no baseline this pass, which is the state that asks the next pass to look again
/// (a class whose static constructor has not run reads as zero until its scene loads). Leaving
/// the marker unset is what turns "retry" into one pass per view change, not one per frame.
fn finish_pass(rewrite: [bool; 3], factors: [f32; 3], no_baseline: [usize; 3]) {
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
def_getter_hook!(CountupModifier_getDuration, Group::Screens, scale_duration, 8, no_answer { 0.0 });
def_getter_hook!(TextModifier_getDuration, Group::Screens, scale_duration, 9, no_answer { 0.0 });
def_getter_hook!(TextModifier_getDelay, Group::Screens, scale_duration, 10, no_answer { 0.0 });
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
// The HP gauge blend time and the plate list interval are the training doors this fork does scale, and both
// are doors whose value the game uses directly for the thing it draws.
//
// The HP gauge progress bar blend time, the only float the gauge class takes. Run 11 measured 63,731 frames on
// the screen this class draws and never measured one animation door of it.
type HpGaugeSetBlendTimeFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
def_detour! {
    HpGauge_SetBlendTimeSpeed(this: *mut Il2CppObject, time: f32) {
            let scaled = scale_duration(time, Group::Screens);
        hit(11, "SingleModeMainViewHpGauge.SetProgressbarBlendTime", time, scaled);

        get_orig_fn!(HpGauge_SetBlendTimeSpeed, HpGaugeSetBlendTimeFn)(this, scaled);
    }
}

// The training stat plate cascade. Run 13 measured 10,324 ms inside one training cut, of which 439 ms
// waited for a tap and 10,034 ms was the cut playing before it asked, while the cut's own timeline
// reported 2.4 s of total length. The door on that path carrying a duration is `InitializePlateList`,
// reached six times in the session on a float of 1.0, next to twelve gauge plays whose duration this
// group had already cut from 2.4 s to 0.12 s. The list is a generic parameter and travels as a pointer
// untouched (C48), and only the float is scaled. The door used to stand in TrainingCuttProbe, which
// installs nothing unless debug_mode is on, and a speed option has to work for a player who never turns
// that switch: the scaling points run 12 and 13 were only reachable because the probe happened to be on.
type PlateInitializeListFn = extern "C" fn(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32);
def_detour! {
    TrainingParamChangeUI_InitializePlateListSpeed(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32) {
            let scaled = scale_duration(interval, Group::Screens);

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
            let scaled = scale_duration(duration, Group::Screens);
        hit(6, "TeamStadiumGrandResultViewController.FadeInContentFromRight", duration, scaled);

        get_orig_fn!(TeamStadiumGrandResult_FadeInContentFromRight, GrandResultFadeInFromRightFn)(this, content, scaled);
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
    let mut no_baseline = [0usize; 3];

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

    // The pass reads and writes process wide mirrors, markers and counters, and `cargo test`
    // runs cases on several threads, so the cases that drive it take turns and hand the state
    // back when they finish. A turn starts with every applied marker at NAN, the state a process
    // that has never written a group holds, which is also the state a build at the shipped
    // defaults is in.
    static PASS_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct PassTurn {
        _turn: MutexGuard<'static, ()>,
        saved_mirrors: [u32; 6],
        saved_applied: [u32; 3],
    }

    fn pass_turn() -> PassTurn {
        let turn = PASS_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        let saved_mirrors = [
            TRANSITION_FACTOR.load(Ordering::Relaxed),
            SCREENS_FACTOR.load(Ordering::Relaxed),
            STORY_FACTOR.load(Ordering::Relaxed),
            TIME_SCALE.load(Ordering::Relaxed),
            UI_ANIMATION_SCALE.load(Ordering::Relaxed),
            STORY_CHOICE_AUTO_SELECT_MULT.load(Ordering::Relaxed),
        ];
        let saved_applied = [
            APPLIED_FACTORS[0].load(Ordering::Relaxed),
            APPLIED_FACTORS[1].load(Ordering::Relaxed),
            APPLIED_FACTORS[2].load(Ordering::Relaxed),
        ];

        for marker in APPLIED_FACTORS.iter() {
            marker.store(f32::NAN.to_bits(), Ordering::Release);
        }

        PassTurn { _turn: turn, saved_mirrors, saved_applied }
    }

    impl Drop for PassTurn {
        fn drop(&mut self) {
            TRANSITION_FACTOR.store(self.saved_mirrors[0], Ordering::Release);
            SCREENS_FACTOR.store(self.saved_mirrors[1], Ordering::Release);
            STORY_FACTOR.store(self.saved_mirrors[2], Ordering::Release);
            TIME_SCALE.store(self.saved_mirrors[3], Ordering::Release);
            UI_ANIMATION_SCALE.store(self.saved_mirrors[4], Ordering::Release);
            STORY_CHOICE_AUTO_SELECT_MULT.store(self.saved_mirrors[5], Ordering::Release);

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
    fn run_planned_pass(no_baseline: [usize; 3]) -> bool {
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

        assert_eq!(factors, [1.0, 1.0, 1.0], "a shipped default mirrored to a speed up");

        // This is the exact decision `apply` bails on, before the entry lock and before the table.
        assert_eq!(plan_pass(factors), [false, false, false],
            "a build with every option at its neutral default asked to rewrite a game field");

        for _ in 0..300 {
            assert!(!run_planned_pass([0; 3]), "a pass at the shipped defaults reached the field table");
        }

        for group in [Group::Transition, Group::Screens, Group::Story] {
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
            if run_planned_pass([0; 3]) {
                working += 1;
            }
        }

        assert_eq!(working, 1, "the plan asked for a rewrite pass {} times in 300 ticks", working);
        assert_eq!(marker_of(Group::Transition), 2.0, "the pass did not mark its own factor as applied");
        assert!(marker_of(Group::Screens).is_nan(), "a group nobody changed was marked as written");

        println!(
            "C36, measured by the shipped plan: 300 ticks at transition x2 ask for one write pass, so that pass reaches at most {} fields instead of 300 passes over all {}. The counts are read off FIELDS, not off a call: the il2cpp read and write need the game's own FieldInfo, none of them resolve on this client (C13), and the apply line a run prints is what reports the calls really made.",
            transition,
            field_count(Group::Transition) + field_count(Group::Screens) + field_count(Group::Story)
        );
    }

    #[test]
    fn a_factor_change_asks_only_for_its_own_group() {
        let _turn = pass_turn();

        let mut config = Config::default();
        mirror_config(&config);

        config.result_screen_speed = 2.0;
        mirror_config(&config);
        assert_eq!(plan_pass(mirrored_factors()), [false, true, false], "the wrong group was due");
        assert!(run_planned_pass([0; 3]));
        assert_eq!(marker_of(Group::Screens), 2.0);

        config.transition_speed = 3.0;
        mirror_config(&config);
        assert_eq!(plan_pass(mirrored_factors()), [true, false, false], "the screens group was due again");
        assert!(run_planned_pass([0; 3]));
        assert_eq!(marker_of(Group::Transition), 3.0);

        config.story_speed = 2.0;
        mirror_config(&config);
        assert_eq!(plan_pass(mirrored_factors()), [false, false, true], "the story group was not due");
        assert!(run_planned_pass([0; 3]));

        // A career run's worth of view changes after that: three factor changes, three passes.
        for _ in 0..40 {
            assert!(!run_planned_pass([0; 3]), "an unchanged config re-planned a rewrite");
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
        assert!(run_planned_pass([0; 3]));
        assert_eq!(scale_value(0.4, 4.0, Il2CppTypeEnum_IL2CPP_TYPE_R4), (0.4f32 / 4.0) as f64);
        assert_eq!(marker_of(Group::Story), 4.0);

        config.story_speed = 1.0;
        mirror_config(&config);

        assert!(plan_pass(mirrored_factors())[2], "turning the option off did not ask for the restore");
        assert!(run_planned_pass([0; 3]));

        // The restore is the remembered baseline divided by 1.0, so it puts the shipped value
        // back instead of writing 1.0 into a field that never held 1.0.
        assert_eq!(scale_value(0.4, 1.0, Il2CppTypeEnum_IL2CPP_TYPE_R4), (0.4f32 / 1.0) as f64, "the shipped value was not put back");
        assert_eq!(marker_of(Group::Story), 1.0);

        for _ in 0..100 {
            assert!(!run_planned_pass([0; 3]), "the restore pass ran more than once");
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
        assert!(run_planned_pass([field_count(Group::Transition); 3]));
        assert!(marker_of(Group::Transition).is_nan(),
            "the group was marked applied at a factor none of its fields ever sat at");

        // The scene loads, the static constructor puts the shipped value in, and the next view
        // change pass finds it. That retry is one pass per view change, not one per frame.
        assert!(run_planned_pass([0; 3]), "the retry pass did not look at the group again");
        assert_eq!(marker_of(Group::Transition), 2.0);
        assert!(!run_planned_pass([0; 3]), "the same factor was written a third time");
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
