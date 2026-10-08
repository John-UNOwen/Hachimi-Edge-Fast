use std::sync::{atomic::{self, AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicUsize}, Mutex};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{AnimationSpeed, HighSpeedSetting},
        symbols::{get_method_addr, GCHandle},
        types::*,
    },
};

static mut GET_ISFINISHED_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_IsFinished, GET_ISFINISHED_ADDR, bool, this: *mut Il2CppObject);

static mut GET_TIMELINEDATA_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_TimelineData, GET_TIMELINEDATA_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);

pub static CURRENT: Mutex<Option<GCHandle>> = Mutex::new(None);
static LAST_BLOCK_ID: AtomicI32 = AtomicI32::new(-1);

pub fn last_block_id() -> i32 {
    LAST_BLOCK_ID.load(atomic::Ordering::Relaxed)
}

type GotoBlockFn = extern "C" fn(this: *mut Il2CppObject, block_id: i32, weaken_cy_spring: bool, is_update: bool, is_choice: bool);
pub extern "C" fn GotoBlock(this: *mut Il2CppObject, block_id: i32, weaken_cy_spring: bool, is_update: bool, is_choice: bool) {
    if Hachimi::instance().config.load().enable_ipc {
        let mut guard = CURRENT.lock().unwrap();

        if !(*guard).as_ref().is_none_or(|h| h.target() == this) {
            *guard = Some(GCHandle::new_weak_ref(this, false));
        }
        LAST_BLOCK_ID.store(block_id, atomic::Ordering::Relaxed);
    }

    get_orig_fn!(GotoBlock, GotoBlockFn)(this, block_id, weaken_cy_spring, is_update, is_choice);
}

// The story timeline advances by `deltaTime * TimeScale`. `get_TimeScale` and
// `get_WaitFrameCountUntilNextBlock` are deliberately not touched: both are backed by
// state the game writes (there is a `set_TimeScale`, and the wait count is counted down
// internally), and scaling only the read half of a value the game also writes makes the
// factor compound instead of being applied once. These two functions are where the
// game computes the scale it then stores, so scaling here lands in the game's own state
// exactly once.
const STORY: AnimationSpeed::Group = AnimationSpeed::Group::Story;

type GetTimeScaleByHighSpeedTypeFn = extern "C" fn(this: *mut Il2CppObject, is_high_speed: bool) -> f32;
extern "C" fn GetTimeScaleByHighSpeedType(this: *mut Il2CppObject, is_high_speed: bool) -> f32 {
    let value = get_orig_fn!(GetTimeScaleByHighSpeedType, GetTimeScaleByHighSpeedTypeFn)(this, is_high_speed);
    let scaled = AnimationSpeed::scale_time_scale(value, STORY);

    if scaled != value {
        debug!("StoryTimelineController::GetTimeScaleByHighSpeedType({}) -> {} (from {})", is_high_speed, scaled, value);
    }

    scaled
}

type GetTimeScaleHighSpeedFn = extern "C" fn(this: *mut Il2CppObject, is_high_speed: bool) -> f32;
extern "C" fn GetTimeScaleHighSpeed(this: *mut Il2CppObject, is_high_speed: bool) -> f32 {
    AnimationSpeed::scale_time_scale(
        get_orig_fn!(GetTimeScaleHighSpeed, GetTimeScaleHighSpeedFn)(this, is_high_speed),
        STORY
    )
}

type SetHighSpeedFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SetHighSpeedFrameCount(this: *mut Il2CppObject, frames: i32) {
    mark_story_activity();
    count_step(&SET_HIGH_SPEED_FRAMES_CALLS, "StoryTimelineController::SetHighSpeedFrameCount");

    // Scaling the value the game is about to store is applied exactly once, unlike
    // scaling its read side.
    let scaled = AnimationSpeed::scale_frame_count(frames, STORY);
    AnimationSpeed::hit(19, "StoryTimelineController.SetHighSpeedFrameCount", frames as f32, scaled as f32);

    get_orig_fn!(SetHighSpeedFrameCount, SetHighSpeedFrameCountFn)(this, scaled);
}

// The high speed story path reports its next step through two reference parameters, so both
// values only exist once the original has filled the caller's storage, and the byref flag has
// been confirmed on the resolved overload before anything is written back.
//
// Which half is the wait and which half is the step is not visible from the signature, so
// both are divided and never multiplied. The pair comes out of the game's own readonly
// `_highSpeedFrameCountArray`, and a value that turns out to be an index into that array
// stays inside it when it only ever gets smaller, while multiplying could walk it off the
// end. The raw pair is logged for the first few calls so a run can measure what the two
// numbers actually control.
const REF_LOG_LIMIT: usize = 6;
static REF_LOGGED: AtomicUsize = AtomicUsize::new(0);
static NULL_REF_WARNED: AtomicBool = AtomicBool::new(false);

