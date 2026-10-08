use std::sync::atomic::{AtomicBool, Ordering};

use crate::{core::Hachimi, il2cpp::{api::il2cpp_resolve_icall, types::*}};

/*** Time.timeScale ***/

/// Set while we are the ones writing `Time.timeScale`, so the hook below passes our
/// own value through instead of multiplying it a second time.
static APPLYING: AtomicBool = AtomicBool::new(false);

/// Raised when the config changes. The write itself is deferred to the game thread,
/// because Unity's native setters are not meant to be called from the overlay thread.
static DIRTY: AtomicBool = AtomicBool::new(false);

/// True while a non-neutral scale has been written, so switching `time_scale` back to
/// 1.0 restores the game's own control instead of leaving our value behind.
static WAS_SCALED: AtomicBool = AtomicBool::new(false);

/// Address of the icall implementation. 0 when this build does not expose it, in which
/// case every entry point here stays inert instead of calling through a null pointer.
static mut SET_TIME_SCALE_ADDR: usize = 0;

type SetTimeScaleFn = extern "C" fn(value: f32);
extern "C" fn set_timeScale(mut value: f32) {
    if !APPLYING.load(Ordering::Acquire) {
        let scale = Hachimi::instance().config.load().time_scale;
        if scale != 1.0 {
            // Whatever the game asked for is scaled, which also preserves the game's
            // own uses of 0 (pauses) and of sub-1 values (slow motion).
            value *= scale;
        }
    }

    get_orig_fn!(set_timeScale, SetTimeScaleFn)(value);
}

/// Push the configured scale into the game.
///
/// Unity keeps `Time.timeScale` across scene loads, but Gallop is free to set it itself
/// and the hook above only scales values the game writes, so this is called once after
/// the game initialises and again on every view change to cover the case where the game
/// never writes one. Calling the (possibly detoured) icall goes through the hook, hence
/// the APPLYING guard.
///
/// With `time_scale` at 1.0 and nothing previously written this does nothing at all, so
/// the game's own use of timeScale (pauses, slow motion) is left untouched. Dropping
/// back to 1.0 writes once to undo a scale that was previously applied.
pub fn apply() {
    let addr = unsafe { SET_TIME_SCALE_ADDR };
    if addr == 0 {
        return;
    }

    let scale = Hachimi::instance().config.load().time_scale;
    if scale == 1.0 && !WAS_SCALED.swap(false, Ordering::AcqRel) {
        return;
    }

    WAS_SCALED.store(scale != 1.0, Ordering::Release);

    unsafe {
        APPLYING.store(true, Ordering::Release);
        let set_time_scale: extern "C" fn(value: f32) = std::mem::transmute(addr);
        set_time_scale(scale);
        APPLYING.store(false, Ordering::Release);
    }
}

/// Called from the overlay when the config is saved; the actual write happens on the
/// next game-thread tick in `GameSystem::GameSystem_Update`.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Release);
}

pub fn apply_if_dirty() {
    if DIRTY.swap(false, Ordering::AcqRel) {
        apply();
    }
}

pub fn init(_UnityEngine_CoreModule: *const Il2CppImage) {
    let set_timeScale_addr = il2cpp_resolve_icall(
        c"UnityEngine.Time::set_timeScale(System.Single)".as_ptr()
    );

    unsafe { SET_TIME_SCALE_ADDR = set_timeScale_addr; }

    if set_timeScale_addr == 0 {
        error!("Failed to resolve UnityEngine.Time::set_timeScale, time scaling is unavailable on this build");
        return;
    }

    new_hook!(set_timeScale_addr, set_timeScale);
}
