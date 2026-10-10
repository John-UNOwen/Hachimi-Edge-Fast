use std::sync::Mutex;
use fnv::FnvHashMap;
use once_cell::sync::Lazy;
use crate::core::sugoi_client::{SugoiClient, StringInfo};
use crate::il2cpp::{ext::{Il2CppStringExt, StringExt}, symbols::{get_method_addr, GCHandle}, types::*};

pub static ACTIVE_TEXT_MESH_COMPONENTS: Lazy<Mutex<FnvHashMap<usize, StringInfo>>> = Lazy::new(|| {
    Mutex::new(FnvHashMap::default())
});

type SetTextFn = extern "C" fn(this: *mut Il2CppObject, value: *mut Il2CppString);
def_detour! {
    pub set_text_hook(this: *mut Il2CppObject, value: *mut Il2CppString) {
            if value.is_null() {
            return get_orig_fn!(set_text_hook, SetTextFn)(this, value);
        }

        let config = crate::core::Hachimi::instance().config.load();
        if !config.auto_translate_localize && !config.auto_translate_stories {
            return get_orig_fn!(set_text_hook, SetTextFn)(this, value);
        }

        let orig_str = unsafe { (*value).as_utf16str().to_string() };
        let str_info = StringInfo {
            str_handle: GCHandle::new_weak_ref(this, false),
            str: orig_str.clone()
        };

        // C2: a poisoned registry answers instead of panicking inside this `extern "C"` frame.
        ACTIVE_TEXT_MESH_COMPONENTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(this as usize, str_info);

        if let Some(trans) = SugoiClient::instance().get_cached(&orig_str) {
            return get_orig_fn!(set_text_hook, SetTextFn)(this, trans.to_il2cpp_string());
        }

        get_orig_fn!(set_text_hook, SetTextFn)(this, value);
    }
}

pub fn apply_translations(completed: &[(String, String)]) {
    // The translation pass pushing a finished string into every live TextMesh, from mod code
    // rather than from this hook's trampoline.
    //
    // Same rule as `Text::apply_translations` (C11): every value the pass calls with is derived at
    // that one write - the component from the entry's weak handle, the string from a strong handle
    // held for the length of the pass, the trampoline from this wrapper's copy - and none of them
    // is an address the pass carried in from before the walk. With the hook not installed the pass
    // has nothing to write (C1).
    crate::core::sugoi_client::apply_translation_pass(
        &ACTIVE_TEXT_MESH_COMPONENTS,
        completed,
        |translated| GCHandle::new(translated.to_il2cpp_string() as *mut Il2CppObject, false),
        |component: *mut Il2CppObject, text: &GCHandle| {
            let Some(set_text) = get_orig_fn_guarded!(set_text_hook, SetTextFn) else { return };

            set_text(component, text.target() as *mut Il2CppString);
        }
    );
}

pub fn init(UnityEngine_TextRenderingModule: *const Il2CppImage) {
    get_class_or_return!(UnityEngine_TextRenderingModule, UnityEngine, TextMesh);

    let set_text_addr = get_method_addr(TextMesh, c"set_text", 1);
    new_hook!(set_text_addr, set_text_hook);
}