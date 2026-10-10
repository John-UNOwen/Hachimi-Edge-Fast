#![allow(non_upper_case_globals)]

macro_rules! new_hook {
    ($orig:ident, $hook:ident) => (
        let hachimi = crate::core::Hachimi::instance();
        if !hachimi.config.load().disabled_hooks.contains(stringify!($hook)) {
            info!("new_hook!: {}", stringify!($hook));
            if ($orig != 0) {
                let res = hachimi.interceptor.hook($orig as usize, $hook as *const () as usize);
                if let Err(e) = res {
                    error!("{}", e);
                }
            }
            else {
                error!("{} is null", stringify!($orig));
            }
        }
        else {
            info!("[DISABLED] new_hook!: {}", stringify!($hook));
        }
    )
}

// C2: a detour wrapper is written with this macro, so the barrier in `guard` below is part of what
// a hook *is* and not something each hook has to remember. "A wrapper", not "every wrapper": the
// six helper macros that used to expand into a bare `extern "C" fn` (four in
// `umamusume/LiveTimelineControl.rs`, `block_input_button!` in `umamusume/InputSystemManager.rs` and
// `def_getter_hook!` in `umamusume/AnimationSpeed.rs`) now all expand into this macro. The two Live
// secondary-camera forms, whose RAII depth guard has to outlive the frame the C frame abandons, reach
// it through the `prelude` arm below rather than by writing the barrier call out at the site: a
// wrapper that needs a guard of its own gets the same two trip answers every other armed detour gets,
// because the arm that gives them is what it is built from.
// The audit that keeps that claim checkable is `tools/scan_hook_barriers.py`: it walks the wrappers
// `new_hook!` arms, which is the population a `def_detour!` scan structurally cannot see, and prints
// for each armed boundary which barrier stands on it and which answer that arm gives a `Panicked`.
// `def_getter_hook!` is the arm that macro family does not fully cover, and that gap is said rather
// than hidden: it takes the `answer -> ty` publish arm, which hands back what the body published and
// falls to `invented_answer` otherwise, and has no room for a stated `fallback` beside it. The five
// wrappers it writes state their no-answer value in the body instead, on the branch that cannot
// reach the original at all, so the only trip left there that the barrier answers with a value of
// its own making is a fault taken inside the game's own method.
//
// The body moves into a closure - an ordinary Rust frame - and runs behind the barrier,
// which stops a Rust panic and, on Windows, a structured exception. Left unwritten this
// way, a panic that reaches the end of an `extern "C"` body becomes rustc's
// "panic in a function that cannot unwind" abort, and a bad pointer in game memory
// unwinds through a trampoline frame that has no unwind info at all.
//
// `Panicked` and `Faulted` are the barrier's two trips, and they fall back differently.
//
// `Panicked` means the mod's own code stopped: `this` and the arguments are the game's,
// intact, and the game's method never received them, so the wrapper still owes the game the
// call it would have got with no mod installed. That is `bail`, and it runs behind
// `detour_fallback`, i.e. behind the same barrier - after a trip it is the only call left in
// the wrapper, and it must not be the one that leaves the boundary unguarded. A fallback can
// fault: `get_orig_fn!` answers 0 for a trampoline the detach path has just taken back (C1).
//
// `Faulted` means the body was stopped by the state it was handed - a null or freed `this`, a
// bad argument, an index past an `Il2CppArray`. A `bail` replays those same arguments into the
// same game method, so it faults again, deterministically, because the fault came from the
// inputs; and the body may already have reached that method, which makes the fallback a second
// entry into it. So a `Faulted` trip never replays the call.
//
// Neither trip may answer with a value the wrapper made up. A trip answers, in this order:
//
// 1. **what the game already returned**. A body that published its answer - the `answer` binding
//    the arms below hand it - has the value the game's own method produced, and the barrier's
//    `Answered` outcome hands that value back. Until this existed a trip threw it away:
//    `LiveViewController.rs` discarded a `moved` that was already `true` and told Unity's
//    coroutine driver the live-view orientation coroutine had finished, `StoryFrameProbe.rs` and
//    `TrainingCuttProbe.rs` discarded a wait-frame count and a target fps the game had just
//    handed over and answered 0 frames and a divisor of zero, and `GameSystem.rs` handed the
//    game's coroutine starter a zeroed enumerator.
// 2. **the value the wrapper says its no-answer value is**, and *both* trips get it. Two forms:
//    the `coroutine` arms answer a door with `coroutine_trip` (the coroutine is not finished, and
//    the door comes out of the registry so the game's own `MoveNext` answers every later call -
//    `false` is not one of its answers, because `false` is the value that ends a live coroutine),
//    and a value wrapper that writes `fallback { .. }` states the answer it gives when it cannot
//    answer with the game's. A `fallback` answers a `Faulted` as well as a `Panicked`, because it
//    is not a replay of the call the trip came out of: `get_Width`'s is `UnityScreen::get_width()`,
//    and its own comment says 0 cannot be passed on because the game and `windows/utils.rs` use
//    this number as a divisor.
// 3. **a refusal**, where a wrapper published nothing and wrote no rule. The barrier still has to
//    hand the caller something of the wrapper's return type, so it hands back the zero value - the
//    answer every wrapper written without a `bail` already gave, kept for the shapes where a zero
//    means "no effect" (`IsSkipToTextClip` answering "not skipped") - but it no longer hands it
//    back quietly: `guard::invented_answer` counts the trip and the first one of that wrapper names
//    it in the log. A zero the game acts on is only acceptable when the run can see a barrier
//    invented it.
//
// A `bail { .. }` is a different thing from a `fallback { .. }`, and the difference is which trips
// it may answer. A `bail` is the call the game would have got with no mod installed, so the
// `Panicked` trip - where the mod's own code stopped and the game's arguments never reached its
// method - owes it. A `Faulted` trip may not run it: the fault came from those arguments or the
// object they point at, the body may already be inside that method, and a replay faults again
// deterministically. A wrapper whose safe answer is *not* that call - because the call is the thing
// that faults, as it is for a `get_orig_fn!` that answered 0 for a trampoline the detach path
// took back (C1) - writes `fallback`, not `bail`.
//
// The name a body publishes through is written by the *call site* (`answer`, or `_answer` where
// a body has nothing to publish) and not by this macro, because a name a macro invents is
// invisible to the tokens it hands over: `macro_rules!` hygiene would make the body's own
// `answer.publish(..)` a different variable from the closure parameter the arm created.
//
// `moves` is for the few wrappers whose body hands a by value struct parameter over to the
// game: a closure that only borrows its captures cannot move out of one, so those bodies
// run in a closure that owns the parameters, and a barrier trip there is a refusal - the
// parameter is already consumed by the body that tripped, so no answer can be replayed.
macro_rules! def_detour {
    // A coroutine door - a detour on `MoveNext/0 -> bool()`, the method Unity drives a live
    // coroutine through - whose body publishes the game's own answer, with the call the game
    // would have got. `Panicked` owes that call, behind the barrier, and a fallback that itself
    // trips falls through to the door rule. `Faulted` may not replay it.
    //
    // The answer binding is written before `->` because a `ty` fragment may only be followed by
    // `{ [ => , > = : ; | as where`, so an identifier cannot sit between the return type and the
    // body.
    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) coroutine $answer:ident -> bool $body:block bail $bail:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> bool {
            match $crate::il2cpp::hook::guard::detour_barrier(|$answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                $crate::il2cpp::hook::guard::BarrierOutcome::Panicked =>
                    $crate::il2cpp::hook::guard::detour_fallback_or(
                        |_answer| $bail,
                        || $crate::il2cpp::hook::guard::coroutine_trip($name as *const ()),
                    ),
                $crate::il2cpp::hook::guard::BarrierOutcome::Faulted =>
                    $crate::il2cpp::hook::guard::coroutine_trip($name as *const ()),
            }
        }
    };

    // The same door with no fallback call written: a trip with no published answer is answered by
    // the door rule directly.
    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) coroutine $answer:ident -> bool $body:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> bool {
            match $crate::il2cpp::hook::guard::detour_barrier(|$answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                $crate::il2cpp::hook::guard::BarrierOutcome::Panicked
                | $crate::il2cpp::hook::guard::BarrierOutcome::Faulted =>
                    $crate::il2cpp::hook::guard::coroutine_trip($name as *const ()),
            }
        }
    };

    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) moves -> $ret:ty $body:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> $ret {
            static NO_ANSWER_WARNED: ::std::sync::atomic::AtomicPtr<()> =
                ::std::sync::atomic::AtomicPtr::new($name as *mut ());

            match $crate::il2cpp::hook::guard::detour_barrier(move |_answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                // Nothing published and no rule written, and no rule is even possible here: the
                // parameters are already consumed by the body that tripped, so nothing left in the
                // wrapper can replay them. A refusal, counted and named once.
                _ => $crate::il2cpp::hook::guard::invented_answer(stringify!($name), &NO_ANSWER_WARNED),
            }
        }
    };

    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) moves $body:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) {
            let _ = $crate::il2cpp::hook::guard::detour_barrier(move |_answer| $body);
        }
    };

    // A value-returning wrapper whose body publishes the game's answer before it does its own
    // work. Its trips are answered from what it published. This arm sits after the `moves` arms:
    // `moves` is a keyword here, and an arm that takes an identifier in that position would
    // otherwise read it as the body's answer binding.
    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) $answer:ident -> $ret:ty $body:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> $ret {
            static NO_ANSWER_WARNED: ::std::sync::atomic::AtomicPtr<()> =
                ::std::sync::atomic::AtomicPtr::new($name as *mut ());

            match $crate::il2cpp::hook::guard::detour_barrier(|$answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                // The body published nothing before it tripped, so the barrier has no answer it
                // took from the game and this wrapper wrote no rule of its own: a refusal.
                _ => $crate::il2cpp::hook::guard::invented_answer(stringify!($name), &NO_ANSWER_WARNED),
            }
        }
    };

    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) -> $ret:ty $body:block bail $bail:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> $ret {
            static NO_ANSWER_WARNED: ::std::sync::atomic::AtomicPtr<()> =
                ::std::sync::atomic::AtomicPtr::new($name as *mut ());

            match $crate::il2cpp::hook::guard::detour_barrier(|_answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                // The call the game would have got, behind the barrier, and if that call trips too
                // this wrapper has nothing left to say for itself: a refusal.
                $crate::il2cpp::hook::guard::BarrierOutcome::Panicked =>
                    $crate::il2cpp::hook::guard::detour_fallback_or(
                        |_answer| $bail,
                        || $crate::il2cpp::hook::guard::invented_answer(stringify!($name), &NO_ANSWER_WARNED),
                    ),
                // A `Faulted` may not run a `bail`: it would replay the arguments that just
                // faulted, into a method the body may already be inside. Nothing else was stated, so
                // nothing here is the wrapper's answer - this is the trip the barrier refuses to
                // answer silently.
                $crate::il2cpp::hook::guard::BarrierOutcome::Faulted =>
                    $crate::il2cpp::hook::guard::invented_answer(stringify!($name), &NO_ANSWER_WARNED),
            }
        }
    };

    // A value wrapper that states the answer it gives when it cannot answer with the game's, written
    // as a value and not as a call. Both trips take it, because it is not a replay of the state the
    // trip came out of, and it runs behind the barrier like every other fallback: the answer it
    // states may itself reach into the game (`UnityScreen::get_width()`) and may fault.
    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) -> $ret:ty $body:block fallback $fallback:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> $ret {
            static NO_ANSWER_WARNED: ::std::sync::atomic::AtomicPtr<()> =
                ::std::sync::atomic::AtomicPtr::new($name as *mut ());

            match $crate::il2cpp::hook::guard::detour_barrier(|_answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                $crate::il2cpp::hook::guard::BarrierOutcome::Panicked
                | $crate::il2cpp::hook::guard::BarrierOutcome::Faulted =>
                    $crate::il2cpp::hook::guard::detour_fallback_or(
                        |_answer| $fallback,
                        || $crate::il2cpp::hook::guard::invented_answer(stringify!($name), &NO_ANSWER_WARNED),
                    ),
            }
        }
    };

    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) -> $ret:ty $body:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) -> $ret {
            static NO_ANSWER_WARNED: ::std::sync::atomic::AtomicPtr<()> =
                ::std::sync::atomic::AtomicPtr::new($name as *mut ());

            match $crate::il2cpp::hook::guard::detour_barrier(|_answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(value)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(value) => value,
                // Nothing published, no `bail` owed, no `fallback` stated: a refusal.
                _ => $crate::il2cpp::hook::guard::invented_answer(stringify!($name), &NO_ANSWER_WARNED),
            }
        }
    };

    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) $body:block bail $bail:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) {
            match $crate::il2cpp::hook::guard::detour_barrier(|_answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(_)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(_) => {}
                $crate::il2cpp::hook::guard::BarrierOutcome::Panicked =>
                    $crate::il2cpp::hook::guard::detour_fallback(|_answer| $bail),
                $crate::il2cpp::hook::guard::BarrierOutcome::Faulted => {}
            }
        }
    };

    // The same void wrapper with a `bail`, and something the wrapper frame has to hold for the
    // whole call: an RAII guard that the barrier's body may not own.
    //
    // The C frame stops a fault by returning out of the frames it wrapped, and those frames'
    // destructors never run. A guard taken inside the body is therefore lost to the wrapper that
    // tripped, and any state it was keeping is left set for every later call - which at the Live
    // secondary-camera wrappers is the depth the game's own nested `GetValue` reads. Out here the
    // guard is a local of the wrapper: it is raised before the barrier, still raised while the
    // `Panicked` trip replays the game's call (that call re-enters the same nested path, which is
    // why it must run inside the guard and not after it), and dropped when the wrapper returns on
    // any of the three answers.
    //
    // The trip answers are the plain `bail` arm's, unchanged: that is the point of this arm
    // existing. A wrapper needing a guard gets the barrier's own semantics, not a barrier call it
    // writes out at the site.
    // The guard's own scope is what this arm is for, so the block stays in the wrapper frame: it is
    // dropped on all three answers, and - unlike a guard the body takes - it is still held while the
    // `Panicked` answer replays the game's call. The block is written at the call site, so when the
    // call site is itself a macro it cannot name the wrapper's own parameters; the guards this arm
    // exists for take none.
    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) prelude $prelude:block $body:block bail $bail:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) {
            let _wrapper_guard = $prelude;

            match $crate::il2cpp::hook::guard::detour_barrier(|_answer| $body) {
                $crate::il2cpp::hook::guard::BarrierOutcome::Done(_)
                | $crate::il2cpp::hook::guard::BarrierOutcome::Answered(_) => {}
                $crate::il2cpp::hook::guard::BarrierOutcome::Panicked =>
                    $crate::il2cpp::hook::guard::detour_fallback(|_answer| $bail),
                $crate::il2cpp::hook::guard::BarrierOutcome::Faulted => {}
            }
        }
    };

    ($(#[$meta:meta])* $vis:vis $name:ident ( $($params:tt)* ) $body:block) => {
        $(#[$meta])* $vis extern "C" fn $name($($params)*) {
            let _ = $crate::il2cpp::hook::guard::detour_barrier(|_answer| $body);
        }
    };
}