type GetNextFrameCountHighSpeedFn = extern "C" fn(this: *mut Il2CppObject, frames: *mut f32, count: *mut i32);
extern "C" fn GetNextFrameCount_HighSpeed(this: *mut Il2CppObject, frames: *mut f32, count: *mut i32) {
    count_step(&NEXT_FRAME_CALLS, "StoryTimelineController::GetNextFrameCount_HighSpeed");

    get_orig_fn!(GetNextFrameCount_HighSpeed, GetNextFrameCountHighSpeedFn)(this, frames, count);

    if frames.is_null() || count.is_null() {
        if !NULL_REF_WARNED.swap(true, atomic::Ordering::AcqRel) {
            warn!("StoryTimelineController::GetNextFrameCount_HighSpeed returned a null reference, leaving the path alone");
        }

        return;
    }

    let (raw_frames, raw_count) = unsafe { (*frames, *count) };

    let scaled_frames = AnimationSpeed::scale_duration(raw_frames, STORY);
    let scaled_count = AnimationSpeed::scale_frame_count(raw_count, STORY);

    if scaled_frames != raw_frames || scaled_count != raw_count {
        let seen = REF_LOGGED.fetch_add(1, atomic::Ordering::Relaxed);

        if seen < REF_LOG_LIMIT {
            debug!(
                "StoryTimelineController::GetNextFrameCount_HighSpeed frames {raw_frames} -> {scaled_frames}, count {raw_count} -> {scaled_count} (story x{})",
                AnimationSpeed::factor(STORY)
            );
        }
    }

    unsafe {
        *frames = scaled_frames;
        *count = scaled_count;
    }
}

// Installed is not the same as called, so each story stepping path keeps a plain counter that
// logs its first few calls and then one line every STEP_CHUNK calls. This is what closes the
// question of whether a path ran at all, which a scaling log cannot answer because it only
// prints when the value changed.
const STEP_DETAIL_LIMIT: usize = 6;
const STEP_CHUNK: usize = 4096;

static SKIP_FRAME_CALLS: AtomicUsize = AtomicUsize::new(0);
static SKIP_MOTION_CALLS: AtomicUsize = AtomicUsize::new(0);
static NEXT_FRAME_CALLS: AtomicUsize = AtomicUsize::new(0);
static SET_HIGH_SPEED_FRAMES_CALLS: AtomicUsize = AtomicUsize::new(0);

fn count_step(calls: &AtomicUsize, name: &'static str) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if calls <= STEP_DETAIL_LIMIT || calls % STEP_CHUNK == 0 {
        debug!("Story step {name} call {calls}");
    }
}

// Same counter for the paths whose value is known on the way in, so the log shows what the
// scaling did to it even when the option is off and the two are equal.
fn log_step(calls: &AtomicUsize, name: &'static str, raw: i32, scaled: i32) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if calls <= STEP_DETAIL_LIMIT {
        debug!("Story step {name} call {calls}: {raw} -> {scaled}");
    } else if calls % STEP_CHUNK == 0 {
        debug!("Story step {name} {calls} calls");
    }
}

// The skip paths measured 76, 182, 313, 448 and 583 frames in rising order, and the same values
// reached SkipMotionFrame, which reads like a frame target inside the timeline rather than a
// wait length. So the option only ever extends the value and never shortens it, and one step may
// add at most MAX_SKIP_EXTENSION frames: a story block is a few hundred frames long, and a
// multiplier that reaches past the block the timeline is standing in is what C23 warns about.
// Both calls get the same scaled value so the pair stays consistent.
const MAX_SKIP_SCALE: f32 = 8.0;
const MAX_SKIP_EXTENSION: i32 = 600;

static STORY_SKIP_FACTOR: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

fn extend_skip(frames: i32) -> i32 {
    let factor = f32::from_bits(STORY_SKIP_FACTOR.load(atomic::Ordering::Relaxed));

    if factor == 1.0 || frames <= 0 {
        return frames;
    }

    let wanted = (frames as f64 * factor as f64).round();
    let ceiling = frames as f64 + MAX_SKIP_EXTENSION as f64;

    (wanted.min(ceiling) as i32).max(frames)
}

type SkipFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool);
extern "C" fn SkipFrameCount(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool) {
    mark_story_activity();

    let scaled = extend_skip(frames);
    log_step(&SKIP_FRAME_CALLS, "StoryTimelineController::SkipFrameCount", frames, scaled);

    get_orig_fn!(SkipFrameCount, SkipFrameCountFn)(this, scaled, flag1, flag2);
}

type SkipMotionFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SkipMotionFrame(this: *mut Il2CppObject, frames: i32) {
    mark_story_activity();

    let scaled = extend_skip(frames);
    log_step(&SKIP_MOTION_CALLS, "StoryTimelineController::SkipMotionFrame", frames, scaled);

    get_orig_fn!(SkipMotionFrame, SkipMotionFrameFn)(this, scaled);
}

// The game holds its story high speed mode in a static field of StoryTimelineController and
// reads it back through IsHighSpeedMode. A career run reported that mode as off the whole time
// while the saved High Speed setting sat at its maximum, so the setting and the mode the
// timeline consults are separate state, and raising the setting alone does not engage it.
//
// These three helpers are the game's own door: `SetHighSpeedType/1 -> static
// void(struct<StoryTimelineController.HighSpeedType:4B>)`, `IsHighSpeedMode/1 -> static
// bool(struct<...:4B>)` and `IsHighSpeedMode/0 -> static bool()`. A 4 byte enum travels in a
// general register, the same shape HighSpeedSetting.rs already uses for SaveHighSpeedType, so no
// enum value is invented here: the value comes from StoryManager::GetMaxHighSpeedType and the
// game's own predicate decides whether that value means high speed on this client.
static mut SET_STORY_HIGH_SPEED_TYPE_ADDR: usize = 0;
impl_addr_wrapper_fn!(SetStoryHighSpeedType, SET_STORY_HIGH_SPEED_TYPE_ADDR, (), high_speed_type: i32);

static mut IS_HIGH_SPEED_MODE_VALUE_ADDR: usize = 0;
impl_addr_wrapper_fn!(IsHighSpeedModeValue, IS_HIGH_SPEED_MODE_VALUE_ADDR, i32, high_speed_type: i32);

static mut IS_HIGH_SPEED_MODE_ADDR: usize = 0;
impl_addr_wrapper_fn!(IsStoryHighSpeedMode, IS_HIGH_SPEED_MODE_ADDR, i32,);

const STORY_ACTIVE_WINDOW_SECS: i64 = 10;
const ENGAGE_INTERVAL_SECS: i64 = 10;

static HIGH_SPEED_ENABLED: AtomicBool = AtomicBool::new(false);
static HIGH_SPEED_VALUE_REJECTED: AtomicI32 = AtomicI32::new(0);

static STORY_START: OnceLock<Instant> = OnceLock::new();
static STORY_LAST_SEC: AtomicI64 = AtomicI64::new(-1);
static ENGAGE_LAST_SEC: AtomicI64 = AtomicI64::new(-1);

fn elapsed_secs() -> i64 {
    STORY_START.get_or_init(Instant::now).elapsed().as_secs() as i64
}

// Stamped by the stepping hooks, so it says that story timeline code is running right now.
fn mark_story_activity() {
    if HIGH_SPEED_ENABLED.load(atomic::Ordering::Relaxed) {
        STORY_LAST_SEC.store(elapsed_secs(), atomic::Ordering::Relaxed);
    }
}

// Config is read here, not in a detour, and the values are mirrored into atoms for the hooks.
pub fn apply_config() {
    let config = Hachimi::instance().config.load();

    HIGH_SPEED_ENABLED.store(config.story_high_speed_mode, atomic::Ordering::Relaxed);

    let scale = if config.story_skip_frame_scale.is_finite() {
        config.story_skip_frame_scale.clamp(1.0, MAX_SKIP_SCALE)
    } else {
        1.0
    };

    STORY_SKIP_FACTOR.store(scale.to_bits(), atomic::Ordering::Relaxed);
}

