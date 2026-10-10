use crate::{core::Hachimi, il2cpp::{ext::{Il2CppStringExt, StringExt}, symbols::get_method_addr, types::*}};

type GetCaptionTextFn = extern "C" fn(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppString;
def_detour! {
    GetCaptionText(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppString {
            let text = get_orig_fn!(GetCaptionText, GetCaptionTextFn)(this, info);

        // C9: `text` is the string the game's own caption getter just answered with, and a plate
        // with no caption text is a legitimate null answer. There is no string to look for a
        // template in, and the game's own value is what the caller is owed.
        if text.is_null() {
            return text;
        }

        let text_utf16 = unsafe { (*text).as_utf16str() };

        // doesn't run through TextGenerator, remove filters
        if text_utf16.as_slice().contains(&36) { // 36 = dollar sign ($)
            Hachimi::instance().template_parser
                .remove_filters(&text_utf16.to_string())
                .to_il2cpp_string()
        }
        else {
            text
        }
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, TrainingParamChangeA2U);

    let GetCaptionText_addr = get_method_addr(TrainingParamChangeA2U, c"GetCaptionText", 1);

    new_hook!(GetCaptionText_addr, GetCaptionText);
}