macro_rules! get_assembly_image_or_return {
    ($var_name:ident, $assembly_name:tt) => (
        let $var_name = match crate::il2cpp::symbols::get_assembly_image(cstr!($assembly_name)) {
            Ok(v) => v,
            Err(e) => {
                error!("{}", e);
                return;
            }
        };
    )
}

macro_rules! get_class_or_return {
    ($image:ident, $namespace:tt, $class_name:ident) => (
        let $class_name = match crate::il2cpp::symbols::get_class($image, cstr!($namespace), cstr!($class_name)) {
            Ok(v) => v,
            Err(e) => {
                error!("{}", e);
                return;
            }
        };
    )
}

macro_rules! find_nested_class_or_return {
    ($parent:ident, $class_name:ident) => (
        let $class_name = match crate::il2cpp::symbols::find_nested_class($parent, cstr!($class_name)) {
            Ok(v) => v,
            Err(e) => {
                error!("{}", e);
                return;
            }
        };
    )
}

/// C1 / section 6: the "this wrapper already said it" marker every macro-built wrapper keeps.
///
/// The 0 guard in `def_method_wrapper_fn!` and `impl_addr_wrapper_fn!` has been there since C1
/// opened, but the `warn!` behind it sat on the call path. A wrapper whose target never resolved
/// is still *called* - by a GUI action, by the translation pass, by a detour on a tween tick - and
/// every one of those calls formatted a line, took the logger's lock and wrote to hachimi.log: the
/// per-call logging section 6 forbids, paid on the one path that is already doing nothing.
///
/// The marker starts out holding the wrapper's own address, so no two markers start as the same
/// bytes and a linker folding identical data has nothing to fold onto another wrapper - the trap
/// `CachedTrampoline` documents in `core/interceptor.rs`. Skipping stores null, so the call that
/// found the target unresolved says it and every later one costs one swap, one compare, and no line.
pub struct UnresolvedMarker(std::sync::atomic::AtomicPtr<std::os::raw::c_void>);

impl UnresolvedMarker {
    pub const fn new(wrapper: *const ()) -> Self {
        Self(std::sync::atomic::AtomicPtr::new(wrapper as *mut std::os::raw::c_void))
    }

    /// Say it if nobody has said it yet, and report whether this call was the one that did.
    /// `#[cold]`: the only reader is the branch that is already skipping the call, and nothing on
    /// the way to a resolved target touches this.
    #[cold]
    pub fn warn_once(&self, name: &str) -> bool {
        let unwarned = self.0.swap(std::ptr::null_mut(), std::sync::atomic::Ordering::AcqRel);

        if unwarned.is_null() {
            return false;
        }

        warn!("{name}: target address is unresolved, call skipped");
        true
    }
}

// shorter ver of doing impl_addr_wrapper_fn!()
macro_rules! def_method_wrapper_fn {
    ($name:tt, $addr:ident, $ret:ty, $($v:ident: $t:ty),*) => {
        static mut $addr: usize = 0;
        pub fn $name($($v: $t),*) -> $ret {
            // Reached from mod code as well as from a trampoline, so an unresolved target
            // is reachable on an ordinary feature path. Jumping to 0 is not recoverable.
            let addr = unsafe { $addr };

            if addr == 0 {
                static UNRESOLVED: $crate::il2cpp::hook::UnresolvedMarker
                    = $crate::il2cpp::hook::UnresolvedMarker::new($name as *const ());

                UNRESOLVED.warn_once(stringify!($name));
                return unsafe { ::std::mem::zeroed() };
            }

            let orig_fn: extern "C" fn($($v: $t),*) -> $ret = unsafe { ::std::mem::transmute(addr) };
            orig_fn($($v),*)
        }
    };
}

macro_rules! impl_addr_wrapper_fn {
    ($name:tt, $addr:ident, $ret:ty, $($v:ident: $t:ty),*) => {
        pub fn $name($($v: $t),*) -> $ret {
            // Same rule as `def_method_wrapper_fn!`: inert when the address never arrived, and it
            // is said once per wrapper rather than once per call.
            let addr = unsafe { $addr };

            if addr == 0 {
                static UNRESOLVED: $crate::il2cpp::hook::UnresolvedMarker
                    = $crate::il2cpp::hook::UnresolvedMarker::new($name as *const ());

                UNRESOLVED.warn_once(stringify!($name));
                return unsafe { ::std::mem::zeroed() };
            }

            let orig_fn: extern "C" fn($($v: $t),*) -> $ret = unsafe { ::std::mem::transmute(addr) };
            orig_fn($($v),*)
        }
    };
}

macro_rules! impl_enum_eq {
    // impl_enum_eq!(Enum, T)
    ($enum_ty:ty, $target_ty:ty) => {
        impl PartialEq<$enum_ty> for $target_ty {
            fn eq(&self, other: &$enum_ty) -> bool {
                *self == *other as $target_ty
            }
        }

        impl PartialEq<$target_ty> for $enum_ty {
            fn eq(&self, other: &$target_ty) -> bool {
                *self as $target_ty == *other
            }
        }
    };

    // Defaults T to i32 if no second arg
    ($enum_ty:ty) => {
        impl_enum_eq!($enum_ty, i32);
    };
}

macro_rules! impl_enum_ord {
    // impl_enum_ord!(Enum, T)
    ($enum_ty:ty, $target_ty:ty) => {
        impl std::cmp::PartialOrd<$target_ty> for $enum_ty {
            fn partial_cmp(&self, other: &$target_ty) -> Option<std::cmp::Ordering> {
                (*self as $target_ty).partial_cmp(other)
            }
        }

        impl std::cmp::PartialOrd<$enum_ty> for $target_ty {
            fn partial_cmp(&self, other: &$enum_ty) -> Option<std::cmp::Ordering> {
                self.partial_cmp(&(*other as $target_ty))
            }
        }
    };

    // Defaults T to i32 if no second arg
    ($enum_ty:ty) => {
        impl_enum_ord!($enum_ty, i32);
    };
}

