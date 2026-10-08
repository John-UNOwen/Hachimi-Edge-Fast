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
use std::ffi::CString;
use std::collections::HashMap;
use std::os::raw::{c_int, c_void};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use once_cell::sync::Lazy;

use crate::core::Hachimi;
use crate::il2cpp::{
    api::{
        il2cpp_class_from_name, il2cpp_class_get_field_from_name, il2cpp_class_get_method_from_name,
        il2cpp_field_get_flags, il2cpp_field_get_type, il2cpp_field_is_literal,
        il2cpp_field_static_get_value, il2cpp_field_static_set_value, il2cpp_method_get_return_type,
    },
    types::*,
};

const FIELD_ATTRIBUTE_STATIC: c_int = 0x10;
const FIELD_ATTRIBUTE_LITERAL: c_int = 0x40;

// Upper bound on how much of an animation may be removed in one step.
const MAX_FACTOR: f32 = 20.0;
const METHOD_ATTRIBUTE_STATIC: u16 = 0x0010;

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

// True once any constant has been rewritten, so returning every factor to 1.0 still
// performs the single pass that puts the shipped values back.
static WAS_SCALED: AtomicBool = AtomicBool::new(false);

// Mirrored for hot paths (timeline getters run every frame while a cutscene plays).
static TRANSITION_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static SCREENS_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static STORY_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

// A ceiling on the time-scale lever, independent of the slider: a game path that reads
// a scaled value back and writes it again would otherwise compound every frame.
const MAX_TIME_SCALE: f32 = 5.0;

