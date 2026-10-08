use std::sync::{atomic::{self, AtomicBool, AtomicI32, AtomicI64, AtomicUsize}, Mutex};
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
//
// Both are dumped as static: `GetTimeScaleByHighSpeedType/1 -> static float(bool)` and
// `GetTimeScaleHighSpeed/1 -> static float(bool)`. A static carries no hidden `this`, so the
// wrapper takes only the bool; a wrapper that declared an instance pointer would read the flag
// from the wrong register, which is why the matcher refused them for a whole ledger cycle (A3).
// They are installed as measurement first: the timeline speed they produce is the biggest story
// lever this class has, and a run has to report what the game returns for each flag before a
// factor is put on it. Run 5 is the reason for that order.
const STORY: AnimationSpeed::Group = AnimationSpeed::Group::Story;

static HIGH_SPEED_SCALE_CALLS: AtomicUsize = AtomicUsize::new(0);
static PLAIN_SCALE_CALLS: AtomicUsize = AtomicUsize::new(0);

fn log_scale(calls: &AtomicUsize, name: &'static str, flag: bool, value: f32) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;
    let flag = u8::from(flag);

    if calls <= STEP_DETAIL_LIMIT {
        debug!("Story scale {name}({flag}) call {calls} -> {value}");
    } else if calls % STEP_CHUNK == 0 {
        debug!("Story scale {name} {calls} calls, most recent value {value}");
    }
}

type GetTimeScaleByHighSpeedTypeFn = extern "C" fn(is_high_speed: bool) -> f32;
extern "C" fn GetTimeScaleByHighSpeedType(is_high_speed: bool) -> f32 {
    let value = get_orig_fn!(GetTimeScaleByHighSpeedType, GetTimeScaleByHighSpeedTypeFn)(is_high_speed);
    log_scale(&HIGH_SPEED_SCALE_CALLS, "StoryTimelineController::GetTimeScaleByHighSpeedType", is_high_speed, value);

    value
}

type GetTimeScaleHighSpeedFn = extern "C" fn(is_high_speed: bool) -> f32;
extern "C" fn GetTimeScaleHighSpeed(is_high_speed: bool) -> f32 {
    let value = get_orig_fn!(GetTimeScaleHighSpeed, GetTimeScaleHighSpeedFn)(is_high_speed);
    log_scale(&PLAIN_SCALE_CALLS, "StoryTimelineController::GetTimeScaleHighSpeed", is_high_speed, value);

    value
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

// Same counter for the paths whose value is known on the way in.
fn log_step(calls: &AtomicUsize, name: &'static str, value: i32) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if calls <= STEP_DETAIL_LIMIT {
        debug!("Story step {name} call {calls}: {value}");
    } else if calls % STEP_CHUNK == 0 {
        debug!("Story step {name} {calls} calls");
    }
}

// These two paths are hooked to measure them, not to change them. A run that extended the frame
// value they carry froze the story timeline: SkipFrameCount(133) went on as 733, the game handed
// that 733 to SkipMotionFrame, and this hook raised it again to 1333. The value one call receives
// is fed into the next, so scaling either one compounds along the chain instead of landing once.
// The wait count then stopped moving at 96, UpdateTimeScaleByHispeedType stopped at 49, and
// IsHighSpeedMode ran at a flat 3600 calls per 20 s while nothing advanced. The measured values
// (133, 243, 161, 263, 691, 163) read like frame targets inside the timeline rather than wait
// lengths, so there is no multiplier here that is safe. Every argument is passed through untouched.
type SkipFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool);
extern "C" fn SkipFrameCount(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool) {
    mark_story_activity();
    log_step(&SKIP_FRAME_CALLS, "StoryTimelineController::SkipFrameCount", frames);

    get_orig_fn!(SkipFrameCount, SkipFrameCountFn)(this, frames, flag1, flag2);
}

type SkipMotionFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SkipMotionFrame(this: *mut Il2CppObject, frames: i32) {
    mark_story_activity();
    log_step(&SKIP_MOTION_CALLS, "StoryTimelineController::SkipMotionFrame", frames);

    get_orig_fn!(SkipMotionFrame, SkipMotionFrameFn)(this, frames);
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

// The stepping paths fire in bursts when the game jumps between blocks, not on a steady cadence:
// one run stepped them at 47 s into a scene and its next game thread report was 98 s later. A
// window measured in seconds therefore reads as "no story running" in the middle of a scene. The
// window is generous so a live scene is never missed; a menu or a race never stamps it at all,
// because nothing there reaches these methods.
const STORY_ACTIVE_WINDOW_SECS: i64 = 300;
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

// Config is read here, not in a detour, so a change lands on the game thread and the hooks only
// read an atom. The line is the proof that the option reached this layer: a run had the option on
// and produced no engagement line at all, and without this there is no way to tell whether the
// option was off or the mirror never ran.
pub fn apply_config() {
    let config = Hachimi::instance().config.load();
    let enabled = config.story_high_speed_mode;

    if HIGH_SPEED_ENABLED.swap(enabled, atomic::Ordering::AcqRel) != enabled {
        info!("StoryTimelineController: story high speed mode option {}", if enabled { "on" } else { "off" });
    }
}

// Each reason an attempt stops gets its own slot and speaks once per session. A shared budget of
// six lines was spent on the menu phase of a run before story content was reached, so the exit
// that mattered printed nothing and the run could not be read.
const REASON_WINDOW: usize = 1 << 0;
const REASON_NO_MAX: usize = 1 << 1;
const REASON_ALREADY_ON: usize = 1 << 2;
const REASON_ATTEMPT: usize = 1 << 3;

static ENGAGE_REASONS: AtomicUsize = AtomicUsize::new(0);

fn note_engage(bit: usize, message: &str) {
    if ENGAGE_REASONS.fetch_or(bit, atomic::Ordering::AcqRel) & bit == 0 {
        debug!("StoryTimelineController: {message}");
    }
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

    // No story frame has stepped in this session yet, which is the menu and loading screen case
    // where the static must not be written. Silence here is the correct behaviour.
    if last_activity < 0 {
        return;
    }

    if now - last_activity > STORY_ACTIVE_WINDOW_SECS {
        note_engage(REASON_WINDOW, "story high speed mode left alone, the story stepping paths have been quiet since the last scene");
        return;
    }

    if last_engage >= 0 && now - last_engage < ENGAGE_INTERVAL_SECS {
        return;
    }

    ENGAGE_LAST_SEC.store(now, atomic::Ordering::Relaxed);

    let target = HighSpeedSetting::GetMaxHighSpeedType();

    if target <= 0 {
        note_engage(REASON_NO_MAX, "story high speed mode left alone, StoryManager reported no high speed type in this context");
        return;
    }

    let before = IsStoryHighSpeedMode();
    let accepted = IsHighSpeedModeValue(target);

    // The one line that carries the three numbers the decision is made from. Without it a run that
    // writes nothing cannot be told apart from a run that never looked.
    if ENGAGE_REASONS.load(atomic::Ordering::Relaxed) & REASON_ATTEMPT == 0 {
        note_engage(REASON_ATTEMPT, &format!(
            "story high speed attempt, StoryManager max {target}, IsHighSpeedMode {before}, IsHighSpeedMode({target}) {}",
            u8::from(accepted != 0)
        ));
    }

    if before != 0 {
        note_engage(REASON_ALREADY_ON, "story high speed mode left alone, the game already reports the mode as on");
        return;
    }

    if accepted == 0 {
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

    let by_high_speed_addr = unsafe { AnimationSpeed::resolve_static_method(
        StoryTimelineController, "GetTimeScaleByHighSpeedType",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_R4,
    ) };
    if by_high_speed_addr != 0 { new_hook!(by_high_speed_addr, GetTimeScaleByHighSpeedType); }

    let high_speed_addr = unsafe { AnimationSpeed::resolve_static_method(
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