macro_rules! def_field_value_accessors {
    ($get_name:ident, $set_name:ident, $field:ident, $t:ty) => {
        static mut $field: *mut FieldInfo = 0 as _;
        pub fn $get_name(this: *mut Il2CppObject) -> $t {
            let field = unsafe { $field };
            if field.is_null() { return unsafe { ::std::mem::zeroed() }; }

            crate::il2cpp::symbols::get_field_value(this, field)
        }

        pub fn $set_name(this: *mut Il2CppObject, value: $t) {
            let field = unsafe { $field };
            if field.is_null() { return; }

            crate::il2cpp::symbols::set_field_value(this, field, &value)
        }
    };
    (get $get_name:ident, $field:ident, $t:ty) => {
        static mut $field: *mut FieldInfo = 0 as _;
        pub fn $get_name(this: *mut Il2CppObject) -> $t {
            let field = unsafe { $field };
            if field.is_null() { return unsafe { ::std::mem::zeroed() }; }

            crate::il2cpp::symbols::get_field_value(this, field)
        }
    };
    (set $set_name:ident, $field:ident, $t:ty) => {
        static mut $field: *mut FieldInfo = 0 as _;
        pub fn $set_name(this: *mut Il2CppObject, value: $t) {
            let field = unsafe { $field };
            if field.is_null() { return; }

            crate::il2cpp::symbols::set_field_value(this, field, &value)
        }
    };
}

macro_rules! def_field_object_accessors {
    ($get_name:ident, $set_name:ident, $field:ident, $t:ty) => {
        static mut $field: *mut FieldInfo = 0 as _;
        pub fn $get_name(this: *mut Il2CppObject) -> *mut $t {
            let field = unsafe { $field };
            if field.is_null() { return ::std::ptr::null_mut(); }

            crate::il2cpp::symbols::get_field_object_value(this, field)
        }

        pub fn $set_name(this: *mut Il2CppObject, value: *mut $t) {
            let field = unsafe { $field };
            if field.is_null() { return; }

            crate::il2cpp::symbols::set_field_object_value(this, field, value)
        }
    };
    (get $get_name:ident, $field:ident, $t:ty) => {
        static mut $field: *mut FieldInfo = 0 as _;
        pub fn $get_name(this: *mut Il2CppObject) -> *mut $t {
            let field = unsafe { $field };
            if field.is_null() { return ::std::ptr::null_mut(); }

            crate::il2cpp::symbols::get_field_object_value(this, field)
        }
    };
    (set $set_name:ident, $field:ident, $t:ty) => {
        static mut $field: *mut FieldInfo = 0 as _;
        pub fn $set_name(this: *mut Il2CppObject, value: *mut $t) {
            let field = unsafe { $field };
            if field.is_null() { return; }

            crate::il2cpp::symbols::set_field_object_value(this, field, value)
        }
    };
}

// C2: the barrier every wrapper the macro above builds runs behind. It is written in this
// file rather than in a `guard.rs` beside it for the same reason the `__try`/`__except`
// frame is written by `build.rs`: the barrier is load-bearing for 103 wrappers, and a file
// git does not track is not part of what a commit carries. `git add -u` and `git commit -am`
// both skip an untracked source, and the tree they produce is one where every wrapper names a
// module that nothing provides. Held here, the macro and what it expands into travel in one
// tracked file. The module path is unchanged, so no wrapper in `il2cpp/hook/` moves.
pub mod guard {
    //! C2: the barrier every detour body runs behind.
    //!
    //! A detour wrapper is an `extern "C" fn` whose body formats, indexes and reads game
    //! memory, and the frame that calls it is generated machine code (the trampoline) reached
    //! from the game's own frame loop, whose caller is the IL2CPP runtime. Two things can
    //! leave such a body and neither of them has anywhere to go:
    //!
    //! - A Rust panic. `extern "C"` is a boundary rustc refuses to unwind through, so the
    //!   panic does not propagate: it becomes a non-unwinding panic and the process aborts.
    //!   Measured in a scratch probe on this toolchain (rustc 1.98.1): an `extern "C" fn` that
    //!   panics, called through a frame with no unwind info, prints
    //!   `panic in a function that cannot unwind` and exits with 0xC0000409. The abort is
    //!   worse here than it looks, because `src/windows/hachimi_impl.rs` kills
    //!   `UnityCrashHandler64.exe`, so there is no dump.
    //! - A structured exception: a null `this`, a freed `Il2CppObject`, an index past an
    //!   Il2CppArray. `catch_unwind` does not catch those (measured in the same probe: the
    //!   process dies with the access violation), and the unwinder cannot step over a
    //!   trampoline frame, which has no entry in any module's `.pdata`.
    //!
    //! The barrier turns both into "this detour did nothing on this call". The body runs
    //! inside a closure - an ordinary Rust frame - behind `catch_unwind` for panics and, on
    //! Windows, behind a `__try`/`__except` frame (the C frame `build.rs` writes into OUT_DIR)
    //! for faults, so nothing unwinds across the boundary. `catch_unwind` is nested *inside* the SEH frame on
    //! purpose: a panic is then taken by Rust, where destructors run and the panic
    //! bookkeeping stays clean, and the C frame only ever sees a fault.
    //!
    //! A trip is reported as the kind it was (`BarrierOutcome`), because the kinds do not owe the
    //! game the same fallback, and because a trip may not answer with a value the barrier made up:
    //!
    //! - The body publishes the answer it has already been given - `GameAnswer`, held in the
    //!   barrier's own frame. The C frame stops a fault by returning out of the closure frames it
    //!   wrapped, so a local of the body is gone when the trip answers; the cell is a local of the
    //!   frame the trip returns *through*, so what the game's method returned is still there, and
    //!   the trip comes back as `Answered(that value)`. That is the answer `def_detour!` prefers,
    //!   and it is what stopped a trip in `LiveViewController.rs`'s mod half from telling Unity's
    //!   coroutine driver that a coroutine the game had just stepped was finished.
    //! - A `Panicked` is answered with the wrapper's `bail`, put *behind* the barrier by
    //!   `detour_fallback_or`: the game never received the call, so the wrapper still owes it.
    //! - A `Faulted` may not replay that call - a fault came from the arguments or the object the
    //!   wrapper was handed, and the body may already be inside that method. What a `Faulted` may
    //!   have is the wrapper's `fallback`: the answer it wrote for itself, which is a value and not
    //!   the call the fault came out of. `get_Width` is that shape - its fallback is what Unity
    //!   itself reports, and the C1 chain this module exists for (`get_orig_fn!` answering 0 for a
    //!   trampoline the detach path took back, a call through 0, an access violation) is the trip
    //!   that answer was written for.
    //! - A coroutine door (`MoveNext/0 -> bool()`) answers a trip that has no published value with
    //!   `coroutine_trip`: the coroutine is still running and this door comes out of the registry.
    //!   Answering it `false` is the fabrication this module must not make: it is the value that
    //!   ends a live coroutine mid-run.
    //! - Only after all of those does `invented_answer` answer, and it does not do it silently: the
    //!   zero value of the wrapper's return type is handed back because the `extern "C"` boundary
    //!   needs a value of that type, the trip is counted, and the first trip of that wrapper says in
    //!   the log which wrapper it was and what type it had to make up. Before this, the answer every
    //!   value arm gave was the same zero and nothing in a run could tell "the barrier held" from
    //!   "the game was handed a number it then divided by".
    //!
    //! It allocates nothing per call and logs nothing on the faulting path a hook reaches every
    //! frame: a trip only bumps a counter, printed once on the cold detach path - including which
    //! answer each trip gave, so a run can tell a swallowed fault answered with the game's own value
    //! from one answered with the wrapper's stated fallback from one the barrier had to make up, and
    //! - by `bail_trip_count`, which is in that line too - whether the answer a wrapper stated for
    //! itself ran or *also* tripped and left the barrier's refusal as the only thing left to hand
    //! over. The one exception is `invented_answer`: the *first* trip of a wrapper that had no answer
    //! writes one line naming it, because that is the trip whose value the game goes on to act on,
    //! and the latch behind it costs one atomic swap on a path that is already cold.
    //!
    //! On Android there is no SEH, so the barrier there covers panics only. A fault needs a
    //! signal handler, which is a different piece of work. Until that exists the whole fault half
    //! of this module - the counters, their accessors and the trip that bumps them - sits behind
    //! the same `cfg(all(target_os = "windows", target_env = "msvc"))` as the `__try`/`__except`
    //! frame that writes them, because on any other target nothing could ever write them and the
    //! cold report would be counting a barrier that is not there. The published answer and the
    //! coroutine door rule need no SEH, so those two counters exist on every target.

    use std::mem::MaybeUninit;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    use std::sync::atomic::AtomicU32;

    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    use std::ffi::c_void;

    use crate::core::interceptor::Interceptor;

    /// Calls the barrier stopped because the body panicked. Plain counters: no allocation, no
    /// log, no lock on the path a faulting hook takes.
    static PANIC_TRIPS: AtomicUsize = AtomicUsize::new(0);
    /// Calls the barrier stopped because the body took a hardware fault. Only the SEH frame can
    /// see one, so this counter exists only where that frame is compiled.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    static FAULT_TRIPS: AtomicUsize = AtomicUsize::new(0);
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    static LAST_FAULT_CODE: AtomicU32 = AtomicU32::new(0);
    /// Where the C frame's `__except` said the exception came from, kept for the cold report only -
    /// it is never dereferenced. A fault *at* address 0 is an execution that jumped to 0, which is
    /// what `get_orig_fn!` answering 0 for a trampoline the detach path took back looks like (C1);
    /// a fault at an address inside a module is a read through a pointer the wrapper trusted.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    static LAST_FAULT_ADDRESS: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
    /// Faults whose exception address was 0: an execution that jumped to 0. That is C1 - a
    /// `get_orig_fn!` answering 0 for a trampoline the detach path took back - and it is the chain
    /// that ends in this module answering for the game, so a run has to be able to see it happened.
    /// Measured in `the_c1_chain_faults_at_address_0_and_the_barrier_answers_it`: the barrier stops it
    /// with `0xC0000005` at address `0x0`, where a read through a pointer the wrapper trusted faults
    /// at an address inside the module instead.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    static NULL_TARGET_FAULT_TRIPS: AtomicUsize = AtomicUsize::new(0);
    /// Trips answered with the value the game's own method had already returned. Not a fault
    /// counter: the published answer works wherever the barrier does, SEH frame or not.
    static ANSWERED_TRIPS: AtomicUsize = AtomicUsize::new(0);
    /// Coroutine doors a trip took out of the registry, so the game's own `MoveNext` answered
    /// every later call on that enumerator.
    static COROUTINE_TAKEDOWNS: AtomicUsize = AtomicUsize::new(0);
    /// Trips that no published answer and nothing the wrapper stated could answer, so the barrier
    /// had to hand back a value of its own making. Counting these is the point of the item this
    /// exists for: a wrong value the game acts on is only tolerable when a run can see it happened,
    /// and which wrapper it happened to.
    static INVENTED_TRIPS: AtomicUsize = AtomicUsize::new(0);
    /// Fallback calls - the `bail` a `Panicked` owes, or the `fallback` a wrapper states - that the
    /// barrier had to stop as well. The number behind "the stated answer itself could not answer":
    /// tier three is only honest to a run if the run can see that the answer the wrapper wrote for
    /// itself is what failed, so this is printed by `trip_report` as the last field of the detach
    /// line, not kept for the tests alone.
    static BAIL_TRIPS: AtomicUsize = AtomicUsize::new(0);

