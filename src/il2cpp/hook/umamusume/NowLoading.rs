use crate::{core::{utils::truncate_text_il2cpp, Hachimi}, il2cpp::{hook::umamusume::AnimationSpeed, hook::UnityEngine_UI::Text, symbols::{get_field_from_name, get_field_object_value, get_method_addr}, types::*}};

static mut _COMICTITLE_FIELD: *mut FieldInfo = 0 as _;
fn get__comicTitle(this: *mut Il2CppObject) -> *mut Il2CppObject {
    get_field_object_value(this, unsafe { _COMICTITLE_FIELD })
}

const COMIC_TITLE_LINE_WIDTH: usize = 23;

type SetupLoadingTipsFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn SetupLoadingTips(this: *mut Il2CppObject) {
    get_orig_fn!(SetupLoadingTips, SetupLoadingTipsFn)(this);

    if Hachimi::instance().localized_data.load().config.now_loading_comic_title_ellipsis {
        let comic_title = get__comicTitle(this);
        if comic_title.is_null() { return; }

        let text = Text::get_text(comic_title);
        if text.is_null() { return; }

        if let Some(new_text) = truncate_text_il2cpp(text, COMIC_TITLE_LINE_WIDTH, true) {
            Text::set_horizontalOverflow(comic_title, 1);
            Text::set_text(comic_title, new_text);
        }
    }
}

type ShowFn = extern "C" fn(this: *mut Il2CppObject, type_: i32, onComplete: *mut Il2CppDelegate, overrideDuration: *mut Il2CppObject, easeType: i32, customInEffect: *mut Il2CppObject, customLoopEffect: *mut Il2CppObject, customOutEffect: *mut Il2CppObject, charaId: i32);
extern "C" fn Show(this: *mut Il2CppObject, #[allow(unused_mut)] mut type_: i32, onComplete: *mut Il2CppDelegate, overrideDuration: *mut Il2CppObject, easeType: i32, customInEffect: *mut Il2CppObject, customLoopEffect: *mut Il2CppObject, customOutEffect: *mut Il2CppObject, charaId: i32) {
    let config = crate::core::Hachimi::instance().config.load();
    #[cfg(target_os = "windows")]
    if type_ == 2 && !config.windows.ui_loading_show_orientation_guide {
        type_ = 0;
    }
    if !config.hide_now_loading {
        get_orig_fn!(Show, ShowFn)(this, type_, onComplete, overrideDuration, easeType, customInEffect, customLoopEffect, customOutEffect, charaId);
    }
    if config.hide_now_loading && !onComplete.is_null() {
        unsafe {
            let invoke: extern "C" fn(*mut Il2CppObject, *const MethodInfo) = std::mem::transmute((*onComplete).method_ptr);
            invoke((*onComplete).target, (*onComplete).method);
        }
    }
}

type HideFn = extern "C" fn(this: *mut Il2CppObject, onComplete: *mut Il2CppDelegate, overrideDuration: *mut Il2CppObject, easeType: i32, onUnloadCustomEffectResourcesComplete: *mut Il2CppDelegate);
extern "C" fn Hide(this: *mut Il2CppObject, onComplete: *mut Il2CppDelegate, overrideDuration: *mut Il2CppObject, easeType: i32, onUnloadCustomEffectResourcesComplete: *mut Il2CppDelegate) {
    let config = crate::core::Hachimi::instance().config.load();
    if !config.hide_now_loading {
        get_orig_fn!(Hide, HideFn)(this, onComplete, overrideDuration, easeType, onUnloadCustomEffectResourcesComplete);
    }
    if config.hide_now_loading && !onComplete.is_null() {
        unsafe {
            let invoke: extern "C" fn(*mut Il2CppObject, *const MethodInfo) = std::mem::transmute((*onComplete).method_ptr);
            invoke((*onComplete).target, (*onComplete).method);
        }
    }
}

