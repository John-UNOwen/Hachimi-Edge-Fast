use crate::il2cpp::{symbols::{get_method_addr, IEnumerator}, types::*};

pub mod ScreenOrientationClassWrapper;

#[cfg(target_os = "android")]
use crate::core::Hachimi;

#[cfg(target_os = "windows")]
use std::ptr::null_mut;

#[cfg(target_os = "windows")]
use crate::{
    core::{Hachimi, game::Region},
    il2cpp::{
        api::il2cpp_field_static_set_value,
        hook::UnityEngine_CoreModule::Screen as UnityScreen,
        symbols::{MoveNextFn, get_field_from_name},
    },
};

def_detour! {
    #[cfg(target_os = "android")]
    ChangeScreenOrientationLandscapeAsync_MoveNext(
    enumerator: *mut Il2CppObject,
) coroutine answer -> bool {
            use crate::il2cpp::symbols::MoveNextFn;
        let moved =
            get_orig_fn!(ChangeScreenOrientationLandscapeAsync_MoveNext, MoveNextFn)(enumerator);
        answer.publish(moved);
        if !moved {
            super::UIManager::apply_ui_scale();
        }
        moved
    }
}

def_detour! {
    #[cfg(target_os = "android")]
    ChangeScreenOrientationPortraitAsync_MoveNext(enumerator: *mut Il2CppObject) coroutine answer -> bool {
            use crate::il2cpp::symbols::MoveNextFn;
        let moved = get_orig_fn!(ChangeScreenOrientationPortraitAsync_MoveNext, MoveNextFn)(enumerator);
        answer.publish(moved);
        if !moved {
            super::UIManager::apply_ui_scale();
        }
        moved
    }
}

#[cfg(target_os = "android")]
type ChangeScreenOrientationLandscapeAsyncFn =
    extern "C" fn() -> crate::il2cpp::symbols::IEnumerator;
def_detour! {
    #[cfg(target_os = "android")]
    ChangeScreenOrientationLandscapeAsync() answer -> crate::il2cpp::symbols::IEnumerator {
            let enumerator = get_orig_fn!(
            ChangeScreenOrientationLandscapeAsync,
            ChangeScreenOrientationLandscapeAsyncFn
        )();
        answer.publish(IEnumerator::from(enumerator.this));
        if Hachimi::instance().config.load().ui_scale == 1.0 {
            return enumerator;
        }

        if let Err(e) = enumerator.hook_move_next(ChangeScreenOrientationLandscapeAsync_MoveNext) {
            error!("Failed to hook enumerator: {}", e);
        }

        enumerator
    }
}

#[cfg(target_os = "android")]
type ChangeScreenOrientationPortraitAsyncFn =
    extern "C" fn() -> crate::il2cpp::symbols::IEnumerator;
def_detour! {
    #[cfg(target_os = "android")]
    ChangeScreenOrientationPortraitAsync() answer -> crate::il2cpp::symbols::IEnumerator {
            let enumerator = get_orig_fn!(
            ChangeScreenOrientationPortraitAsync,
            ChangeScreenOrientationPortraitAsyncFn
        )();
        answer.publish(IEnumerator::from(enumerator.this));
        if Hachimi::instance().config.load().ui_scale == 1.0 {
            return enumerator;
        }

        if let Err(e) = enumerator.hook_move_next(ChangeScreenOrientationPortraitAsync_MoveNext) {
            error!("Failed to hook enumerator: {}", e);
        }

        enumerator
    }
}