    pub fn panic_trip_count() -> usize {
        PANIC_TRIPS.load(Ordering::Relaxed)
    }

    pub fn answered_trip_count() -> usize {
        ANSWERED_TRIPS.load(Ordering::Relaxed)
    }

    pub fn coroutine_takedown_count() -> usize {
        COROUTINE_TAKEDOWNS.load(Ordering::Relaxed)
    }

    pub fn invented_trip_count() -> usize {
        INVENTED_TRIPS.load(Ordering::Relaxed)
    }

    pub fn bail_trip_count() -> usize {
        BAIL_TRIPS.load(Ordering::Relaxed)
    }

    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    pub fn fault_trip_count() -> usize {
        FAULT_TRIPS.load(Ordering::Relaxed)
    }

    /// Faults the barrier stopped that were an execution through 0 - the C1 signature, and the half
    /// of the fault count that means the mod lost its own target rather than the game handing the
    /// wrapper a bad pointer.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    pub fn null_target_fault_trip_count() -> usize {
        NULL_TARGET_FAULT_TRIPS.load(Ordering::Relaxed)
    }

    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    pub fn last_fault_code() -> u32 {
        LAST_FAULT_CODE.load(Ordering::Relaxed)
    }

    /// The address the last fault came from, for the cold report and its tests. `null` is the
    /// signature of an execution that jumped to 0 (C1), not of "no fault yet" - `fault_trip_count()`
    /// answers that question.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    pub fn last_fault_address() -> *mut c_void {
        LAST_FAULT_ADDRESS.load(Ordering::Relaxed)
    }

    #[cold]
    fn trip_panics() {
        PANIC_TRIPS.fetch_add(1, Ordering::Relaxed);
    }

    /// A trip that answered with the value the game's own method had returned. Counted apart from
    /// the trip itself: the barrier stopped the mod's half, and the game still got its own answer.
    #[cold]
    fn trip_answered() {
        ANSWERED_TRIPS.fetch_add(1, Ordering::Relaxed);
    }

    /// The call the wrapper reached for *after* a trip tripped too. Counted apart from the invention
    /// it leads to, because "the wrapper's own answer faulted" is a different fact from "the barrier
    /// answered", and a run needs to know which of them it is looking at.
    #[cold]
    fn trip_bail() {
        BAIL_TRIPS.fetch_add(1, Ordering::Relaxed);
    }

    #[cold]
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    fn trip_faults(code: u32, address: *mut c_void) {
        FAULT_TRIPS.fetch_add(1, Ordering::Relaxed);
        LAST_FAULT_CODE.store(code, Ordering::Relaxed);
        LAST_FAULT_ADDRESS.store(address, Ordering::Relaxed);

        if address.is_null() {
            NULL_TARGET_FAULT_TRIPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Test-only: one turn for the trip counters. They are process-wide and the tests that drive the
    /// barrier are not all in this module - `umamusume/LiveTimelineControl.rs` and
    /// `umamusume/InputSystemManager.rs` assert on the same counters for the wrappers their own
    /// macros build - so every one of them takes this lock rather than each module keeping its own.
    /// `#[cfg(test)]`: it is not in the shipped build and no detour path touches it.
    #[cfg(test)]
    pub fn barrier_turn() -> std::sync::MutexGuard<'static, ()> {
        static COUNTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

        COUNTING.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Cold path only (DLL detach): a run says in the log how many detours the barrier had to
    /// stop, so a swallowed panic or fault is never invisible - and how each of those trips was
    /// *answered*, because "the barrier held" and "the game got a value it can live with" are two
    /// different claims. The line names only what the barrier on this target can see, so a target
    /// without the SEH frame never prints a fault count for a barrier that cannot detect one. The
    /// last field is `bail_trip_count()`: how many of those trips reached the answer the wrapper
    /// stated for itself and found that answer had tripped too, which is the tier-3 fact a run has
    /// to be able to read or the refusal is invisible again.
    pub fn report_trips() {
        if let Some(line) = trip_report() {
            warn!("{}", line);
        }
    }

    /// The detach line itself, assembled from the counters as they stand, or `None` when this
    /// target's barrier stopped nothing.
    ///
    /// It returns the line instead of logging it because the claim this line carries is a claim about
    /// a log line, and the test process installs no logger: `report_trips` is reached once, from
    /// `DLL_PROCESS_DETACH` (`src/windows/main.rs:84`), so as long as the assembly and the `warn!`
    /// were one function nothing in the tree could read the report a run is supposed to print. That
    /// is how `BAIL_TRIPS` - bumped by `detour_fallback_or` for every fallback or `bail` the barrier
    /// had to stop *after* a trip, i.e. "the wrapper's own stated answer could not answer either" -
    /// ended up counted, asserted by unit tests, and printed nowhere: the tier-3 promise that a run
    /// can tell a stated answer that ran from a stated answer that tripped had no line behind it.
    /// `trip_report` is that line, and `the_detach_report_names_the_stated_answer_that_could_not_answer`
    /// drives both halves of it through the shipped wrappers.
    ///
    /// One `String` on a path that runs once per process. Nothing here is on a path a detour runs.
    pub fn trip_report() -> Option<String> {
        let panics = panic_trip_count();
        let answered = answered_trip_count();
        let takendowns = coroutine_takedown_count();
        let invented = invented_trip_count();
        let bails = bail_trip_count();

        #[cfg(all(target_os = "windows", target_env = "msvc"))]
        {
            let faults = fault_trip_count();

            if panics + faults + bails == 0 {
                None
            }
            else {
                Some(format!(
                    "Hook barrier stopped {} detour call(s): {} panicked, {} faulted ({} of them a call through 0, C1), last fault code {:#010x} at {:p}; {} answered with the game's own value, {} coroutine door(s) taken down, {} answered with a value the barrier had to make up, {} where the wrapper's own stated answer tripped too",
                    panics + faults,
                    panics,
                    faults,
                    null_target_fault_trip_count(),
                    last_fault_code(),
                    last_fault_address(),
                    answered,
                    takendowns,
                    invented,
                    bails
                ))
            }
        }

        #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
        {
            if panics + bails == 0 {
                None
            }
            else {
                Some(format!(
                    "Hook barrier stopped {} detour call(s): {} panicked, no fault barrier on this target; {} answered with the game's own value, {} coroutine door(s) taken down, {} answered with a value the barrier had to make up, {} where the wrapper's own stated answer tripped too",
                    panics, panics, answered, takendowns, invented, bails
                ))
            }
        }
    }

    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    extern "C" {
        fn hachimi_guard_run(
            executor: unsafe extern "C" fn(*mut c_void),
            proc: *mut c_void,
            code: *mut u32,
            address: *mut *mut c_void,
        ) -> i32;
    }

    /// What the C frame calls: it turns the opaque pointer back into the closure the barrier
    /// was handed.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    unsafe extern "C" fn run_closure<F: FnMut()>(proc: *mut c_void) {
        unsafe { (&mut *proc.cast::<F>())() };
    }

    /// Runs `call` under the `__try`/`__except` frame. True when it ran to its end, false when
    /// the frame took a fault; `*code` and `*address` then say what the frame stopped.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    unsafe fn run_under_seh<F: FnMut()>(
        call: &mut F,
        code: &mut u32,
        address: &mut *mut c_void,
    ) -> bool {
        // SAFETY: `call` is a live local of the caller for the whole duration of the C frame,
        // and the frame only ever calls it through the pointer passed here.
        unsafe {
            hachimi_guard_run(
                run_closure::<F>,
                call as *mut F as *mut c_void,
                code as *mut u32,
                address as *mut *mut c_void,
            ) == 0
        }
    }

    /// The answer a body has already been given, held by the barrier.
    ///
    /// A body that has called the game's own method holds the game's answer in a local - and a
    /// local of the body is *gone* when the trip answers, because the C frame stops a fault by
    /// returning out of the closure frames it wrapped, and the frame the panic was caught in is
    /// on the same way out. So the body hands the answer over here: this cell is a local of
    /// `detour_barrier`, the frame a trip returns *through*.
    ///
    /// `publish` takes the value by value and writes it. A body that still needs it keeps its own
    /// copy, which is what every game value returned through a detour here is: a scalar or a
    /// pointer. The cell is a `MaybeUninit`, so a published value that never gets handed back is
    /// dropped by nobody - and a value published twice leaks the first rather than dropping it
    /// twice. A value with drop glue does not belong in this cell.
    pub struct GameAnswer<T> {
        value: MaybeUninit<T>,
        published: bool,
    }

    impl<T> GameAnswer<T> {
        const fn new() -> Self {
            Self { value: MaybeUninit::uninit(), published: false }
        }

        /// "The game's method answered, and this is what it answered." A trip later in the body
        /// answers with it. `#[inline(always)]`: this sits on the clean path of every wrapper that
        /// publishes, and it is two stores into the barrier's frame.
        #[inline(always)]
        pub fn publish(&mut self, value: T) {
            self.value.write(value);
            self.published = true;
        }
    }

    /// Why a detour body stopped, or what it returned. The wrapper has to know which of these it
    /// is answering, because they do not owe the game the same thing.
    ///
    /// A body that *panicked* was stopped by the mod's own code: the `this` and the arguments are
    /// the game's, untouched and intact, and the game's method never got them. The wrapper still
    /// owes the game the call it would have got with no mod installed, and `bail` is that call.
    ///
    /// A body that *faulted* was stopped by the state it was handed - a null or freed `this`, a bad
    /// argument, an index past an `Il2CppArray` - which is exactly the state the fallback would
    /// replay, so the same call into the same method faults again deterministically. It is also
    /// unknown how far that body got: a body that had already handed the call to the game faults
    /// while it is inside it, and a fallback is then a second entry into the same game method. A
    /// `Faulted` trip is therefore the one trip a `bail` may not answer with.
    ///
    /// `Answered` is neither of those two: the trip came *after* the game's method had returned,
    /// so the value the game produced is the answer, and no fallback is owed. Before it existed
    /// `detour_barrier` kept `value` in a `MaybeUninit` and only read it when `completed` was
    /// true - so a trip after a successful original call threw that value away and the macro could
    /// only ever answer with one blanket fallback for both trips.
    pub enum BarrierOutcome<R> {
        /// The body ran to its end; this is what it returned.
        Done(R),
        /// The body published the game's own answer and was stopped after that. Hand it back.
        Answered(R),
        /// `catch_unwind` took a Rust panic out of the body, before it published an answer.
        Panicked,
        /// The C frame took a structured exception out of the body, before it published an answer.
        Faulted,
    }

    /// Run a detour body so that neither a Rust panic nor a structured exception leaves it.
    ///
    /// The body is taken as `FnOnce` and owned here: a body that hands a by value parameter over
    /// to the game moves it out of whatever captured it, and an `FnMut` closure may not do that.
    /// It is handed the `GameAnswer` it may publish through.
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    #[inline(always)]
    pub fn detour_barrier<R, F: FnOnce(&mut GameAnswer<R>) -> R>(body: F) -> BarrierOutcome<R> {
        let mut body = Some(body);
        let mut answer = GameAnswer::<R>::new();
        let mut value = MaybeUninit::<R>::uninit();
        let mut completed = false;
        let mut code = 0u32;
        let mut address: *mut c_void = std::ptr::null_mut();

        let mut run = || {
            if let Some(call) = body.take() {
                value.write(call(&mut answer));
                completed = true;
            }
        };
        // `completed` is only ever written here, inside the frame the panic is caught in, so the
        // wrapper learns which half of the barrier stopped the body.
        let mut guarded = || match catch_unwind(AssertUnwindSafe(&mut run)) {
            Ok(()) => {}
            Err(_) => {}
        };

        if unsafe { run_under_seh(&mut guarded, &mut code, &mut address) } {
            if completed {
                // SAFETY: the guarded closure returned without faulting, so it wrote the value.
                return BarrierOutcome::Done(unsafe { value.assume_init() });
            }

            trip_panics();

            if answer.published {
                trip_answered();
                // SAFETY: `publish` wrote the cell before this body was stopped.
                return BarrierOutcome::Answered(unsafe { answer.value.assume_init() });
            }

            BarrierOutcome::Panicked
        } else {
            trip_faults(code, address);

            if answer.published {
                trip_answered();
                // SAFETY: as above - the fault came after the cell was written, and the write is
                // a store into a local of this frame.
                return BarrierOutcome::Answered(unsafe { answer.value.assume_init() });
            }

            BarrierOutcome::Faulted
        }
    }

    /// The `bail` arm, behind the barrier, for a wrapper that has nothing to answer with.
    ///
    /// A trip makes the fallback the only call left in the wrapper, so it cannot be the call that
    /// leaves the hook boundary unguarded: a fallback is where a wrapper reaches the game's own
    /// method, and that call can itself panic - the registry lock it reads is poisoned - or fault,
    /// which is what a trampoline copy the detach path has just sent back as 0 looks like (C1).
    ///
    /// A fallback that tripped answers with the zero value, which is only an answer a `void` wrapper
    /// may give: it is inventing nothing when there is nothing to answer with. A wrapper with a
    /// return type uses `detour_fallback_or` and hands this function's job to `invented_answer`, so
    /// the trip where its fallback could not answer is counted and named instead of handed over as a
    /// silent zero - which is what the value arms of this macro did to `Gallop.Screen::get_Width`, a
    /// number the game and `windows/utils.rs` divide by.
    ///
    /// `#[cold]`/`#[inline(never)]`: the body of a detour runs this only after the barrier stopped
    /// the body, and the barrier machinery behind it must not land in the wrapper the game calls.
    #[cold]
    #[inline(never)]
    pub fn detour_fallback<R, F: FnOnce(&mut GameAnswer<R>) -> R>(bail: F) -> R {
        detour_fallback_or(bail, || unsafe { ::std::mem::zeroed() })
    }

    /// The `bail` arm, or the `fallback` a value wrapper states, for a wrapper whose last answer is
    /// not a zero: `last_resort` is what the wrapper answers when the fallback tripped too. A
    /// coroutine door's `bail` is the game's own `MoveNext`, and `false` is not an answer that door
    /// may give from a trip - its last resort is `coroutine_trip`. A value wrapper that states no
    /// answer of its own hands the job to `invented_answer`.
    #[cold]
    #[inline(never)]
    pub fn detour_fallback_or<R, F: FnOnce(&mut GameAnswer<R>) -> R, G: FnOnce() -> R>(bail: F, last_resort: G) -> R {
        match detour_barrier(bail) {
            BarrierOutcome::Done(value) | BarrierOutcome::Answered(value) => value,
            BarrierOutcome::Panicked | BarrierOutcome::Faulted => {
                trip_bail();
                last_resort()
            }
        }
    }

    /// Tier three, and the only place left where the barrier answers with a value of its own making.
    ///
    /// The `extern "C"` boundary needs a value of the wrapper's return type and this trip had neither
    /// the game's answer nor anything the wrapper stated, so the zero value is what goes back - the
    /// same value those arms already returned. What changes is that it no longer goes back quietly:
    /// every such trip is counted for the detach report, and the first one a wrapper produces says so
    /// in the log, naming the wrapper and the type it had to fabricate. A run that reads
    /// "the barrier held" off the fault count and a game that divides by the number the barrier made
    /// up were the same event in the log before this; they are not the same event now.
    ///
    /// `first` is the latch the call site owns - a `static` the `def_detour!` arms declare inside the
    /// wrapper they build, started at that wrapper's own address for the same reason `UnresolvedMarker`
    /// is: latches that begin as the same bytes are what a linker folding identical data folds onto one
    /// another, and a folded latch is one line for a hundred wrappers. A null latch is a spent one. So
    /// the once-per-wrapper line costs one atomic swap on a path that is already cold - no lock, no
    /// allocation, no name registry.
    ///
    /// `#[cold]`/`#[inline(never)]`: never in the wrapper the game calls.
    #[cold]
    #[inline(never)]
    pub fn invented_answer<R>(name: &'static str, first: &AtomicPtr<()>) -> R {
        INVENTED_TRIPS.fetch_add(1, Ordering::Relaxed);

        if !first.swap(std::ptr::null_mut(), Ordering::AcqRel).is_null() {
            warn!(
                "Hook barrier had no answer for {}: it answered this call with a zero {}, and the game is the one that has to live with it; later trips of this wrapper are counted in the detach report",
                name,
                std::any::type_name::<R>()
            );
        }

        unsafe { ::std::mem::zeroed() }
    }

    /// The rule for a coroutine door - a detour on `MoveNext/0 -> bool()`, the method Unity drives
    /// a live coroutine through - that tripped before the game answered.
    ///
    /// `true`: this call does not end the coroutine. It is the one answer a door can give without
    /// inventing a finished coroutine, and it is not a fabrication of a game quantity - a driver
    /// that is told "still running" calls `MoveNext` again, and a state machine whose coroutine
    /// really did finish answers `false` to that call by itself. `false` is what the barrier used
    /// to answer, which is the sentence "this coroutine completed" handed to a coroutine that was
    /// still mid-run.
    ///
    /// A door that cannot answer is also taken out of the registry, so it cannot give this answer
    /// every frame: the next call on that enumerator is answered by the game's own `MoveNext`.
    /// That is a trip-path action - the registry lock is never taken on a clean call.
    ///
    /// `#[cold]`/`#[inline(never)]`: never in the wrapper the game calls.
    #[cold]
    #[inline(never)]
    pub fn coroutine_trip(wrapper: *const ()) -> bool {
        let hachimi = crate::core::Hachimi::instance();
        coroutine_trip_taken_down(&hachimi.interceptor, wrapper)
    }

    /// The door rule without the singleton, so a unit test drives it against its own registry
    /// (`Hachimi::instance()` ends the test process when a test asks for it - AGENTS section 4).
    #[cold]
    pub fn coroutine_trip_taken_down(interceptor: &Interceptor, wrapper: *const ()) -> bool {
        if interceptor.unhook(wrapper as usize).is_some() {
            COROUTINE_TAKEDOWNS.fetch_add(1, Ordering::Relaxed);
        }

        true
    }

    /// Android and any non-MSVC target: no SEH frame, so panics only, and no trip here is ever
    /// reported as a fault. The `Faulted` arm of a wrapper is unreachable on this target for the
    /// same reason the fault counters are: nothing on it can see a fault. The published answer is
    /// not target specific - a panic half-way through a body loses the game's value exactly the
    /// same way - so `Answered` works here too.
    #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
    #[inline(always)]
    pub fn detour_barrier<R, F: FnOnce(&mut GameAnswer<R>) -> R>(body: F) -> BarrierOutcome<R> {
        let mut body = Some(body);
        let mut answer = GameAnswer::<R>::new();
        let mut value = MaybeUninit::<R>::uninit();
        let mut completed = false;

        let mut run = || {
            if let Some(call) = body.take() {
                value.write(call(&mut answer));
                completed = true;
            }
        };

        match catch_unwind(AssertUnwindSafe(&mut run)) {
            Ok(()) if completed => {
                // SAFETY: the body ran to its end and wrote the value.
                BarrierOutcome::Done(unsafe { value.assume_init() })
            },
            _ => {
                trip_panics();

                if answer.published {
                    trip_answered();
                    // SAFETY: `publish` wrote the cell before the panic unwound out of the body.
                    BarrierOutcome::Answered(unsafe { answer.value.assume_init() })
                }
                else {
                    BarrierOutcome::Panicked
                }
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::core::interceptor::Interceptor;
        use std::sync::atomic::AtomicBool;

        static VOID_BAIL_RAN: AtomicBool = AtomicBool::new(false);

        // The turn is the guard module's, not this module's: the hook files whose macros now expand
        // into `def_detour!` count the same process-wide counters.
        fn counting() -> std::sync::MutexGuard<'static, ()> {
            super::barrier_turn()
        }

        // The shapes every wrapper in src/il2cpp/hook is written with.
        def_detour! { Panics(_this: *mut u64) -> *mut u64 { panic!("a detour body that blows up") } }
        def_detour! { PanicsWithBail(x: i32) -> i32 { panic!("a detour body that blows up") } bail { x + 1 } }
        def_detour! { PanicsVoid(_this: *mut u64) { panic!("a detour body that blows up") } bail { VOID_BAIL_RAN.store(true, Ordering::Relaxed); } }
        def_detour! { Doubles(mut duration: f32, steps: i32) { duration *= 2.0; let _ = steps; } }
        def_detour! { Scales(duration: f32) -> f32 { duration * 2.0 } bail { duration } }
        def_detour! { StopsEarly(kind: i32) -> i32 { if kind == 0 { return 7; } 1 } }

        #[test]
        fn a_clean_body_still_returns_its_value() {
            assert_eq!(Scales(2.0), 4.0);
            assert_eq!(StopsEarly(0), 7);
            assert_eq!(StopsEarly(1), 1);
            Doubles(1.5, 3);
        }

        #[test]
        fn a_panicking_body_does_not_reach_the_caller() {
            let _turn = counting();
            let before = panic_trip_count();

            // The way the trampoline reaches a detour: a plain function pointer call.
            let call: extern "C" fn(*mut u64) -> *mut u64 = Panics;
            assert!(call(std::ptr::null_mut()).is_null());

            assert_eq!(panic_trip_count(), before + 1);
        }

        #[test]
        fn a_trip_runs_the_bail_the_game_would_have_got() {
            let _turn = counting();
            let before = panic_trip_count();

            let call: extern "C" fn(i32) -> i32 = PanicsWithBail;
            assert_eq!(call(41), 42);

            let call: extern "C" fn(*mut u64) = PanicsVoid;
            call(std::ptr::null_mut());
            assert!(VOID_BAIL_RAN.load(Ordering::Relaxed), "the void bail ran");

            assert_eq!(panic_trip_count(), before + 2);
        }

        // The `prelude` arm: the same two trip answers as the plain void `bail` arm, plus something
        // the wrapper frame holds for the whole call. `DEPTH` stands for the state those wrappers
        // exist to hold - the Live secondary-camera depth at `LiveTimelineControl.rs` - and the two
        // wrappers below are the arm's two trips.
        static WRAPPER_GUARD_DEPTH: AtomicUsize = AtomicUsize::new(0);
        static DEPTH_THE_BAIL_SAW: AtomicUsize = AtomicUsize::new(0);
        static BAIL_RAN_WITH_A_PRELUDE: AtomicBool = AtomicBool::new(false);
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        static BAIL_RAN_AFTER_A_FAULT: AtomicBool = AtomicBool::new(false);

        struct HeldByTheWrapperFrame;

        impl Drop for HeldByTheWrapperFrame {
            fn drop(&mut self) {
                WRAPPER_GUARD_DEPTH.fetch_sub(1, Ordering::Relaxed);
            }
        }

        fn raise_the_wrapper_guard() -> HeldByTheWrapperFrame {
            WRAPPER_GUARD_DEPTH.fetch_add(1, Ordering::Relaxed);
            HeldByTheWrapperFrame
        }

        def_detour! { GuardedVoidPanicking(_this: *mut u64)
            prelude { raise_the_wrapper_guard() }
            { panic!("a detour body that blows up") }
            bail {
                DEPTH_THE_BAIL_SAW.store(WRAPPER_GUARD_DEPTH.load(Ordering::Relaxed), Ordering::Relaxed);
                BAIL_RAN_WITH_A_PRELUDE.store(true, Ordering::Relaxed);
            }
        }

        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            GuardedVoidFaulting(_this: *mut u64)
            prelude { raise_the_wrapper_guard() }
            {
                let mut value: u64 = 0;
                unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
                let _ = value;
            }
            bail { BAIL_RAN_AFTER_A_FAULT.store(true, Ordering::Relaxed); }
        }

        #[test]
        fn the_prelude_holds_the_guard_across_the_bail_and_drops_it_on_the_way_out() {
            let _turn = counting();
            let before_panics = panic_trip_count();

            let call: extern "C" fn(*mut u64) = GuardedVoidPanicking;
            call(std::ptr::null_mut());

            assert!(BAIL_RAN_WITH_A_PRELUDE.load(Ordering::Relaxed),
                "the arm did not answer a Panicked trip with the game's call, the way the plain \
                 void bail arm does");
            assert_eq!(panic_trip_count(), before_panics + 1);
            assert_eq!(DEPTH_THE_BAIL_SAW.load(Ordering::Relaxed), 1,
                "the replayed call ran outside the state the wrapper raised, so the game's own \
                 nested calls would have answered this call as if no mod were installed");
            assert_eq!(WRAPPER_GUARD_DEPTH.load(Ordering::Relaxed), 0,
                "the wrapper returned with the guard it holds still raised");
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_fault_under_a_prelude_releases_the_guard_and_replays_nothing() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_panics = panic_trip_count();

            let call: extern "C" fn(*mut u64) = GuardedVoidFaulting;
            call(std::ptr::null_mut());

            assert_eq!(fault_trip_count(), before_faults + 1, "the C frame took the fault");
            assert_eq!(last_fault_code(), 0xC0000005);
            assert_eq!(panic_trip_count(), before_panics);
            assert!(!BAIL_RAN_AFTER_A_FAULT.load(Ordering::Relaxed),
                "a Faulted trip replayed the call whose state just faulted");
            assert_eq!(WRAPPER_GUARD_DEPTH.load(Ordering::Relaxed), 0,
                "the guard was taken inside the frames the C frame abandoned");
        }

        #[test]
        fn a_swallowed_panic_leaves_rusts_own_unwinding_intact() {
            // The barrier catches the panic with Rust's own machinery. If the C frame had taken
            // it instead, std's panic bookkeeping would be left dirty and this catch would not
            // behave normally afterwards.
            let _turn = counting();
            let call: extern "C" fn(*mut u64) -> *mut u64 = Panics;
            call(std::ptr::null_mut());

            let later = catch_unwind(AssertUnwindSafe(|| panic!("a panic raised afterwards")));
            assert!(later.is_err(), "catch_unwind still works after a barrier trip");
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_faulting_body_does_not_take_the_process_down() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_panics = panic_trip_count();

            def_detour! { Faults(_this: *mut u64) -> u64 {
                let mut value: u64 = 0;
                // The read a detour takes when a `this` it trusted is already freed.
                unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
                value
            } }

            let call: extern "C" fn(*mut u64) -> u64 = Faults;
            assert_eq!(call(std::ptr::null_mut()), 0);
            assert_eq!(last_fault_code(), 0xC0000005, "the C frame stopped an access violation");
            assert_eq!(fault_trip_count(), before_faults + 1);
            assert_eq!(panic_trip_count(), before_panics);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_panic_is_counted_as_a_panic_not_as_a_fault() {
            let _turn = counting();
            let before_faults = fault_trip_count();

            let call: extern "C" fn(*mut u64) -> *mut u64 = Panics;
            call(std::ptr::null_mut());

            assert_eq!(fault_trip_count(), before_faults, "the C frame never saw an exception");
        }

        // The shape every one of these wrappers has now: the body calls the game's own method, gets
        // an answer, and then does the mod's half - which is where the trip happens. `answer` is the
        // binding the `answer ->` arm hands it, so the game's value outlives the body's frame.
        def_detour! { PublishesThenPanics(value: i32) answer -> i32 {
            answer.publish(value);
            panic!("the mod half blew up after the game had already answered")
        } }

        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            PublishesThenFaults(_this: *mut u64) answer -> u64 {
                answer.publish(0x5A5A);

                let mut junk: u64 = 0;
                unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") junk) };
                junk
            }
        }

        // A coroutine door that has the game's `MoveNext` answer in hand before it blows up. Both
        // answers are legal here - `true` is a coroutine that stepped, `false` is one that really
        // finished - and neither is the barrier's invention.
        def_detour! { DoorStillRunning(_enumerator: *mut u64) coroutine answer -> bool {
            answer.publish(true);
            panic!("the mod half blew up after the coroutine stepped")
        } }

        def_detour! { DoorFinished(_enumerator: *mut u64) coroutine answer -> bool {
            answer.publish(false);
            panic!("the mod half blew up after the coroutine finished")
        } }

        #[test]
        fn a_trip_after_the_game_answered_hands_that_answer_back() {
            let _turn = counting();
            let before_panics = panic_trip_count();
            let before_answered = answered_trip_count();

            let call: extern "C" fn(i32) -> i32 = PublishesThenPanics;
            assert_eq!(call(7), 7, "the wrapper returned what the game returned, not 0");

            assert_eq!(panic_trip_count(), before_panics + 1);
            assert_eq!(answered_trip_count(), before_answered + 1);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_fault_after_the_game_answered_hands_that_answer_back_too() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_answered = answered_trip_count();

            let call: extern "C" fn(*mut u64) -> u64 = PublishesThenFaults;
            assert_eq!(call(std::ptr::null_mut()), 0x5A5A);

            assert_eq!(fault_trip_count(), before_faults + 1);
            assert_eq!(answered_trip_count(), before_answered + 1);
        }

        #[test]
        fn a_fallback_that_published_before_it_tripped_is_an_answer_too() {
            let _turn = counting();
            let before_panics = panic_trip_count();
            let before_answered = answered_trip_count();

            let value = detour_fallback(|answer| {
                answer.publish(42u32);
                panic!("the call into the game tripped after it returned")
            });
            assert_eq!(value, 42);

            assert_eq!(panic_trip_count(), before_panics + 1);
            assert_eq!(answered_trip_count(), before_answered + 1);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_fallback_that_faulted_after_the_game_answered_hands_the_answer_back() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_answered = answered_trip_count();

            let value = detour_fallback(|answer| {
                answer.publish(9u32);
                let mut junk: u64 = 0;
                unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") junk) };
                junk as u32
            });
            assert_eq!(value, 9);

            assert_eq!(fault_trip_count(), before_faults + 1);
            assert_eq!(answered_trip_count(), before_answered + 1);
        }

        #[test]
        fn a_door_that_has_the_games_answer_answers_with_it_whether_or_not_the_coroutine_finished() {
            let _turn = counting();
            let before_answered = answered_trip_count();

            let running: extern "C" fn(*mut u64) -> bool = DoorStillRunning;
            let finished: extern "C" fn(*mut u64) -> bool = DoorFinished;

            // `true` is a coroutine that stepped and is not finished, and `false` is the game saying
            // it is: the door keeps both, so the barrier does not invent either one.
            assert!(running(std::ptr::null_mut()));
            assert!(!finished(std::ptr::null_mut()));

            assert_eq!(answered_trip_count(), before_answered + 2);
        }

        #[test]
        fn a_door_with_no_answer_says_the_coroutine_is_still_running_and_leaves_the_registry() {
            let _turn = counting();
            let before_takendowns = coroutine_takedown_count();

            extern "C" fn a_coroutine_door(_enumerator: *mut u64) -> bool { true }

            let interceptor = Interceptor::default();
            interceptor.record_hook(a_coroutine_door as usize, 0x1000);

            let answer = coroutine_trip_taken_down(&interceptor, a_coroutine_door as *const ());
            assert!(answer, "the barrier does not get to say a coroutine finished");

            assert_eq!(coroutine_takedown_count(), before_takendowns + 1);
            assert_eq!(interceptor.get_trampoline_addr(a_coroutine_door as usize), 0, "the door is gone: the next call on that enumerator is answered by the game's own MoveNext");
        }

        // The game method a `bail` calls. It reads a field out of the `this` it was handed the way a
        // Unity method reads one, so the null `this` that faulted in the body faults inside it too,
        // and it counts its calls: a test can see whether a wrapper replayed the game or not. Only
        // where a fault can be taken at all, which is where the tests that use it run.
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        static GAME_CALLS: AtomicUsize = AtomicUsize::new(0);

        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        #[inline(never)]
        extern "C" fn game_method(this: *mut u64) -> u64 {
            GAME_CALLS.fetch_add(1, Ordering::Relaxed);
            unsafe { core::ptr::read_volatile(this) }
        }

        // The shape 110 of the migrated wrappers have (Transform.rs:156-164, SafetyNet.rs:14,
        // Connecting.rs:9 and the rest): the guarded body *is* the call to the game's original, and
        // the `bail` repeats it with the same arguments.
        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            FaultsInsideOriginal(this: *mut u64) -> u64 { game_method(this) } bail { game_method(this) }
        }

        // The same wrappers where the body faults in the mod half first, so the game method never ran.
        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            FaultsInBodyWithBail(this: *mut u64) -> u64 {
                let mut value: u64 = 0;
                unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
                game_method(this) + value
            }
            bail { game_method(this) }
        }

        // The reported problem (fix report round 3, problem 1) and the two ways a wrapper states the
        // answer it gives when it cannot answer with the game's. `Gallop.Screen::get_Width` is the live
        // site: its own comment says "this width is a divisor where the game and `windows/utils.rs` use
        // it, so 0 is not an answer that can be passed on", and it wrote that answer as a `bail` - which
        // is the *call the game would have got*, the one thing a `Faulted` may not replay - so the answer
        // the wrapper stated was thrown away and the game was handed 0. 1080 below is what the wrapper
        // says the answer is.
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        const UNITY_WIDTH: i32 = 1080;

        /// What a detach leaves behind: the address a `new_hook!` resolved, back at 0.
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        static TRAMPOLINE_LEFT_BY_THE_DETACH: AtomicUsize = AtomicUsize::new(0);

        /// The fault a wrapper takes when the state it was handed is the bad half - a read through a
        /// pointer nobody owns. The C frame reports it at the address of the instruction, inside this
        /// module, which is what separates it from the chain below.
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        #[inline(never)]
        fn read_the_null_page() -> i32 {
            let mut value: i32 = 0;
            unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
            value
        }

        /// The chain the reported problem names: `get_orig_fn!` answering 0 for a trampoline the detach
        /// path has taken back (C1), the site transmuting the address it read into a function pointer,
        /// the body calling it, and the access violation that follows.
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        #[inline(never)]
        fn call_through_a_taken_back_trampoline() -> i32 {
            let addr = TRAMPOLINE_LEFT_BY_THE_DETACH.load(Ordering::Relaxed);
            let orig: extern "C" fn() -> i32 = unsafe { ::std::mem::transmute(addr) };
            orig()
        }

        // `get_Width` as the item found it: its safe answer written as a `bail`.
        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            WidthWithItsAnswerWrittenAsABail(_this: *mut u64) -> i32 {
                call_through_a_taken_back_trampoline()
            } bail { UNITY_WIDTH }
        }

        // The same wrapper with that answer written as what it always was: the value it answers with.
        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            WidthWithItsAnswerStated(_this: *mut u64) -> i32 {
                call_through_a_taken_back_trampoline()
            } fallback { UNITY_WIDTH }
        }

        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            WidthWithNoAnswerWritten(_this: *mut u64) -> i32 {
                call_through_a_taken_back_trampoline()
            }
        }

        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            WidthWhoseStatedAnswerFaultsToo(_this: *mut u64) -> i32 {
                call_through_a_taken_back_trampoline()
            } fallback { read_the_null_page() }
        }

        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            WidthPanickingWithAStatedAnswer(_this: *mut u64) -> i32 {
                panic!("the mod half blew up before it reached the game")
            } fallback { UNITY_WIDTH }
        }

        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            WidthReadThroughAFreedPointerWithAStatedAnswer(_this: *mut u64) -> i32 {
                read_the_null_page()
            } fallback { UNITY_WIDTH }
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn the_c1_chain_faults_at_address_0_and_the_barrier_says_so() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_null_targets = null_target_fault_trip_count();

            let call: extern "C" fn(*mut u64) -> i32 = WidthWithNoAnswerWritten;
            call(std::ptr::null_mut());

            assert_eq!(fault_trip_count(), before_faults + 1);
            assert_eq!(last_fault_code(), 0xC0000005);
            assert!(last_fault_address().is_null(), "a call through 0 faults at address 0, which is what C1 looks like to the frame");
            assert_eq!(null_target_fault_trip_count(), before_null_targets + 1);

            // And the other half of the fault count stays where it belongs: a read through a pointer
            // the wrapper trusted faults at an instruction inside the module, not at 0.
            let read: extern "C" fn(*mut u64) -> i32 = WidthReadThroughAFreedPointerWithAStatedAnswer;
            read(std::ptr::null_mut());
            assert_eq!(fault_trip_count(), before_faults + 2);
            assert_eq!(null_target_fault_trip_count(), before_null_targets + 1, "a read fault is not a call through 0");
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_fault_answers_the_value_the_wrapper_stated_as_safe() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_bails = bail_trip_count();
            let before_invented = invented_trip_count();

            // The chain the item named, on the wrapper whose stated answer is what Unity reports:
            // `get_orig_fn!` answered 0, the body called through it, the barrier took the fault, and
            // the game is handed 1080 rather than a 0 it would go on to divide by.
            let call: extern "C" fn(*mut u64) -> i32 = WidthWithItsAnswerStated;
            assert_eq!(call(std::ptr::null_mut()), UNITY_WIDTH, "a faulted get_Width answers what its wrapper stated, not 0");

            assert_eq!(fault_trip_count(), before_faults + 1);
            assert_eq!(bail_trip_count(), before_bails, "the stated answer ran, and did not trip");
            assert_eq!(invented_trip_count(), before_invented, "the barrier invented nothing here");
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_stated_answer_answers_the_bad_state_a_fault_came_from_too() {
            // Not the C1 chain but the other trip this answer is for: the wrapper was handed a freed
            // object. The stated value does not touch that state, so it stands, and the game's method
            // is not entered a second time (see `a_fault_does_not_replay_the_arguments_into_the_game_method`).
            let _turn = counting();
            let before_faults = fault_trip_count();

            let call: extern "C" fn(*mut u64) -> i32 = WidthReadThroughAFreedPointerWithAStatedAnswer;
            assert_eq!(call(std::ptr::null_mut()), UNITY_WIDTH);

            assert_eq!(fault_trip_count(), before_faults + 1);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_stated_answer_answers_a_panicked_trip_as_well() {
            let _turn = counting();
            let before_panics = panic_trip_count();
            let before_invented = invented_trip_count();

            let call: extern "C" fn(*mut u64) -> i32 = WidthPanickingWithAStatedAnswer;
            assert_eq!(call(std::ptr::null_mut()), UNITY_WIDTH);

            assert_eq!(panic_trip_count(), before_panics + 1);
            assert_eq!(invented_trip_count(), before_invented);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_trip_the_wrapper_stated_no_answer_for_is_counted_as_one_the_barrier_invented() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_invented = invented_trip_count();

            // The same chain, on a wrapper that wrote no rule: the zero value the arm already gave
            // (it still has to hand back something of `i32`), and the count that stops it being
            // silent wrong data.
            let call: extern "C" fn(*mut u64) -> i32 = WidthWithNoAnswerWritten;
            assert_eq!(call(std::ptr::null_mut()), 0);
            assert_eq!(invented_trip_count(), before_invented + 1);

            // Same for a `bail` the fault may not replay: the answer the wrapper wrote is a call, the
            // trip may not run it, so what is left is a refusal and a count, not the wrapper's value.
            let bail: extern "C" fn(*mut u64) -> i32 = WidthWithItsAnswerWrittenAsABail;
            assert_eq!(bail(std::ptr::null_mut()), 0, "a `bail` is not a value a fault may hand over");
            assert_eq!(invented_trip_count(), before_invented + 2);
            assert_eq!(fault_trip_count(), before_faults + 2);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_stated_answer_that_cannot_answer_either_is_counted_twice() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_bails = bail_trip_count();
            let before_invented = invented_trip_count();

            let call: extern "C" fn(*mut u64) -> i32 = WidthWhoseStatedAnswerFaultsToo;
            assert_eq!(call(std::ptr::null_mut()), 0);

            assert_eq!(fault_trip_count(), before_faults + 2, "the body and the stated answer each faulted");
            assert_eq!(bail_trip_count(), before_bails + 1, "the fallback the barrier had to stop");
            assert_eq!(invented_trip_count(), before_invented + 1);
        }

        // The shape a fallback has when the trampoline it reads is gone: the body panicked (so the
        // replay is the right answer) and the replay itself faults - `get_orig_fn!` answering 0, C1.
        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            PanicsWithFaultingBail(this: *mut u64) -> u64 { panic!("a detour body that blows up") } bail { game_method(this) }
        }
        def_detour! {
            #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
            PanicsVoidFaultingBail(this: *mut u64) { panic!("a detour body that blows up") } bail { let _ = game_method(this); }
        }
        def_detour! { PanicsWithPanickingBail(_x: i32) -> i32 { panic!("a detour body that blows up") } bail { panic!("a bail that blows up as well") } }

        // The same wrapper with a stated `fallback` that runs: the pair the detach report has to tell
        // apart, a fallback that could not answer and one that answered.
        def_detour! { PanicsWithItsAnswerIntact(_x: i32) -> i32 { panic!("a detour body that blows up") } fallback { 7 } }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_fault_does_not_replay_the_arguments_into_the_game_method() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_panics = panic_trip_count();
            let mut before_game = GAME_CALLS.load(Ordering::Relaxed);

            // Body = the game's method, faulting on a freed `this`. The fallback is the same call
            // with the same `this`, so it is not run: the wrapper answers with the zero value and
            // the game method is entered exactly once (the body's attempt), not twice.
            let call: extern "C" fn(*mut u64) -> u64 = FaultsInsideOriginal;
            assert_eq!(call(std::ptr::null_mut()), 0, "a fault is answered with the zero value");
            before_game += 1;
            assert_eq!(GAME_CALLS.load(Ordering::Relaxed), before_game, "the bail did not enter the game method again");

            // And when the body faults before the game method ever ran: still no replay, because the
            // `this` the bail would hand over is the one that just faulted.
            let call: extern "C" fn(*mut u64) -> u64 = FaultsInBodyWithBail;
            assert_eq!(call(std::ptr::null_mut()), 0);
            assert_eq!(GAME_CALLS.load(Ordering::Relaxed), before_game, "the bail did not reach the game method at all");

            assert_eq!(fault_trip_count(), before_faults + 2, "both faults were taken by the barrier");
            assert_eq!(panic_trip_count(), before_panics);
            assert_eq!(last_fault_code(), 0xC0000005);
        }

        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn a_fallback_runs_behind_the_barrier_too() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_panics = panic_trip_count();
            let before_game = GAME_CALLS.load(Ordering::Relaxed);

            // A panicked body: the arguments are the game's own, so the fallback does reach the game
            // method - and when that call faults, the barrier is what stops it. Before this shape was
            // fixed, this is the call that had no guard in front of it and ended the process.
            let call: extern "C" fn(*mut u64) -> u64 = PanicsWithFaultingBail;
            assert_eq!(call(std::ptr::null_mut()), 0, "a fallback that faults is answered with the zero value");

            let call: extern "C" fn(*mut u64) = PanicsVoidFaultingBail;
            call(std::ptr::null_mut());

            assert_eq!(GAME_CALLS.load(Ordering::Relaxed), before_game + 2, "both fallbacks reached the game method");
            assert_eq!(panic_trip_count(), before_panics + 2, "one per body that panicked");
            assert_eq!(fault_trip_count(), before_faults + 2, "one per fallback that faulted");
            assert_eq!(last_fault_code(), 0xC0000005);
        }

        #[test]
        fn a_fallback_that_panics_is_stopped_as_well() {
            let _turn = counting();
            let before = panic_trip_count();

            let call: extern "C" fn(i32) -> i32 = PanicsWithPanickingBail;
            assert_eq!(call(41), 0, "the wrapper still returns, through an extern \"C\" boundary");
            assert_eq!(panic_trip_count(), before + 2, "the body and the fallback each tripped");
        }

        /// The tier-3 claim the item states - that a run can tell "the wrapper's stated answer ran"
        /// from "the stated answer itself could not answer" - is a claim about the detach line, so it
        /// is checked against the line the shipped `report_trips` prints (`src/windows/main.rs:84`)
        /// rather than against a counter no log reads. Both wrappers are panic-only shapes, so this
        /// runs on every target, including the one with no SEH frame.
        #[test]
        fn the_detach_report_names_the_stated_answer_that_could_not_answer() {
            let _turn = counting();
            let before_panics = panic_trip_count();
            let before_invented = invented_trip_count();
            let before_bails = bail_trip_count();

            // Tier three's last hop: the wrapper stated an answer, the barrier ran that answer behind
            // itself, the answer tripped, and the barrier's refusal is what the game was handed.
            let tripped: extern "C" fn(i32) -> i32 = PanicsWithPanickingBail;
            assert_eq!(tripped(41), 0, "the stated answer tripped, so a refusal is what came back");

            // The other half of the same sentence: a stated answer that ran is not a failure, and it
            // is not counted as one either.
            let intact: extern "C" fn(i32) -> i32 = PanicsWithItsAnswerIntact;
            assert_eq!(intact(41), 7, "the stated answer answered");

            assert_eq!(bail_trip_count(), before_bails + 1, "one wrapper's own answer could not answer");
            assert_eq!(invented_trip_count(), before_invented + 1, "and that ended in the barrier's refusal");
            assert_eq!(panic_trip_count(), before_panics + 3, "two bodies and the fallback that tripped");

            let line = trip_report().expect("a trip was taken, so the detach report has something to say");

            // The numbers in the line are the counters as they stand, and the field is the one the
            // claim names. Each field is matched from its delimiter outward, so a line carrying a
            // different number cannot pass by containing the digits: `5 panicked` is inside
            // `15 panicked`, `, 5 panicked` is not. Before this, `BAIL_TRIPS` was bumped, asserted
            // here, and printed nowhere: no run could see it at all.
            assert!(line.contains(&format!(": {} panicked", panic_trip_count())), "the line is not the barrier's: {}", line);
            assert!(line.contains(&format!(", {} answered with a value the barrier had to make up", invented_trip_count())),
                "the line does not name the answer the barrier invented: {}", line);
            assert!(line.contains(&format!(", {} where the wrapper's own stated answer tripped too", bail_trip_count())),
                "the line does not name the stated answer that could not answer: {}", line);
        }

        /// The same field on the fault half of the line, which is the shape the C1 chain reaches:
        /// the body calls through a trampoline the detach path took back, that call faults, and so
        /// does the answer the wrapper stated for exactly that case.
        #[test]
        #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
        fn the_detach_report_names_a_stated_answer_that_faulted_too() {
            let _turn = counting();
            let before_faults = fault_trip_count();
            let before_invented = invented_trip_count();
            let before_bails = bail_trip_count();

            let call: extern "C" fn(*mut u64) -> i32 = WidthWhoseStatedAnswerFaultsToo;
            assert_eq!(call(std::ptr::null_mut()), 0, "body and stated answer both faulted");

            assert_eq!(fault_trip_count(), before_faults + 2, "the body and the stated answer each faulted");
            assert_eq!(bail_trip_count(), before_bails + 1);
            assert_eq!(invented_trip_count(), before_invented + 1);

            let line = trip_report().expect("the barrier stopped faults, so the detach report has something to say");

            assert!(line.contains(&format!(": {} panicked", panic_trip_count())), "the line is not the barrier's: {}", line);
            assert!(line.contains(&format!(", {} faulted", fault_trip_count())), "the line does not name the faults: {}", line);
            assert!(line.contains(&format!(", {} where the wrapper's own stated answer tripped too", bail_trip_count())),
                "the fault half of the line does not name the stated answer that could not answer: {}", line);
        }
    }
}

