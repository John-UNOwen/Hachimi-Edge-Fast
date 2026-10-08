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
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};

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

// The instance the guard is held for. IN_SKIP serialises the whole process while the
// guarded call is per instance (`skip_fn(this)` below takes the result part that asked),
// so a second result part asking while the first chain is still running is not queued,
// it is dropped. Which pointer was held says whether a contention was the expected
// re-entry from the completion callback or a different part that lost its skip.
static IN_SKIP_OWNER: AtomicPtr<Il2CppObject> = AtomicPtr::new(std::ptr::null_mut());

// A dropped request used to log nothing, so the entry line and the `SkipFadeInTween
// unavailable` warning were the only two signals a run had, and they cannot tell a
// finished tween chain from a swallowed request - which is exactly how run 3 read them
// as proof the skip worked end to end (DEFECTS.md:125-127, C38). Every dropped request
// now counts on its own line, so `entries - unavailable - contended` is the number of
// chains this hook actually finished.
static SKIP_GUARD_CONTENDED: AtomicUsize = AtomicUsize::new(0);
const GUARD_DETAIL_LIMIT: usize = 6;
const GUARD_CHUNK: usize = 4096;

struct SkipGuard;

impl SkipGuard {
    // Taken only when the flag was free, and cleared on every exit path, including a
    // panic inside the game call.
    fn try_enter(this: *mut Il2CppObject) -> Option<Self> {
        if IN_SKIP.swap(true, Ordering::AcqRel) { return None; }

        IN_SKIP_OWNER.store(this, Ordering::Release);

        Some(SkipGuard)
    }
}

impl Drop for SkipGuard {
    fn drop(&mut self) {
        IN_SKIP_OWNER.store(std::ptr::null_mut(), Ordering::Release);
        IN_SKIP.store(false, Ordering::Release);
    }
}

fn log_guard_contended(this: *mut Il2CppObject) {
    let contended = SKIP_GUARD_CONTENDED.fetch_add(1, Ordering::Relaxed) + 1;

    // The chain that held the guard can have finished between the refused swap and this
    // read, which prints as `held for 0x0`: the request was still dropped, and that is the
    // fact the count exists to carry.
    let held_for = IN_SKIP_OWNER.load(Ordering::Acquire);

    if contended <= GUARD_DETAIL_LIMIT {
        warn!("auto_skip_result_screens: skip guard busy, request {contended} dropped (asked by {this:p}, held for {held_for:p})");
    } else if contended % GUARD_CHUNK == 0 {
        warn!("auto_skip_result_screens: skip guard has dropped {contended} requests");
    }
}

type ActivateSkipButtonFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn ActivateSkipButton(this: *mut Il2CppObject) {
    // Without a line here the run log could only prove the hook was installed, never that
    // auto_skip_result_screens actually ran. An entry line counts a request; the
    // `SkipFadeInTween unavailable` warning and the `skip guard busy` line are the two ways
    // a request that was logged can still have done nothing.
    let auto_skip = Hachimi::instance().config.load().auto_skip_result_screens;
    debug!("SingleModeResultContentBase::ActivateSkipButton (auto_skip_result_screens {auto_skip})");

    get_orig_fn!(ActivateSkipButton, ActivateSkipButtonFn)(this);

    if !auto_skip {
        return;
    }

    let addr = match skip_fade_in_tween_addr(this) {
        Some(addr) => addr,
        None => return,
    };

    let _guard = match SkipGuard::try_enter(this) {
        Some(guard) => guard,
        None => {
            log_guard_contended(this);

            return;
        }
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
