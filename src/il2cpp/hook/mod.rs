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

// shorter ver of doing impl_addr_wrapper_fn!()
macro_rules! def_method_wrapper_fn {
    ($name:tt, $addr:ident, $ret:ty, $($v:ident: $t:ty),*) => {
        static mut $addr: usize = 0;
        pub fn $name($($v: $t),*) -> $ret {
            // Reached from mod code as well as from a trampoline, so an unresolved target
            // is reachable on an ordinary feature path. Jumping to 0 is not recoverable.
            let addr = unsafe { $addr };

            if addr == 0 {
                warn!("{}: target address is unresolved, call skipped", stringify!($name));
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
            let addr = unsafe { $addr };

            if addr == 0 {
                warn!("{}: target address is unresolved, call skipped", stringify!($name));
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

        info!(
            "Config snapshot: transition {} result {} story {} ui_animation {} time_scale {} story_tcps {} choice_delay {} target_fps {} auto_skip_result {} high_speed_settings {} story_high_speed {} skip_scale {} hide_now_loading {} physics {:?}",
            config.transition_speed,
            config.result_screen_speed,
            config.story_speed,
            config.ui_animation_scale,
            config.time_scale,
            config.story_tcps_multiplier,
            config.story_choice_auto_select_delay,
            config.target_fps.unwrap_or(-1),
            config.auto_skip_result_screens,
            config.high_speed_settings,
            config.story_high_speed_mode,
            config.story_skip_frame_scale,
            config.hide_now_loading,
            config.physics_update_mode
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