pub mod mscorlib;

pub mod UnityEngine_CoreModule;
pub mod UnityEngine_AssetBundleModule;
pub mod UnityEngine_TextRenderingModule;
pub mod UnityEngine_ImageConversionModule;
pub mod Unity_RenderPipelines_Universal_Runtime;
pub mod UnityEngine_UI;
pub mod UnityEngine_UIModule;
pub mod Unity_TextMeshPro;

#[cfg(target_os = "windows")]
pub mod UnityEngine_InputLegacyModule;
#[cfg(target_os = "windows")]
pub mod Unity_InputSystem;

pub mod LibNative_Runtime;
pub mod umamusume;
pub mod Cute_UI_Assembly;
pub mod Plugins;
pub mod Cute_Cri_Assembly;
pub mod CriMw_CriWare_Runtime;
mod DOTween;

#[cfg(target_os = "android")]
mod Cute_Core_Assembly;

pub fn init() {
    info!("Initializing il2cpp hooks");

    // One line naming every knob that changes timing. The mod rewrites config.json on
    // exit, so a run has to carry the values it actually ran with or the log cannot be
    // read against the settings.
    {
        let config = crate::core::Hachimi::instance().config.load();

        // The unfocused cap is the one Performance knob that does not exist on every platform:
        // its storage and the code that honours it sit behind cfg(target_os = "windows"), as
        // does the tab row, so the log names it only where the option is reachable. It travels
        // as a finished fragment because a format string is a literal and cannot hold a
        // cfg'd placeholder.
        #[cfg(target_os = "windows")]
        let target_fps_unfocused = format!(" target_fps_unfocused {}", config.windows.target_fps_unfocused.unwrap_or(-1));
        #[cfg(not(target_os = "windows"))]
        let target_fps_unfocused = "";

        info!(
            "Config snapshot: transition {} result {} story {} ui_animation {} time_scale {} story_tcps {} choice_delay {} target_fps {}{} auto_skip_result {} high_speed_settings {} story_high_speed {} hide_now_loading {} physics {:?} cyspring_mono_uncap_frame_scale {}",
            config.transition_speed,
            config.result_screen_speed,
            config.story_speed,
            config.ui_animation_scale,
            config.time_scale,
            config.story_tcps_multiplier,
            config.story_choice_auto_select_delay,
            config.target_fps.unwrap_or(-1),
            target_fps_unfocused,
            config.auto_skip_result_screens,
            config.high_speed_settings,
            config.story_high_speed_mode,
            config.hide_now_loading,
            config.physics_update_mode,
            config.cyspring_mono_uncap_frame_scale
        );
    }

    // Arming one hook at a time measured 24 ms per hook in the run log, which was most of
    // the gap between these two lines. No module below calls through a hook it installs, so
    // everything can be created first and armed in a single pass at the end.
    let interceptor = &crate::core::Hachimi::instance().interceptor;
    interceptor.begin_batch();
    let hooking_started = std::time::Instant::now();

    // C# / .NET
    mscorlib::init();

    // Unity
    UnityEngine_AssetBundleModule::init();
    UnityEngine_CoreModule::init();
    UnityEngine_TextRenderingModule::init();
    UnityEngine_ImageConversionModule::init();

    Unity_RenderPipelines_Universal_Runtime::init();
    UnityEngine_UI::init();
    UnityEngine_UIModule::init();
    Unity_TextMeshPro::init();

    #[cfg(target_os = "windows")]
    {
        UnityEngine_InputLegacyModule::init();
        Unity_InputSystem::init();
    }

    // Umamusume
    LibNative_Runtime::init();
    umamusume::init();
    Cute_UI_Assembly::init();
    Plugins::init();
    Cute_Cri_Assembly::init();
    CriMw_CriWare_Runtime::init();
    DOTween::init();

    #[cfg(target_os = "android")]
    Cute_Core_Assembly::init();

    let armed = interceptor.finish_batch();
    info!(
        "Hooking finished: {} hooks armed in one pass, {:.3} s",
        armed,
        hooking_started.elapsed().as_secs_f32()
    );

    // debug_mode only: writes the game's own method and field names to
    // <data dir>/introspect.log so hooks can be aimed at real names.
    crate::il2cpp::introspect::dump_if_enabled();
}