#[cfg(target_os = "windows")]
type GetWidthFn = extern "C" fn() -> i32;
def_detour! {
    #[cfg(target_os = "windows")]
    get_Width() -> i32 {
            if Hachimi::instance().config.load().windows.freeform_window {
            return UnityScreen::get_width();
        }

        if let Some((width, _)) = crate::windows::utils::get_scaling_res() {
            return width;
        }

        get_orig_fn!(get_Width, GetWidthFn)()
    }
    // Not a `bail`: this is not the call the game would have got, it is the value this wrapper says
    // it answers with when it cannot answer with the game's, and a `Faulted` may have it because
    // nothing in it replays the state that just faulted. The trip it is written for is the C1 chain:
    // `get_orig_fn!` answering 0 for a trampoline the detach path took back, the line above calling
    // through 0, the barrier taking the access violation. Answering that with the zero value - which
    // is what a value arm used to do whatever the wrapper stated - hands the game and
    // `windows/utils.rs` a divisor of 0. This is the same answer `get_Width_orig` already falls back
    // to for the same reason.
    fallback {
                UnityScreen::get_width()
    }
}

#[cfg(target_os = "windows")]
pub fn get_Width_orig() -> i32 {
    // `windows/utils.rs` calls this on the resolution-scaling path, and the result is a divisor
    // there, so 0 is not an answer that can be passed on. These two `get_Method_addr` lookups sit
    // on a game class (`Gallop.Screen`), which is exactly where a run writes `_addr is null` - the
    // Global log has `umamusume::Screen: SetResolution_addr is null` - so fall back to what Unity
    // itself reports, the same guarded call the detour above uses.
    let Some(get_width) = get_orig_fn_guarded!(get_Width, GetWidthFn) else {
        return UnityScreen::get_width();
    };

    get_width()
}

#[cfg(target_os = "windows")]
type GetHeightFn = extern "C" fn() -> i32;
def_detour! {
    #[cfg(target_os = "windows")]
    get_Height() -> i32 {
            if Hachimi::instance().config.load().windows.freeform_window && Hachimi::instance().game.region != Region::Global {
            return UnityScreen::get_height();
        }

        if let Some((_, height)) = crate::windows::utils::get_scaling_res() {
            return height;
        }

        get_orig_fn!(get_Height, GetHeightFn)()
    }
    // As above: the height is a divisor too, and `get_Height_orig` already treats Unity's answer as
    // the one that can be passed on. A `Faulted` takes it for the same reason `get_Width` does - the
    // body's own `get_orig_fn!` call is the thing that faults, so the answer cannot be a replay of it.
    fallback {
                UnityScreen::get_height()
    }
}

#[cfg(target_os = "windows")]
pub fn get_Height_orig() -> i32 {
    // Same call site as `get_Width_orig` above: a divisor in `windows/utils.rs`, never a jump.
    let Some(get_height) = get_orig_fn_guarded!(get_Height, GetHeightFn) else {
        return UnityScreen::get_height();
    };

    get_height()
}

#[cfg(target_os = "windows")]
static mut ORIGINAL_SCREEN_WIDTH_FIELD: *mut FieldInfo = null_mut();
#[cfg(target_os = "windows")]
static mut ORIGINAL_SCREEN_HEIGHT_FIELD: *mut FieldInfo = null_mut();

#[cfg(target_os = "windows")]
fn set_static_field<T>(field: *mut FieldInfo, value: T) {
    if field.is_null() {
        return;
    }

    il2cpp_field_static_set_value(field, std::ptr::from_ref(&value) as _);
}

#[cfg(target_os = "windows")]
pub fn update_original_screen_size(width: i32, height: i32) {
    let is_portrait = width < height;
    unsafe {
        set_static_field(
            ORIGINAL_SCREEN_WIDTH_FIELD,
            if is_portrait { height } else { width },
        );
        set_static_field(
            ORIGINAL_SCREEN_HEIGHT_FIELD,
            if is_portrait { width } else { height },
        );
    }
}