// Called from the game thread. It does nothing while the option is off, and only acts while the
// story stepping paths were reached inside the last window, so the static is never written from
// a menu or a race. The read of IsHighSpeedMode makes it self correcting: once the game reports
// the mode as on it writes nothing, and if the game clears the mode for the next story it asks
// again, at most once per interval.
pub fn engage_high_speed_mode() {
    if !HIGH_SPEED_ENABLED.load(atomic::Ordering::Relaxed) {
        return;
    }

    let now = elapsed_secs();
    let last_activity = STORY_LAST_SEC.load(atomic::Ordering::Relaxed);
    let last_engage = ENGAGE_LAST_SEC.load(atomic::Ordering::Relaxed);

    if last_activity < 0 || now - last_activity > STORY_ACTIVE_WINDOW_SECS {
        return;
    }

    if last_engage >= 0 && now - last_engage < ENGAGE_INTERVAL_SECS {
        return;
    }

    ENGAGE_LAST_SEC.store(now, atomic::Ordering::Relaxed);

    let target = HighSpeedSetting::GetMaxHighSpeedType();

    if target <= 0 {
        return;
    }

    let before = IsStoryHighSpeedMode();

    if before != 0 {
        return;
    }

    if IsHighSpeedModeValue(target) == 0 {
        if HIGH_SPEED_VALUE_REJECTED.swap(target, atomic::Ordering::AcqRel) != target {
            warn!("StoryTimelineController: this client does not read HighSpeedType {target} as a high speed mode, leaving the story timeline alone");
        }

        return;
    }

    SetStoryHighSpeedType(target);

    info!(
        "StoryTimelineController: asked the story timeline for high speed type {target}, IsHighSpeedMode {} -> {}",
        before,
        IsStoryHighSpeedMode()
    );
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryTimelineController);

    let GotoBlock_addr = get_method_addr(StoryTimelineController, c"GotoBlock", 4);

    new_hook!(GotoBlock_addr, GotoBlock);

    unsafe {
        GET_ISFINISHED_ADDR = get_method_addr(StoryTimelineController, c"get_IsFinished", 0);
        GET_TIMELINEDATA_ADDR = get_method_addr(StoryTimelineController, c"get_TimelineData", 0);
    }

    let by_high_speed_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "GetTimeScaleByHighSpeedType",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_R4,
    ) };
    if by_high_speed_addr != 0 { new_hook!(by_high_speed_addr, GetTimeScaleByHighSpeedType); }

    let high_speed_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "GetTimeScaleHighSpeed",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_R4,
    ) };
    if high_speed_addr != 0 { new_hook!(high_speed_addr, GetTimeScaleHighSpeed); }

    let set_frames_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "SetHighSpeedFrameCount",
        &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if set_frames_addr != 0 { new_hook!(set_frames_addr, SetHighSpeedFrameCount); }

    // Dumped as `GetNextFrameCount_HighSpeed/2 -> void(float&, int&)`, one overload, and the
    // element types stay R4 and I4 with the reference marked by the byref bit.
    let next_frames_addr = unsafe { AnimationSpeed::resolve_ref_method(
        StoryTimelineController, "GetNextFrameCount_HighSpeed",
        &[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if next_frames_addr != 0 { new_hook!(next_frames_addr, GetNextFrameCount_HighSpeed); }

    let skip_frames_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "SkipFrameCount",
        &[
            Il2CppTypeEnum_IL2CPP_TYPE_I4,
            Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
            Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
        ],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if skip_frames_addr != 0 { new_hook!(skip_frames_addr, SkipFrameCount); }

    let skip_motion_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "SkipMotionFrame",
        &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if skip_motion_addr != 0 { new_hook!(skip_motion_addr, SkipMotionFrame); }

    // The dump spells the enum parameter as `struct<StoryTimelineController.HighSpeedType:4B>`.
    // VALUETYPE is what an enum is, and CLASS is tried too so that a client spelling it the other
    // way shows up in the log instead of leaving the option quietly inert.
    for candidate in [Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Il2CppTypeEnum_IL2CPP_TYPE_CLASS] {
        let addr = unsafe { AnimationSpeed::resolve_static_method(
            StoryTimelineController, "SetHighSpeedType", &[candidate], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
        ) };

        if addr != 0 {
            unsafe { SET_STORY_HIGH_SPEED_TYPE_ADDR = addr };
            break;
        }
    }

    for candidate in [Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE, Il2CppTypeEnum_IL2CPP_TYPE_CLASS] {
        let addr = unsafe { AnimationSpeed::resolve_static_method(
            StoryTimelineController, "IsHighSpeedMode", &[candidate], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
        ) };

        if addr != 0 {
            unsafe { IS_HIGH_SPEED_MODE_VALUE_ADDR = addr };
            break;
        }
    }

    unsafe {
        IS_HIGH_SPEED_MODE_ADDR = AnimationSpeed::resolve_static_method(
            StoryTimelineController, "IsHighSpeedMode", &[], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
        );
    }

    let helpers = unsafe {
        (
            SET_STORY_HIGH_SPEED_TYPE_ADDR != 0,
            IS_HIGH_SPEED_MODE_VALUE_ADDR != 0,
            IS_HIGH_SPEED_MODE_ADDR != 0,
        )
    };

    debug!(
        "StoryTimelineController: story high speed helpers setter {}, value predicate {}, state reader {}",
        helpers.0, helpers.1, helpers.2
    );
}