// Gallop.NowLoading plays the between-scene wipe through these three, and the shipped
// FADE_TIME / BLACK_FADE_TIME / WHITE_OUT_HORSE_SHOE_FADE_TIME constants reach the
// tween as arguments here. They are `const`, so they have no writable storage; scaling
// the argument is the only place the value can still be reached.
//
// PlayFadeNowLoading takes the alpha endpoints first: the calls logged by this client are
// always (0, 1, dur) for the wipe in and (1, 0, dur) for the wipe out, so only the third
// argument is a duration. Scaling the endpoints made the fade target 0.05 at factor 20.
const TRANSITION: AnimationSpeed::Group = AnimationSpeed::Group::Transition;

type PlayFadeNowLoadingFn = extern "C" fn(this: *mut Il2CppObject, first: f32, second: f32, third: f32, onComplete: *mut Il2CppObject);
extern "C" fn PlayFadeNowLoading(this: *mut Il2CppObject, first: f32, second: f32, third: f32, onComplete: *mut Il2CppObject) {
    if AnimationSpeed::factor(TRANSITION) != 1.0 {
        debug!("NowLoading::PlayFadeNowLoading({}, {}, {})", first, second, third);
    }

    get_orig_fn!(PlayFadeNowLoading, PlayFadeNowLoadingFn)(
        this,
        first,
        second,
        AnimationSpeed::scale_duration(third, TRANSITION),
        onComplete
    );
}

type PlayInNowLoadingFn = extern "C" fn(this: *mut Il2CppObject, duration: f32, onComplete: *mut Il2CppObject);
extern "C" fn PlayInNowLoading(this: *mut Il2CppObject, duration: f32, onComplete: *mut Il2CppObject) {
    if AnimationSpeed::factor(TRANSITION) != 1.0 {
        debug!("NowLoading::PlayInNowLoading({})", duration);
    }

    get_orig_fn!(PlayInNowLoading, PlayInNowLoadingFn)(this, AnimationSpeed::scale_duration(duration, TRANSITION), onComplete);
}

type PlayOutNowLoadingFn = extern "C" fn(this: *mut Il2CppObject, duration: f32, onComplete: *mut Il2CppObject);
extern "C" fn PlayOutNowLoading(this: *mut Il2CppObject, duration: f32, onComplete: *mut Il2CppObject) {
    if AnimationSpeed::factor(TRANSITION) != 1.0 {
        debug!("NowLoading::PlayOutNowLoading({})", duration);
    }

    get_orig_fn!(PlayOutNowLoading, PlayOutNowLoadingFn)(this, AnimationSpeed::scale_duration(duration, TRANSITION), onComplete);
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, NowLoading);

    let SetupLoadingTips_addr = get_method_addr(NowLoading, c"SetupLoadingTips", 0);
    let show_addr = get_method_addr(NowLoading, c"Show", 8);
    let hide_addr = get_method_addr(NowLoading, c"Hide", 4);

    new_hook!(SetupLoadingTips_addr, SetupLoadingTips);
    new_hook!(show_addr, Show);
    new_hook!(hide_addr, Hide);

    // Show/8 and Hide/4 are the Japanese client's signatures; this client has Show/7
    // and Hide/3 with value-type parameters at different positions, so those two are
    // left alone until the parameter layouts are known. These three take only floats
    // and references, so their wrapper cannot misread an argument.
    let play_fade_addr = unsafe { AnimationSpeed::resolve_method(
        NowLoading, "PlayFadeNowLoading",
        &[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    let play_in_addr = unsafe { AnimationSpeed::resolve_method(
        NowLoading, "PlayInNowLoading",
        &[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    let play_out_addr = unsafe { AnimationSpeed::resolve_method(
        NowLoading, "PlayOutNowLoading",
        &[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_CLASS],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };

    if play_fade_addr != 0 { new_hook!(play_fade_addr, PlayFadeNowLoading); }
    if play_in_addr != 0 { new_hook!(play_in_addr, PlayInNowLoading); }
    if play_out_addr != 0 { new_hook!(play_out_addr, PlayOutNowLoading); }

    unsafe {
        _COMICTITLE_FIELD = get_field_from_name(NowLoading, c"_comicTitle");
    }
}