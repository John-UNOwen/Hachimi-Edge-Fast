use std::sync::{atomic};

use crate::{core::Hachimi, il2cpp::{api::il2cpp_resolve_icall, symbols::get_method_addr, types::*}};

type SetTargetFrameRateFn = extern "C" fn(value: i32);
def_detour! {
    pub set_targetFrameRate(mut value: i32) {
            #[cfg(target_os = "windows")]
        LAST_GAME_FPS.store(value, atomic::Ordering::Relaxed);

        let hachimi = Hachimi::instance();
        let target_fps = hachimi.target_fps.load(atomic::Ordering::Relaxed);
        if target_fps != -1 {
            value = target_fps;
        }
        #[cfg(target_os = "windows")]
        {
            let unfocused_fps = hachimi.target_fps_unfocused.load(atomic::Ordering::Relaxed);
            if unfocused_fps != -1 && crate::windows::wnd_hook::window_unfocused() {
                value = unfocused_fps;
            }
        }
        // `poke_target_frame_rate` - the GUI's frame-rate knob - calls this wrapper from the
        // overlay thread, so the body runs outside its own trampoline. When the icall never
        // resolved there is nothing to aim at, and the game keeps the frame rate it chose (C1).
        let Some(set_target_frame_rate) = get_orig_fn_guarded!(set_targetFrameRate, SetTargetFrameRateFn) else {
            return;
        };

        set_target_frame_rate(value);
    }
}

#[cfg(target_os = "windows")]
static LAST_GAME_FPS: atomic::AtomicI32 = atomic::AtomicI32::new(-1);

#[cfg(target_os = "windows")]
pub fn current_effective_frame_rate() -> i32 {
    let hachimi = Hachimi::instance();
    let target_fps = hachimi.target_fps.load(atomic::Ordering::Relaxed);
    let unfocused_fps = hachimi.target_fps_unfocused.load(atomic::Ordering::Relaxed);
    if unfocused_fps != -1 && crate::windows::wnd_hook::window_unfocused() {
        unfocused_fps
    } else if target_fps != -1 {
        target_fps
    } else {
        LAST_GAME_FPS.load(atomic::Ordering::Relaxed)
    }
}

#[cfg(target_os = "windows")]
pub fn poke_target_frame_rate() {
    let value = current_effective_frame_rate();
    set_targetFrameRate(value);
}

#[cfg(target_os = "windows")]
type OpenURLFn = extern "C" fn(il2cpp_url:*mut Il2CppString);
def_detour! {
    #[cfg(target_os = "windows")]
    pub OpenURL(url: *mut Il2CppString) {
            if !crate::windows::webview::open(url){
            // Half the calls into this wrapper come from the GUI's own link buttons
            // (`core::gui.rs`, the updater's deep link), not from this hook's trampoline. With the
            // target unresolved there is no game opener to fall back to: the click does nothing,
            // rather than jumping to 0 off the overlay thread (C1).
            let Some(open_url) = get_orig_fn_guarded!(OpenURL, OpenURLFn) else {
                return;
            };

            open_url(url);
        }
    }
}

static mut GET_PERSISTENTDATAPATH_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_persistentDataPath, GET_PERSISTENTDATAPATH_ADDR, *mut Il2CppString,);

#[cfg(target_os = "android")]
static mut OPENURL_ADDR: usize = 0;
#[cfg(target_os = "android")]
impl_addr_wrapper_fn!(OpenURL, OPENURL_ADDR, (), url: *mut Il2CppString);

static mut GET_SYSTEMLANGUAGE_ADDR: usize = 0;
impl_addr_wrapper_fn!(systemLanguage, GET_SYSTEMLANGUAGE_ADDR, i32, );

pub fn init(UnityEngine_CoreModule: *const Il2CppImage) {
    get_class_or_return!(UnityEngine_CoreModule, UnityEngine, Application);

    let set_targetFrameRate_addr = il2cpp_resolve_icall(
        c"UnityEngine.Application::set_targetFrameRate(System.Int32)".as_ptr()
    );
    new_hook!(set_targetFrameRate_addr, set_targetFrameRate);

    #[cfg(target_os = "windows")]
    {
        let openurl_addr = get_method_addr(Application, c"OpenURL", 1);
        new_hook!(openurl_addr, OpenURL);
    }

    unsafe {
        GET_PERSISTENTDATAPATH_ADDR = get_method_addr(Application, c"get_persistentDataPath", 0);
        #[cfg(target_os = "android")]
        {
            OPENURL_ADDR = get_method_addr(Application, c"OpenURL", 1);
        }
        GET_SYSTEMLANGUAGE_ADDR = il2cpp_resolve_icall(c"UnityEngine.Application::get_systemLanguage()".as_ptr());
    }
}
