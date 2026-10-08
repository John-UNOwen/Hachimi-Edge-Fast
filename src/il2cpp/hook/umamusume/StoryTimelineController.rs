use std::sync::{atomic::{self, AtomicI32}, Mutex};

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
}