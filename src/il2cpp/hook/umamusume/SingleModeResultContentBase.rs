// Result screens (training turn summary, race result, grand result) build their
// content as a chain of DOTween fades and count-ups, and the game only enables the
// Skip button once the chain has started. `SingleModeResultContentBase` exposes the
// game's own way out of that chain: `SkipFadeInTween()`, the same call the Skip button
// handler makes.
//
// With `auto_skip_result_screens` enabled we let the game enable the button as usual
// and then immediately finish the tween chain on that instance, so the player sees the
// settled content instead of the cascade. Only zero-argument methods are used here, so
// nothing depends on guessing a parameter layout.
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::Hachimi;
use crate::il2cpp::{
    api::{il2cpp_class_get_method_from_name, il2cpp_class_get_parent, il2cpp_object_get_class},
    symbols::get_method_addr,
    types::*,
};

static mut SKIP_FADE_IN_TWEEN_ADDR: usize = 0;

// SkipFadeInTween can end up back in ActivateSkipButton through the view's own
// completion callback; without this guard the two would ping-pong.
static IN_SKIP: AtomicBool = AtomicBool::new(false);

struct SkipGuard;

impl SkipGuard {
    // Taken only when the flag was free, and cleared on every exit path, including a
    // panic inside the game call.
    fn try_enter() -> Option<Self> {
        if IN_SKIP.swap(true, Ordering::AcqRel) { None }
        else { Some(SkipGuard) }
    }
}

impl Drop for SkipGuard {
    fn drop(&mut self) {
        IN_SKIP.store(false, Ordering::Release);
    }
}

type ActivateSkipButtonFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn ActivateSkipButton(this: *mut Il2CppObject) {
    get_orig_fn!(ActivateSkipButton, ActivateSkipButtonFn)(this);

    if !Hachimi::instance().config.load().auto_skip_result_screens {
        return;
    }

    let addr = match skip_fade_in_tween_addr(this) {
        Some(addr) => addr,
        None => return,
    };

    let _guard = match SkipGuard::try_enter() {
        Some(guard) => guard,
        None => return,
    };

    unsafe {
        let skip_fn: extern "C" fn(this: *mut Il2CppObject) = std::mem::transmute(addr);
        skip_fn(this);
    }
}

// Prefer the method on the concrete runtime class (a subclass may override it),
// falling back to whatever the base class declared at init time.
fn skip_fade_in_tween_addr(this: *mut Il2CppObject) -> Option<usize> {
    if this.is_null() {
        return None;
    }

    unsafe {
        let mut class = il2cpp_object_get_class(this);

        while !class.is_null() {
            let method = il2cpp_class_get_method_from_name(class, c"SkipFadeInTween".as_ptr(), 0);

            if !method.is_null() && (*method).methodPointer != 0 && (*method).is_generic() == 0 {
                return Some((*method).methodPointer);
            }

            class = il2cpp_class_get_parent(class);
        }

        if SKIP_FADE_IN_TWEEN_ADDR != 0 {
            return Some(SKIP_FADE_IN_TWEEN_ADDR);
        }
    }

    debug!("auto_skip_result_screens: SkipFadeInTween unavailable for this result part");

    None
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, SingleModeResultContentBase);

    unsafe {
        SKIP_FADE_IN_TWEEN_ADDR = get_method_addr(SingleModeResultContentBase, c"SkipFadeInTween", 0);
    }

    let ActivateSkipButton_addr = get_method_addr(SingleModeResultContentBase, c"ActivateSkipButton", 0);
    new_hook!(ActivateSkipButton_addr, ActivateSkipButton);
}