#[cfg(target_os = "windows")]
type SetResolutionFn = extern "C" fn(
    width: i32,
    height: i32,
    fullscreen: bool,
    force_update: bool,
    skip_keep_aspect: bool,
);
def_detour! {
    #[cfg(target_os = "windows")]
    SetResolution(
    width: i32,
    height: i32,
    fullscreen: bool,
    force_update: bool,
    skip_keep_aspect: bool,
) {
            if !Hachimi::instance().config.load().windows.freeform_window || Hachimi::instance().game.region == Region::Global {
            get_orig_fn!(SetResolution, SetResolutionFn)(
                width,
                height,
                fullscreen,
                force_update,
                skip_keep_aspect,
            );
        }
    }
}

#[cfg(target_os = "windows")]
type IsCurrentOrientationFn = extern "C" fn(target: ScreenOrientation) -> bool;
def_detour! {
    #[cfg(target_os = "windows")]
    IsCurrentOrientation(target: ScreenOrientation) -> bool {
            if Hachimi::instance().config.load().windows.freeform_window && Hachimi::instance().game.region != Region::Global {
            return true;
        }

        get_orig_fn!(IsCurrentOrientation, IsCurrentOrientationFn)(target)
    }
}

#[cfg(target_os = "windows")]
type WaitDeviceOrientationFn = extern "C" fn(target: ScreenOrientation) -> IEnumerator;
def_detour! {
    #[cfg(target_os = "windows")]
    WaitDeviceOrientation(target: ScreenOrientation) answer -> IEnumerator {
            let enumerator = get_orig_fn!(WaitDeviceOrientation, WaitDeviceOrientationFn)(target);
        answer.publish(IEnumerator::from(enumerator.this));
        if Hachimi::instance().config.load().windows.freeform_window && Hachimi::instance().game.region != Region::Global {
            if let Err(e) = enumerator.hook_move_next(WaitDeviceOrientation_MoveNext) {
                error!("Failed to stop WaitDeviceOrientation: {}", e);
            }
        }
        enumerator
    }
}

def_detour! {
    #[cfg(target_os = "windows")]
    WaitDeviceOrientation_MoveNext(_enumerator: *mut Il2CppObject) coroutine _answer -> bool {
            if crate::windows::wnd_hook::close_freeform_window_for_landscape() {
            return get_orig_fn!(WaitDeviceOrientation_MoveNext, MoveNextFn)(_enumerator);
        }

        if Hachimi::instance().config.load().windows.freeform_window {
            // Holding this coroutine open is the wrapper's own decision, and on a clean call it
            // stands. Nothing here is an answer the game gave, so a trip is answered by the door
            // rule instead: the coroutine is left to the game's own `MoveNext`.
            return false;
        }

        get_orig_fn!(WaitDeviceOrientation_MoveNext, MoveNextFn)(_enumerator)
    }
}

type ChangeScreenOrientationFn = extern "C" fn(target: ScreenOrientation, force: bool) -> IEnumerator;
def_detour! {
    ChangeScreenOrientation(target: ScreenOrientation, force: bool) answer -> IEnumerator {
            #[cfg(target_os = "windows")]
        {
            let enumerator = get_orig_fn!(ChangeScreenOrientation, ChangeScreenOrientationFn)(target, force);
            answer.publish(IEnumerator::from(enumerator.this));
            if Hachimi::instance().config.load().windows.freeform_window && Hachimi::instance().game.region != Region::Global {
                if let Err(e) = enumerator.hook_move_next(ChangeScreenOrientation_MoveNext) {
                    error!("Failed to stop ChangeScreenOrientation: {}", e);
                }
            }
            enumerator
        }
        #[cfg(target_os = "android")]
        {
            if should_force_orientation() {
                let force_orientation = Hachimi::instance().config.load().android.force_orientation_mode;
                let swapped = get_orig_fn!(ChangeScreenOrientation, ChangeScreenOrientationFn)(force_orientation, true);
                answer.publish(IEnumerator::from(swapped.this));
                return swapped;
            }

            let enumerator = get_orig_fn!(ChangeScreenOrientation, ChangeScreenOrientationFn)(target, force);
            answer.publish(IEnumerator::from(enumerator.this));
            enumerator
        }
    }
}

