use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::Screen::ScreenOrientationClassWrapper,
        symbols::{get_method_addr, get_field_from_name, IEnumerator, MoveNextFn},
        types::*
    }
};

static mut CLASS: *mut Il2CppClass = 0 as _;
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

static mut PAUSELIVE_ADDR: usize = 0;
impl_addr_wrapper_fn!(PauseLive, PAUSELIVE_ADDR, (), this: *mut Il2CppObject);

static mut RESUMELIVE_ADDR: usize = 0;
impl_addr_wrapper_fn!(ResumeLive, RESUMELIVE_ADDR, (), this: *mut Il2CppObject);

static mut SKIPLIVE_ADDR: usize = 0;
impl_addr_wrapper_fn!(SkipLive, SKIPLIVE_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);

static mut GETVIEWBASE_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetViewBase, GETVIEWBASE_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);

def_field_value_accessors!(get__state, set__state, _STATE_FIELD, i32);

static mut SOCW_RET_ORIENTATION: *mut Il2CppObject = std::ptr::null_mut();

type GetChangeViewOrientationFn = extern "C" fn(this: *mut Il2CppObject, retOrientation: *mut Il2CppObject) -> IEnumerator;
def_detour! {
    GetChangeViewOrientation(this: *mut Il2CppObject, retOrientation: *mut Il2CppObject) answer -> IEnumerator {
            let enumerator = get_orig_fn!(GetChangeViewOrientation, GetChangeViewOrientationFn)(this, retOrientation);

        // Published before the mod's own work: arming the door below is a lock and a map write, and
        // a trip in it still owes the game the enumerator its method produced - a zeroed one is a
        // coroutine that never existed.
        answer.publish(IEnumerator::from(enumerator.this));

        unsafe { SOCW_RET_ORIENTATION = retOrientation; }
        if let Err(e) = enumerator.hook_move_next(GetChangeViewOrientation_MoveNext) {
            error!("Failed to hook GetChangeViewOrientation MoveNext: {}", e);
        }

        enumerator
    }
}

def_detour! {
    GetChangeViewOrientation_MoveNext(enumerator: *mut Il2CppObject) coroutine answer -> bool {
            let moved = get_orig_fn!(GetChangeViewOrientation_MoveNext, MoveNextFn)(enumerator);
        // The orientation half is the mod's own work, and it is the half that reaches for a game
        // object this coroutine may already have released. The game's answer is published first so
        // that a trip in that half is answered with the step the coroutine actually took.
        answer.publish(moved);
        if !moved { // hasn't moved = enumerator just finished
            unsafe {
                let wrapper = SOCW_RET_ORIENTATION;
                if !wrapper.is_null() && Hachimi::instance().config.load().trainer_live_landscape {
                    let orientation = ScreenOrientationClassWrapper::get_Orientation(wrapper);
                    if orientation == ScreenOrientation_Portrait {
                        ScreenOrientationClassWrapper::set_Orientation(wrapper, ScreenOrientation_LandscapeLeft);
                        SOCW_RET_ORIENTATION = std::ptr::null_mut();
                    }
                }
            }
        }
        moved
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, LiveViewController);

    let get_change_view_orientation_addr = get_method_addr(LiveViewController, c"GetChangeViewOrientation", 1);
    new_hook!(get_change_view_orientation_addr, GetChangeViewOrientation);

    unsafe {
        CLASS = LiveViewController;
        PAUSELIVE_ADDR = get_method_addr(LiveViewController, c"PauseLive", 0);
        RESUMELIVE_ADDR = get_method_addr(LiveViewController, c"ResumeLive", 0);
        SKIPLIVE_ADDR = get_method_addr(LiveViewController, c"SkipLive", 0);
        GETVIEWBASE_ADDR = get_method_addr(LiveViewController, c"GetViewBase", 0);
        _STATE_FIELD = get_field_from_name(LiveViewController, c"_state");
    }
}
