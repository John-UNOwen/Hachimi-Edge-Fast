use std::ffi::CStr;
use std::sync::{atomic::{self, AtomicI32}, Mutex};

use crate::{
    core::Hachimi,
    il2cpp::{
        api::{il2cpp_class_get_method_from_name, il2cpp_method_get_return_type},
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

// The story timeline advances by `deltaTime * TimeScale` and holds between blocks for
// a counted number of frames. Those two are the game's own high-speed knobs, so
// scaling them speeds the cutscene up through the game's normal path instead of
// inventing a parallel skip path.
const MAX_STORY_TIME_SCALE: f32 = 5.0;

type GetTimeScaleFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
extern "C" fn get_TimeScale(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(get_TimeScale, GetTimeScaleFn)(this);
    let factor = super::AnimationSpeed::story_factor();

    // Only the read side is scaled, so a game path that reads this back and writes it
    // through set_TimeScale would otherwise compound the factor every frame. The cap
    // bounds that even if such a path exists.
    if factor != 1.0 { (value * factor).min(MAX_STORY_TIME_SCALE).max(value) }
    else { value }
}

type GetWaitFrameCountUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitFrameCountUntilNextBlock(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitFrameCountUntilNextBlock, GetWaitFrameCountUntilNextBlockFn)(this);
    let factor = super::AnimationSpeed::story_factor();

    // Keep at least one frame: the block-advance coroutine polls this to decide when
    // to move on, and a zero wait can leave it spinning without ever advancing.
    if factor != 1.0 && value > 0 { ((value as f32 / factor).max(1.0)).round() as i32 }
    else { value }
}

// A property getter hooked with the wrong return type hands garbage to every caller,
// so the declared type has to match the wrapper before the detour is installed.
unsafe fn get_method_checked(
    class: *mut Il2CppClass,
    name: &CStr,
    argc: ::std::os::raw::c_int,
    expected: Il2CppTypeEnum,
) -> usize {
    let method = il2cpp_class_get_method_from_name(class, name.as_ptr(), argc);

    if method.is_null() || (*method).is_generic() != 0 {
        return 0;
    }

    let return_type = il2cpp_method_get_return_type(method);

    if return_type.is_null() {
        return 0;
    }

    let actual = (*return_type).type_();

    if actual != expected {
        warn!(
            "StoryTimelineController::{} returns il2cpp type {}, wrapper expects {}",
            name.to_string_lossy(),
            actual,
            expected
        );
        return 0;
    }

    (*method).methodPointer
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryTimelineController);

    let GotoBlock_addr = get_method_addr(StoryTimelineController, c"GotoBlock", 4);

    new_hook!(GotoBlock_addr, GotoBlock);

    unsafe {
        GET_ISFINISHED_ADDR = get_method_addr(StoryTimelineController, c"get_IsFinished", 0);
        GET_TIMELINEDATA_ADDR = get_method_addr(StoryTimelineController, c"get_TimelineData", 0);
    }

    let get_TimeScale_addr =
        unsafe { get_method_checked(StoryTimelineController, c"get_TimeScale", 0, Il2CppTypeEnum_IL2CPP_TYPE_R4) };
    if get_TimeScale_addr != 0 {
        new_hook!(get_TimeScale_addr, get_TimeScale);
    }

    let get_WaitFrameCountUntilNextBlock_addr = unsafe {
        get_method_checked(
            StoryTimelineController,
            c"get_WaitFrameCountUntilNextBlock",
            0,
            Il2CppTypeEnum_IL2CPP_TYPE_I4,
        )
    };
    if get_WaitFrameCountUntilNextBlock_addr != 0 {
        new_hook!(get_WaitFrameCountUntilNextBlock_addr, get_WaitFrameCountUntilNextBlock);
    }
}