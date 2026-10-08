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
    hook::umamusume::AnimationSpeed,
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

const SCREENS: AnimationSpeed::Group = AnimationSpeed::Group::Screens;

// The result parts hand their hardcoded FADE_DURATION / FADE_OFFSET / COUNTUP_DURATION
// to these three as float arguments. Same reason as NowLoading above: the constants
// themselves are `const`, so the argument is where they can still be reached.
type FadeInContentFn = extern "C" fn(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32, offset: *mut Il2CppObject, onComplete: *mut Il2CppObject);
extern "C" fn FadeInContent(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32, offset: *mut Il2CppObject, onComplete: *mut Il2CppObject) {
    log_duration("FadeInContent", duration);

    get_orig_fn!(FadeInContent, FadeInContentFn)(
        this, content, AnimationSpeed::scale_duration(duration, SCREENS), offset, onComplete
    );
}

type FadeInContentFromRightFn = extern "C" fn(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32, onComplete: *mut Il2CppObject);
extern "C" fn FadeInContentFromRight(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32, onComplete: *mut Il2CppObject) {
    log_duration("FadeInContentFromRight", duration);

    get_orig_fn!(FadeInContentFromRight, FadeInContentFromRightFn)(
        this, content, AnimationSpeed::scale_duration(duration, SCREENS), onComplete
    );
}

type FadeInContentFromBottomFn = extern "C" fn(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32, offset: *mut Il2CppObject, onComplete: *mut Il2CppObject);
extern "C" fn FadeInContentFromBottom(this: *mut Il2CppObject, content: *mut Il2CppObject, duration: f32, offset: *mut Il2CppObject, onComplete: *mut Il2CppObject) {
    log_duration("FadeInContentFromBottom", duration);

    get_orig_fn!(FadeInContentFromBottom, FadeInContentFromBottomFn)(
        this, content, AnimationSpeed::scale_duration(duration, SCREENS), offset, onComplete
    );
}

fn log_duration(name: &str, duration: f32) {
    if AnimationSpeed::factor(SCREENS) != 1.0 {
        debug!("SingleModeResultContentBase::{}({})", name, duration);
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

    let class = SingleModeResultContentBase;

    let fade_in_content_addr = unsafe { AnimationSpeed::resolve_method(
        class, "FadeInContent",
        &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    let fade_in_right_addr = unsafe { AnimationSpeed::resolve_method(
        class, "FadeInContentFromRight",
        &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    let fade_in_bottom_addr = unsafe { AnimationSpeed::resolve_method(
        class, "FadeInContentFromBottom",
        &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };

    if fade_in_content_addr != 0 { new_hook!(fade_in_content_addr, FadeInContent); }
    if fade_in_right_addr != 0 { new_hook!(fade_in_right_addr, FadeInContentFromRight); }
    if fade_in_bottom_addr != 0 { new_hook!(fade_in_bottom_addr, FadeInContentFromBottom); }
}