pub fn story_factor() -> f32 {
    f32::from_bits(STORY_FACTOR.load(Ordering::Acquire))
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

// A time scale only ever goes up, and only to MAX_TIME_SCALE.
pub fn scale_time_scale(value: f32, group: Group) -> f32 {
    let factor = factor(group);

    if factor == 1.0 || !value.is_finite() {
        return value;
    }

    (value * factor).min(MAX_TIME_SCALE).max(value)
}

// A detour installed against the wrong overload reads its arguments from the wrong
// registers, so the parameter list and the return type both have to match the declared
// method before anything is hooked.
pub unsafe fn resolve_method(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    resolve_method_any(class, name, params, ret, false)
}

// `allow_static` is only for wrappers that carry no real arguments: a static method
// simply ignores the `this` register such a wrapper hands over.
unsafe fn resolve_method_any(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
    allow_static: bool,
) -> usize {
    let method = match crate::il2cpp::symbols::get_method_overload(class, name, params) {
        Ok(method) => method,
        Err(_) => {
            debug!("AnimationSpeed: {} has no overload with the expected signature", name);
            return 0;
        }
    };

    if (*method).is_generic() != 0 {
        debug!("AnimationSpeed: {} is generic", name);
        return 0;
    }

    // A wrapper reserves the first register for `this`. Against a static method that
    // register holds the first real argument, so every following argument would be read
    // from the wrong place. `flags` is the MethodAttributes word, and bit 0x0010 is static.
    if !allow_static && (*method).flags & METHOD_ATTRIBUTE_STATIC != 0 {
        debug!("AnimationSpeed: {} is static, its arguments would be misread", name);
        return 0;
    }

    let return_type = il2cpp_method_get_return_type(method);

    if return_type.is_null() || (*return_type).type_() != ret {
        warn!(
            "AnimationSpeed: {} returns il2cpp type {}, wrapper expects {}",
            name,
            if return_type.is_null() { u32::MAX } else { (*return_type).type_() },
            ret
        );
        return 0;
    }

    (*method).methodPointer
}

// Scaling only the read half of a settable property is unsafe: a game path that does
// `prop = prop - 1` writes the already divided value back into the backing field, and
// the value collapses geometrically instead of being shortened once. A getter is only
// wrapped when the class has no matching setter to write it back.
unsafe fn resolve_getter(class: *mut Il2CppClass, name: &str, ret: Il2CppTypeEnum) -> usize {
    // Zero-argument getters are safe either way: a static one simply ignores the `this`
    // register the wrapper hands over.
    let addr = resolve_method_any(class, name, &[], ret, true);

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

pub fn hit(slot: usize, name: &str, raw: f32, scaled: f32) {
    if raw != scaled && slot < HIT_SLOTS && !FIRST_HIT[slot].swap(true, Ordering::AcqRel) {
        debug!("AnimationSpeed: {name} {raw} -> {scaled}");
    }
}

// These getters hand out a hardcoded duration or a playback scale, and are the only way
// to reach a `const` duration that is not passed as an argument anywhere.
macro_rules! def_getter_hook {
    ($hook:ident, $group:expr, $scale:ident, $slot:literal) => {
        extern "C" fn $hook(this: *mut Il2CppObject) -> f32 {
            type Orig = extern "C" fn(*mut Il2CppObject) -> f32;

            let raw = get_orig_fn!($hook, Orig)(this);
            let scaled = $scale(raw, $group);

            hit($slot, stringify!($hook), raw, scaled);

            scaled
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

fn factors() -> (f32, f32, f32) {
    let config = Hachimi::instance().config.load();

    (
        normalize(config.transition_speed),
        normalize(config.result_screen_speed),
        normalize(config.story_speed),
    )
}

fn read_static(field: *mut FieldInfo, kind: Il2CppTypeEnum) -> f64 {
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

fn write_static(field: *mut FieldInfo, kind: Il2CppTypeEnum, original: f64, factor: f32) -> f64 {
    match kind {
        Il2CppTypeEnum_IL2CPP_TYPE_R4 => {
            let value = original as f32 / factor;
            il2cpp_field_static_set_value(field, std::ptr::from_ref(&value) as *mut c_void);
            value as f64
        }
        Il2CppTypeEnum_IL2CPP_TYPE_R8 => {
            let value = original / factor as f64;
            il2cpp_field_static_set_value(field, std::ptr::from_ref(&value) as *mut c_void);
            value
        }
        _ => {
            // Frame counts round to the nearest frame but never below one frame unless
            // the field is already zero: a zero wait between story blocks can starve the
            // block-advance coroutine. A negative offset moves towards zero, which is
            // the same shortening in the other direction.
            let scaled = original / factor as f64;
            let value = if original > 0.0 { scaled.max(1.0).round() as i32 }
            else if original < 0.0 { scaled.min(-1.0).round() as i32 }
            else { 0 };

            il2cpp_field_static_set_value(field, std::ptr::from_ref(&value) as *mut c_void);
            value as f64
        }
    }
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
    apply();
}

def_getter_hook!(CountupModifier_getDuration, Group::Screens, scale_duration, 8);
def_getter_hook!(TextModifier_getDuration, Group::Screens, scale_duration, 9);
def_getter_hook!(TextModifier_getDelay, Group::Screens, scale_duration, 10);
def_getter_hook!(TrainingFooter_GetCloseAnimWaitTime, Group::Screens, scale_duration, 11);
def_getter_hook!(TrainingCuttClip_getDelayTime, Group::Story, scale_duration, 12);
def_getter_hook!(SingleModeUtils_GetHighSpeedPlayDuration, Group::Story, scale_duration, 13);
def_getter_hook!(StoryTimeline_getTimeScaleEventWipe, Group::Story, scale_time_scale, 14);
def_getter_hook!(StoryTimeline_getTimeScaleAfterEndStory, Group::Story, scale_time_scale, 15);
def_getter_hook!(SingleModeUtils_GetCutTimeScale, Group::Story, scale_time_scale, 16);

// One float argument in a fixed position, so the wrapper cannot misread it. This is the
// grand result screen's own entry point for its hardcoded DURATION constant.
type GrandResultFadeInFromRightFn = extern "C" fn(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32);
extern "C" fn TeamStadiumGrandResult_FadeInContentFromRight(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32) {
    let scaled = scale_duration(duration, Group::Screens);
    hit(6, "TeamStadiumGrandResultViewController.FadeInContentFromRight", duration, scaled);

    get_orig_fn!(TeamStadiumGrandResult_FadeInContentFromRight, GrandResultFadeInFromRightFn)(this, content, scaled);
}

// A getter that takes the thing it is measuring and still returns a duration.
type TrainingFooterGetItemAnimDurationFn = extern "C" fn(this: *mut Il2CppObject, item: *mut Il2CppObject) -> f32;
extern "C" fn TrainingFooter_GetItemAnimDuration(this: *mut Il2CppObject, item: *mut Il2CppObject) -> f32 {
    let raw = get_orig_fn!(TrainingFooter_GetItemAnimDuration, TrainingFooterGetItemAnimDurationFn)(this, item);
    let scaled = scale_duration(raw, Group::Screens);

    hit(7, "SingleModeMainViewTrainingFooter.GetItemAnimDuration", raw, scaled);

    scaled
}

// Every class named below has to be resolved before the getters are installed, because
// the install macro looks the class up by name.
const GETTER_CLASSES: &[&str] = &[
    "CountupModifier", "TextModifier", "SingleModeMainViewTrainingFooter",
    "StoryTimelineTrainingCuttClipData", "SingleModeUtils",
    "StoryTimelineController", "TeamStadiumGrandResultViewController",
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
    install_getter!(classes, TrainingFooter_GetCloseAnimWaitTime, SingleModeMainViewTrainingFooter, GetCloseAnimWaitTime);
    install_getter!(classes, TrainingCuttClip_getDelayTime, StoryTimelineTrainingCuttClipData, get_DelayTime);
    install_getter!(classes, SingleModeUtils_GetHighSpeedPlayDuration, SingleModeUtils, GetHighSpeedPlayDuration);
    install_getter!(classes, StoryTimeline_getTimeScaleEventWipe, StoryTimelineController, get_TimeScaleEventWipe);
    install_getter!(classes, StoryTimeline_getTimeScaleAfterEndStory, StoryTimelineController, get_TimeScaleAfterEndStory);
    install_getter!(classes, SingleModeUtils_GetCutTimeScale, SingleModeUtils, GetCutTimeScale);

    if let Some(class) = classes.get("TeamStadiumGrandResultViewController").copied() {
        let addr = unsafe { resolve_method(
            class, "FadeInContentFromRight",
            &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4],
            Il2CppTypeEnum_IL2CPP_TYPE_VOID,
        ) };

        if addr != 0 { new_hook!(addr, TeamStadiumGrandResult_FadeInContentFromRight); }
    }

    if let Some(class) = classes.get("SingleModeMainViewTrainingFooter").copied() {
        let addr = unsafe { resolve_method(
            class, "GetItemAnimDuration",
            &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
            Il2CppTypeEnum_IL2CPP_TYPE_R4,
        ) };

        if addr != 0 { new_hook!(addr, TrainingFooter_GetItemAnimDuration); }
    }
}

pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Release);
}

pub fn apply_if_dirty() {
    // While any option is on this also re-asserts every frame, which is what keeps a
    // static constructor that the game runs later (a class first touched by the scene
    // you just entered) from quietly restoring the shipped duration.
    if DIRTY.swap(false, Ordering::AcqRel) || WAS_SCALED.load(Ordering::Acquire) {
        apply();
    }
}

pub fn apply() {
    let (transition, screens, story) = factors();
    // Cached first, and even when there is nothing to rewrite: the detours that scale
    // arguments read these values, and a build with no rewritable duration field would
    // otherwise ignore every option.
    TRANSITION_FACTOR.store(transition.to_bits(), Ordering::Release);
    SCREENS_FACTOR.store(screens.to_bits(), Ordering::Release);
    STORY_FACTOR.store(story.to_bits(), Ordering::Release);

    let mut entries = ENTRIES.lock().unwrap();
    if entries.is_empty() {
        return;
    }

    // View changes re-assert the constants, so the common case (nothing enabled) has
    // to bail out without touching the game's fields. WAS_SCALED makes the single
    // pass back to 1.0 that actually restores the shipped values.
    if transition == 1.0 && screens == 1.0 && story == 1.0 && !WAS_SCALED.swap(false, Ordering::AcqRel) {
        return;
    }

    let mut scaled = 0usize;

    for entry in entries.iter_mut() {
        let factor = match entry.group {
            Group::Transition => transition,
            Group::Screens => screens,
            Group::Story => story,
        };

        let field = entry.info as *mut FieldInfo;
        let current = read_static(field, entry.kind);

        // Anything the game put here since our last write is the new baseline, and
        // comparing against our own last write is what stops a factor from compounding
        // against a value the game reassigns at runtime.
        if current != entry.last_written {
            entry.original = current;
        }

        if entry.original == 0.0 {
            // Class not initialised yet, or the shipped constant really is zero.
            continue;
        }

        let written = write_static(field, entry.kind, entry.original, factor);
        entry.last_written = written;

        if factor != 1.0 {
            WAS_SCALED.store(true, Ordering::Release);
        }

        if current != written {
            scaled += 1;
            debug!("AnimationSpeed: {}.{} {} -> {} (x{})", entry.class, entry.field, current, written, factor);
        }
    }

    if scaled > 0 {
        info!("AnimationSpeed: shortened {} animation duration fields", scaled);
    }
}