#[cfg(target_os = "android")]
pub fn should_force_orientation() -> bool {
    let config = Hachimi::instance().config.load();
    let force_orientation = config.android.force_orientation_mode;
    if force_orientation <= ScreenOrientation_Unknown || force_orientation >= 6 {
        return false;
    }
    true
}

def_detour! {
    #[cfg(target_os = "windows")]
    ChangeScreenOrientation_MoveNext(_enumerator: *mut Il2CppObject) coroutine _answer -> bool {
            if crate::windows::wnd_hook::close_freeform_window_for_landscape() {
            return get_orig_fn!(ChangeScreenOrientation_MoveNext, MoveNextFn)(_enumerator);
        }

        if Hachimi::instance().config.load().windows.freeform_window {
            // Not the game's answer, so nothing is published and a trip goes to the door rule.
            return false;
        }

        get_orig_fn!(ChangeScreenOrientation_MoveNext, MoveNextFn)(_enumerator)
    }
}

#[cfg(target_os = "windows")]
type ChangeScreenOrientationAsyncFn = extern "C" fn() -> IEnumerator;
def_detour! {
    #[cfg(target_os = "windows")]
    ChangeScreenOrientationLandscapeAsyncWindows() answer -> IEnumerator {
            let enumerator = get_orig_fn!(
            ChangeScreenOrientationLandscapeAsyncWindows,
            ChangeScreenOrientationAsyncFn
        )();
        answer.publish(IEnumerator::from(enumerator.this));
        if Hachimi::instance().config.load().windows.freeform_window && Hachimi::instance().game.region != Region::Global {
            if let Err(e) =
                enumerator.hook_move_next(ChangeScreenOrientationLandscapeAsyncWindows_MoveNext)
            {
                error!("Failed to stop landscape orientation change: {}", e);
            }
        }
        enumerator
    }
}

def_detour! {
    #[cfg(target_os = "windows")]
    ChangeScreenOrientationLandscapeAsyncWindows_MoveNext(
    _enumerator: *mut Il2CppObject,
) coroutine _answer -> bool {
            if crate::windows::wnd_hook::close_freeform_window_for_landscape() {
            return get_orig_fn!(
                ChangeScreenOrientationLandscapeAsyncWindows_MoveNext,
                MoveNextFn
            )(_enumerator);
        }

        if Hachimi::instance().config.load().windows.freeform_window {
            // Not the game's answer, so nothing is published and a trip goes to the door rule.
            return false;
        }

        get_orig_fn!(
            ChangeScreenOrientationLandscapeAsyncWindows_MoveNext,
            MoveNextFn
        )(_enumerator)
    }
}

def_detour! {
    #[cfg(target_os = "windows")]
    ChangeScreenOrientationPortraitAsyncWindows() answer -> IEnumerator {
            let enumerator = get_orig_fn!(
            ChangeScreenOrientationPortraitAsyncWindows,
            ChangeScreenOrientationAsyncFn
        )();
        answer.publish(IEnumerator::from(enumerator.this));
        if Hachimi::instance().config.load().windows.freeform_window && Hachimi::instance().game.region != Region::Global {
            if let Err(e) =
                enumerator.hook_move_next(ChangeScreenOrientationPortraitAsyncWindows_MoveNext)
            {
                error!("Failed to stop portrait orientation change: {}", e);
            }
        }
        enumerator
    }
}

def_detour! {
    #[cfg(target_os = "windows")]
    ChangeScreenOrientationPortraitAsyncWindows_MoveNext(
    _enumerator: *mut Il2CppObject,
) coroutine _answer -> bool {
            if Hachimi::instance().config.load().windows.freeform_window {
            // Not the game's answer, so nothing is published and a trip goes to the door rule.
            return false;
        }

        get_orig_fn!(
            ChangeScreenOrientationPortraitAsyncWindows_MoveNext,
            MoveNextFn
        )(_enumerator)
    }
}

