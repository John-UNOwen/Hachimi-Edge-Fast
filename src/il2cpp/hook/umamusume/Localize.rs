use std::{cell::LazyCell, collections::{hash_map::Entry, BTreeMap}};

use fnv::FnvHashMap;

use crate::{
    core::{utils, Hachimi, SugoiClient, game::Region},
    il2cpp::{ext::{Il2CppStringExt, StringExt}, symbols::{get_method_overload_addr, unbox}, types::*}
};

use super::TextId;

// SAFETY: Localize::Get is only called from the Unity main thread.
static mut TEXTID_NAME_CACHE: LazyCell<FnvHashMap<i32, String>> = LazyCell::new(|| FnvHashMap::default());

/**
 * Gallop::Localize::Get
 * Used by the game to get localized strings for builtin text (mostly UI).
 * 
 * id is a value of the TextId enum
 * cy devs likes to insert stuff at arbitrary locations within the enum, changing their values
 * so we'll just map them to their actual name instead
 */
type GetFn = extern "C" fn(id: i32) -> *mut Il2CppString;
def_detour! {
    pub Get(id: i32) -> *mut Il2CppString {
            let hachimi = Hachimi::instance();
        let localized_data = hachimi.localized_data.load();
        if localized_data.localize_dict.is_empty() {
            // `Get` is a mod entry point as well as a detour: `core::utils::get_localized_string`
            // and the template filters in `SingleModeUtils` call this body directly, so it runs on
            // paths that are not this hook's own trampoline. `init` may have installed nothing (the
            // nested `Localize::JP` class is region dependent), and then there is no original to
            // call. C1: null is the answer, never address 0.
            let Some(get_orig) = get_orig_fn_guarded!(Get, GetFn) else { return std::ptr::null_mut() };

            return get_orig(id);
        }

        let name = match unsafe { TEXTID_NAME_CACHE.entry(id) } {
            Entry::Occupied(e) => &*e.into_mut(),
            Entry::Vacant(e) => {
                let name = TextId::get_name(id);

                // C9: the name is whatever the game's own `TextId.GetName` answered, and an id this
                // client does not have is a legitimate null. The cache keeps an empty name for it,
                // which matches nothing in the localize dict and the call falls through to the
                // game's own `Get` - the same result a name nobody translated gets.
                let name_str = if name.is_null() {
                    String::new()
                }
                else {
                    unsafe { (*name).as_utf16str().to_string() }
                };

                e.insert(name_str)
            },
        };

        if let Some(text) = localized_data.localize_dict.get(name) {
            text.to_il2cpp_string()
        }
        else {
            let Some(get_orig) = get_orig_fn_guarded!(Get, GetFn) else {
                return std::ptr::null_mut();
            };

            let str = get_orig(id);
            if Hachimi::instance().config.load().translator_mode && id != 1109 && id != 1032 {
                // 1109 and 1032 seems to be debugging strings (they're annoying)
                // C9: the game's `Get` answers null for an id it has no string for, and that is the
                // answer rather than something to print.
                if !str.is_null() {
                    utils::print_json_entry(name, unsafe { &(*str).as_utf16str().to_string() });
                }
            }
            if hachimi.config.load().auto_translate_localize && !str.is_null() && unsafe { (*str).length > 0 } {
                let s = unsafe { (*str).as_utf16str().to_string() };

                let sugoi = SugoiClient::instance();
                if let Some(translated) = sugoi.get_cached(&s) {
                    return translated.to_il2cpp_string();
                } else {
                    sugoi.translate_async(s);
                }
            }
            str
        }
    }
}

pub fn dump_strings() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();

    // A GUI action walking every TextId, so this is mod code calling the game's `Get` about ten
    // thousand times, not a detour reading its own trampoline. Resolve once, and if `Get` is not
    // installed - `init` aims it at `Localize::JP`, a nested class the client may not have - the
    // dump is empty rather than a call through 0 in a loop (C1).
    let Some(get_orig) = get_orig_fn_guarded!(Get, GetFn) else { return map };

    for obj in TextId::get_values().enumerator().map(|e| e.iter()).unwrap_or_default().expect("enum values enumerator") {
        let value: i32 = unsafe { unbox(obj) };
        let name = TextId::get_name(value);

        // C9: the name is the game's answer for this enum value, and an id it has no name for is a
        // legitimate null. There is no key to file this entry under, so the value is skipped.
        if name.is_null() {
            continue;
        }

        let name_str = unsafe { (*name).as_utf16str() };

        let res = get_orig(value);
        if !res.is_null() {
            let res_str = unsafe { (*res).as_utf16str() };
            map.insert(name_str.to_string(), res_str.to_string());
        }
    }

    map
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, Localize);

    let Get_addr = if Hachimi::instance().game.region == Region::Taiwan {
        get_method_overload_addr(Localize, "Get", &[Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE])
    } else {
        find_nested_class_or_return!(Localize, JP);
        get_method_overload_addr(JP, "Get", &[Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE])
    };

    new_hook!(Get_addr, Get);
}