use std::sync::atomic::{AtomicU8, Ordering};

use crate::{
    windows::free_camera,
    il2cpp::{
        symbols::get_method_addr,
        types::*,
    },
};

static GET_BUTTON_HOOK_ID: AtomicU8 = AtomicU8::new(1);
static GET_BUTTON_DOWN_HOOK_ID: AtomicU8 = AtomicU8::new(2);
static GET_BUTTON_UP_HOOK_ID: AtomicU8 = AtomicU8::new(3);
static GET_AXIS_HOOK_ID: AtomicU8 = AtomicU8::new(4);
static GET_VECTOR2_HOOK_ID: AtomicU8 = AtomicU8::new(5);
static KEYBOARD_TRIGGER_HOOK_ID: AtomicU8 = AtomicU8::new(6);
static GAMEPAD_TRIGGER_HOOK_ID: AtomicU8 = AtomicU8::new(7);
static ANY_KEY_TRIGGER_HOOK_ID: AtomicU8 = AtomicU8::new(8);

#[inline(always)]
fn preserve_hook_identity(identity: &AtomicU8) {
    std::hint::black_box(identity.load(Ordering::Relaxed));
}

type InputButtonFn = extern "C" fn(this: *mut Il2CppObject, action_name: *mut Il2CppString) -> bool;

// C2: this macro writes the three input wrappers `init` below arms, and the game polls them every
// frame. Its body used to be a plain `extern "C" fn`, so it was one of the hook boundaries the
// barrier C2 installs was not standing on, and the mod half of it is exactly the shape C2 names:
// `is_game_input_capture_active()` reaches `free_camera`'s `STATE` / `is_enabled()` - a `Mutex`
// taken with `unwrap()` in `src/windows/free_camera.rs` and `Hachimi::instance()` - so a poisoned
// lock turns every later per-frame `GetButton` into a panic crossing the FFI into the trampoline.
// It now expands into `def_detour!`, like `GetAxis`, `GetVector2` and the rest of this file. A body
// that panicked is answered with the call the game would have got with no mod installed, behind
// `detour_fallback`; a body that faulted is answered with `false`, the same inert value the capture
// branch already returns, because the fallback would replay the arguments that just faulted.
//
// The second arm takes the capture decision as an expression. No shipped instantiation uses it: the
// tests at the end of the file build a wrapper out of this same macro with an injected fault, which
// is the only way to show the barrier is on this boundary without the game (AGENTS section 4 - a
// unit test reaches no trampoline and no `Hachimi::instance()`, and `is_game_input_capture_active()`
// itself reaches the singleton).
macro_rules! block_input_button {
    ($hook:ident, $identity:ident) => {
        block_input_button!($hook, $identity, free_camera::is_game_input_capture_active());
    };

    ($hook:ident, $identity:ident, $capture_active:expr) => {
        def_detour! {
            $hook(this: *mut Il2CppObject, action_name: *mut Il2CppString) -> bool {
                preserve_hook_identity(&$identity);

                if $capture_active {
                    false
                } else {
                    get_orig_fn!($hook, InputButtonFn)(this, action_name)
                }
            } bail {
                get_orig_fn!($hook, InputButtonFn)(this, action_name)
            }
        }
    };
}

block_input_button!(GetButton, GET_BUTTON_HOOK_ID);
block_input_button!(GetButtonDown, GET_BUTTON_DOWN_HOOK_ID);
block_input_button!(GetButtonUp, GET_BUTTON_UP_HOOK_ID);

type GetAxisFn = extern "C" fn(this: *mut Il2CppObject, action_name: *mut Il2CppString) -> f32;
def_detour! {
    GetAxis(this: *mut Il2CppObject, action_name: *mut Il2CppString) -> f32 {
            preserve_hook_identity(&GET_AXIS_HOOK_ID);
        if free_camera::is_game_input_capture_active() {
            0.0
        } else {
            get_orig_fn!(GetAxis, GetAxisFn)(this, action_name)
        }
    }
}

type GetVector2Fn = extern "C" fn(this: *mut Il2CppObject, action_name: *mut Il2CppString) -> Vector2_t;
def_detour! {
    GetVector2(this: *mut Il2CppObject, action_name: *mut Il2CppString) -> Vector2_t {
            preserve_hook_identity(&GET_VECTOR2_HOOK_ID);
        if free_camera::is_game_input_capture_active() {
            Vector2_t::default()
        } else {
            get_orig_fn!(GetVector2, GetVector2Fn)(this, action_name)
        }
    }
}

type IsActionKeyTriggeredInKeyboardFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
def_detour! {
    IsActionKeyTriggeredInKeyboard(this: *mut Il2CppObject) -> bool {
            preserve_hook_identity(&KEYBOARD_TRIGGER_HOOK_ID);
        if free_camera::is_game_input_capture_active() {
            false
        } else {
            get_orig_fn!(IsActionKeyTriggeredInKeyboard, IsActionKeyTriggeredInKeyboardFn)(this)
        }
    }
}

type IsActionButtonTriggeredInGamepadFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
def_detour! {
    IsActionButtonTriggeredInGamepad(this: *mut Il2CppObject) -> bool {
            preserve_hook_identity(&GAMEPAD_TRIGGER_HOOK_ID);
        if free_camera::is_game_input_capture_active() {
            false
        } else {
            get_orig_fn!(IsActionButtonTriggeredInGamepad, IsActionButtonTriggeredInGamepadFn)(this)
        }
    }
}

type get_IsAnyKeyTriggeredInKeyboardFn = extern "C" fn() -> bool;
def_detour! {
    get_IsAnyKeyTriggeredInKeyboard() -> bool {
            preserve_hook_identity(&ANY_KEY_TRIGGER_HOOK_ID);
        if free_camera::is_game_input_capture_active() {
            false
        } else {
            get_orig_fn!(get_IsAnyKeyTriggeredInKeyboard, get_IsAnyKeyTriggeredInKeyboardFn)()
        }
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, "Gallop", InputSystemManager);

    let get_button_addr = get_method_addr(InputSystemManager, c"GetButton", 1);
    new_hook!(get_button_addr, GetButton);

    let get_button_down_addr = get_method_addr(InputSystemManager, c"GetButtonDown", 1);
    new_hook!(get_button_down_addr, GetButtonDown);

    let get_button_up_addr = get_method_addr(InputSystemManager, c"GetButtonUp", 1);
    new_hook!(get_button_up_addr, GetButtonUp);

    let get_axis_addr = get_method_addr(InputSystemManager, c"GetAxis", 1);
    new_hook!(get_axis_addr, GetAxis);

    let get_vector2_addr = get_method_addr(InputSystemManager, c"GetVector2", 1);
    new_hook!(get_vector2_addr, GetVector2);

    let is_action_key_triggered_addr =
        get_method_addr(InputSystemManager, c"IsActionKeyTriggeredInKeyboard", 0);
    new_hook!(is_action_key_triggered_addr, IsActionKeyTriggeredInKeyboard);

    let is_action_button_triggered_addr =
        get_method_addr(InputSystemManager, c"IsActionButtonTriggeredInGamepad", 0);
    new_hook!(
        is_action_button_triggered_addr,
        IsActionButtonTriggeredInGamepad
    );

    let is_any_key_triggered_addr =
        get_method_addr(InputSystemManager, c"get_IsAnyKeyTriggeredInKeyboard", 0);
    new_hook!(is_any_key_triggered_addr, get_IsAnyKeyTriggeredInKeyboard);
}

// The wrappers the test below drives, built by `block_input_button!` itself. They are instantiated
// here - at the scope the macro is defined in, which is the scope the names in its body resolve in
// - with the capture decision replaced by a fault, so the game call stays unreachable:
// `get_orig_fn!` for a hook no `init` installed falls back to `Hachimi::instance()`, which ends the
// process (AGENTS section 4). That the calls return with one fault counted each and no panic is also
// what shows a `Faulted` trip did not replay the method through the `bail`.
//
// The read is the one `guard.rs` already uses for the same purpose, and only the target whose barrier
// has the SEH half compiles any of this.
#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
#[inline(never)]
fn capture_check_that_faults() -> bool {
    let mut value: u64 = 0;
    unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
    value != 0
}

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
block_input_button!(InjectedGetButton, GET_BUTTON_HOOK_ID, capture_check_that_faults());

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
block_input_button!(InjectedGetButtonDown, GET_BUTTON_DOWN_HOOK_ID, capture_check_that_faults());

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
block_input_button!(InjectedGetButtonUp, GET_BUTTON_UP_HOOK_ID, capture_check_that_faults());

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
mod tests {
    use crate::il2cpp::{hook::guard, types::*};

    use super::{InjectedGetButton, InjectedGetButtonDown, InjectedGetButtonUp};

    #[test]
    fn an_input_button_wrapper_fault_is_taken_at_the_boundary() {
        let _turn = guard::barrier_turn();
        let before_faults = guard::fault_trip_count();
        let before_panics = guard::panic_trip_count();

        for call in [
            InjectedGetButton as extern "C" fn(*mut Il2CppObject, *mut Il2CppString) -> bool,
            InjectedGetButtonDown as extern "C" fn(*mut Il2CppObject, *mut Il2CppString) -> bool,
            InjectedGetButtonUp as extern "C" fn(*mut Il2CppObject, *mut Il2CppString) -> bool,
        ] {
            assert!(!call(std::ptr::null_mut(), std::ptr::null_mut()), "the wrapper answers the trip itself");
        }

        assert_eq!(guard::fault_trip_count(), before_faults + 3, "one fault taken per wrapper");
        assert_eq!(guard::panic_trip_count(), before_panics, "no trip was counted as a panic");
        assert_eq!(guard::last_fault_code(), 0xC0000005, "the C frame stopped an access violation");
    }
}