#[cfg(target_os = "windows")]
static mut GET_ISSPLITWINDOW_ADDR: usize = 0;
#[cfg(target_os = "windows")]
impl_addr_wrapper_fn!(get_IsSplitWindow, GET_ISSPLITWINDOW_ADDR, bool,);

#[cfg(target_os = "windows")]
static mut GET_ISLANDSCAPE_MODE_ADDR: usize = 0;
#[cfg(target_os = "windows")]
impl_addr_wrapper_fn!(get_IsLandscapeMode, GET_ISLANDSCAPE_MODE_ADDR, bool,);

static mut GET_ISVERTICAL_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_IsVertical, GET_ISVERTICAL_ADDR, bool,);

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, Screen);

    ScreenOrientationClassWrapper::init(Screen);

    let ChangeScreenOrientation_addr = get_method_addr(Screen, c"ChangeScreenOrientation", 2);
    new_hook!(ChangeScreenOrientation_addr, ChangeScreenOrientation);

    #[cfg(target_os = "android")]
    {
        let ChangeScreenOrientationLandscapeAsync_addr = get_method_addr(Screen, c"ChangeScreenOrientationLandscapeAsync", 0);
        new_hook!(
            ChangeScreenOrientationLandscapeAsync_addr,
            ChangeScreenOrientationLandscapeAsync
        );

        let ChangeScreenOrientationPortraitAsync_addr = get_method_addr(Screen, c"ChangeScreenOrientationPortraitAsync", 0);
        new_hook!(
            ChangeScreenOrientationPortraitAsync_addr,
            ChangeScreenOrientationPortraitAsync
        );
    }

    #[cfg(target_os = "windows")]
    {
        let get_Width_addr = get_method_addr(Screen, c"get_Width", 0);
        new_hook!(get_Width_addr, get_Width);
 
        let get_Height_addr = get_method_addr(Screen, c"get_Height", 0);
        new_hook!(get_Height_addr, get_Height);

        let SetResolution_addr = get_method_addr(Screen, c"SetResolution", 5);
        new_hook!(SetResolution_addr, SetResolution);

        let IsCurrentOrientation_addr = get_method_addr(Screen, c"IsCurrentOrientation", 1);
        new_hook!(IsCurrentOrientation_addr, IsCurrentOrientation);

        let WaitDeviceOrientation_addr = get_method_addr(Screen, c"WaitDeviceOrientation", 1);
        new_hook!(WaitDeviceOrientation_addr, WaitDeviceOrientation);

        let ChangeScreenOrientationLandscapeAsyncWindows_addr = get_method_addr(Screen, c"ChangeScreenOrientationLandscapeAsync", 0);
        new_hook!(
            ChangeScreenOrientationLandscapeAsyncWindows_addr,
            ChangeScreenOrientationLandscapeAsyncWindows
        );

        let ChangeScreenOrientationPortraitAsyncWindows_addr = get_method_addr(Screen, c"ChangeScreenOrientationPortraitAsync", 0);
        new_hook!(
            ChangeScreenOrientationPortraitAsyncWindows_addr,
            ChangeScreenOrientationPortraitAsyncWindows
        );

        unsafe {
            GET_ISLANDSCAPE_MODE_ADDR = get_method_addr(Screen, c"get_IsLandscapeMode", 0);
            GET_ISSPLITWINDOW_ADDR = get_method_addr(Screen, c"get_IsSplitWindow", 0);
            ORIGINAL_SCREEN_WIDTH_FIELD = get_field_from_name(Screen, c"_originalScreenWidth");
            ORIGINAL_SCREEN_HEIGHT_FIELD = get_field_from_name(Screen, c"_originalScreenHeight");
        }
    }

    unsafe {
        GET_ISVERTICAL_ADDR = get_method_addr(Screen, c"get_IsVertical", 0);
    }
}
