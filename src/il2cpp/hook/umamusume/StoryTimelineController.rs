use std::sync::{atomic::{self, AtomicBool, AtomicI32, AtomicUsize}, Mutex};

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::AnimationSpeed,
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
}