#[cfg(test)]
mod tests {
    // C1: these two macros are the shape every `def_method_wrapper_fn!` / `impl_addr_wrapper_fn!`
    // site in `il2cpp/hook/` has. A run that wrote `X_addr is null` left that site's address at 0,
    // and mod code - the GUI, `il2cpp::sql.rs`, `il2cpp::ext.rs`, the IPC plane - still calls into
    // them, which is why "inert" and "quiet" both have to hold here.
    use super::UnresolvedMarker;

    // Tags the linker cannot merge onto each other, the way a wrapper's own address keeps one
    // macro's marker from landing on another wrapper's.
    extern "C" fn marker_owner(_this: *mut u8) {}
    extern "C" fn other_marker_owner(_this: *mut u8) {}

    def_method_wrapper_fn!(probe_wrapper, PROBE_WRAPPER_ADDR, i32, input: i32);

    static mut PROBE_IMPL_ADDR: usize = 0;
    impl_addr_wrapper_fn!(probe_impl_wrapper, PROBE_IMPL_ADDR, *mut u8,);

    #[test]
    fn a_wrapper_with_no_resolved_target_never_calls_through_zero() {
        // `init` never ran for these two, which is what a run that logged `... is null` leaves
        // behind. The answer has to be a value the caller can live with, not a jump.
        unsafe {
            PROBE_WRAPPER_ADDR = 0;
            PROBE_IMPL_ADDR = 0;
        }

        for call in 0..10_000 {
            assert_eq!(probe_wrapper(call), 0, "the wrapper called through address 0");
            assert!(probe_impl_wrapper().is_null(), "the wrapper called through address 0");
        }
    }

    #[test]
    fn an_unresolved_wrapper_says_it_once_however_often_it_is_called() {
        // The first call that finds the target unresolved says it; the 10,000 after it say nothing.
        // A `warn!` per call is what AGENTS section 6 forbids on a path a feature can reach every
        // frame - a GUI knob, a tween tick detour, the translation pass.
        let marker = UnresolvedMarker::new(marker_owner as *const ());

        let mut said = 0;
        for _ in 0..10_000 {
            if marker.warn_once("probe_wrapper") {
                said += 1;
            }
        }
        assert_eq!(said, 1, "an unresolved wrapper said it {said} times over 10,000 calls");

        // Every wrapper starts with its own marker unspent, which is the part the tag buys: two
        // macros' markers must not fold onto one another, or the second wrapper's reason goes
        // missing while both stay inert.
        assert!(UnresolvedMarker::new(marker_owner as *const ()).warn_once("fresh"), "a fresh marker arrived spent");
        assert!(UnresolvedMarker::new(other_marker_owner as *const ()).warn_once("fresh"), "a fresh marker arrived spent");
    }
}
