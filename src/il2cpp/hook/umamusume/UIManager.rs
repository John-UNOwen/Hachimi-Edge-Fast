use crate::{
    core::Hachimi,
    il2cpp::{
        ext::{Il2CppStringExt, StringExt},
        hook::UnityEngine_UI::CanvasScaler,
        symbols::{get_method_addr, get_method_overload_addr, get_field_from_name, Array, SingletonLike},
        types::*
    }
};

static mut CLASS: *mut Il2CppClass = 0 as _;
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

pub fn instance() -> *mut Il2CppObject {
    let Some(singleton) = SingletonLike::new(class()) else {
        return 0 as _;
    };
    singleton.instance()
}

static mut GETCANVASSCALERLIST_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetCanvasScalerList, GETCANVASSCALERLIST_ADDR, Array, this: *mut Il2CppObject);

def_field_object_accessors!(get_noticeCanvas, set_noticeCanvas, _NOTICECANVAS_FIELD, Il2CppObject);
def_field_object_accessors!(get_systemCanvas, set_systemCanvas, _SYSTEMCANVAS_FIELD, Il2CppObject);
def_field_object_accessors!(get_mainCanvas, set_mainCanvas, _MAINCANVAS_FIELD, Il2CppObject);

pub fn apply_ui_scale() {
    let config = Hachimi::instance().config.load();

    #[allow(unused_mut)]
    let mut scale = config.ui_scale;

    #[cfg(target_os = "windows")]
    {
        if config.windows.freeform_window {
            if config.windows.freeform_ui_scale_auto {
                if let Some((_, height)) = crate::windows::wnd_hook::get_client_size() {
                    scale *= height as f32 / 1080.0 *
                        config.windows.freeform_ui_scale_auto_ratio;
                }
                scale = scale.clamp(0.1, 10.0);
            }
        }
        else if let Some((width, height)) = crate::windows::utils::get_scaling_res() {
            if width < height {
                scale *= width as f32 / 1080.0;
            }
            else {
                scale *= height as f32 / 1080.0;
            }
        }
    }

    // C9: the singleton is the game's, and a `get_Instance` that has nothing to answer hands back
    // null. There is no scaler list to read off it, and asking the game for one on a null `this`
    // is a crash inside the game rather than a UI scale that did not apply.
    let ui_manager = instance();
    if ui_manager.is_null() {
        return;
    }

    let canvas_scaler_list = GetCanvasScalerList(ui_manager);
    for scaler in unsafe { canvas_scaler_list.as_slice().iter() } {
        // C9: the list is the game's array of scalers; a slot nobody filled is not a scaler.
        if scaler.is_null() {
            continue;
        }

        #[cfg(target_os = "android")]
        {
            let res = CanvasScaler::get_m_ReferenceResolution(*scaler);

            // C9: this hands back the address of a slot in the game's object - null when the field
            // name is not in this client, which the getter already logs - and there is nothing to
            // divide at that address.
            if !res.is_null() {
                unsafe {
                    (*res).x /= scale;
                    (*res).y /= scale;
                }
            }
        }
        
        #[cfg(target_os = "windows")]
        CanvasScaler::set_scaleFactor(*scaler, scale);
    }
}

type SetHeaderTitleTextFn = extern "C" fn(this: *mut Il2CppObject, text: *mut Il2CppString, guide_id: i32);
def_detour! {
    SetHeaderTitleText(this: *mut Il2CppObject, text_: *mut Il2CppString, guide_id: i32) {
            // C9: `text_` is what the game is about to write into the header title, and clearing a
            // title is done by handing it null. There is no string to look for a template in, and
            // the original call is what the game wanted to make anyway.
            let text_utf16 = if text_.is_null() {
                return get_orig_fn!(SetHeaderTitleText, SetHeaderTitleTextFn)(this, text_, guide_id);
            }
            else {
                unsafe { (*text_).as_utf16str() }
            };

        // The title text (aka the purple ribbon on the top left of the screen) doesn't run
        // through TextGenerator, so we have to evaluate templates here (by emptying any filter exprs)
        let new_text = if text_utf16.as_slice().contains(&36) { // 36 = dollar sign ($)
            Hachimi::instance().template_parser
                .remove_filters(&text_utf16.to_string())
                .to_il2cpp_string()
        }
        else {
            text_
        };

        get_orig_fn!(SetHeaderTitleText, SetHeaderTitleTextFn)(this, new_text, guide_id)
    }
}

#[cfg(target_os = "windows")]
type ChangeResizeUIForPCFn = extern "C" fn(this: *mut Il2CppObject, width: i32, height: i32);
def_detour! {
    #[cfg(target_os = "windows")]
    ChangeResizeUIForPC(this: *mut Il2CppObject, width: i32, height: i32) {
            use super::GraphicSettings;

        let windows_config = &Hachimi::instance().config.load().windows;
        if !windows_config.freeform_window {
            get_orig_fn!(ChangeResizeUIForPC, ChangeResizeUIForPCFn)(this, width, height);
        }

        // Recreate the render texture so it scales with the resolution
        if windows_config.freeform_window ||
            windows_config.resolution_scaling.is_not_default()
        {
            CreateRenderTextureFromScreen(this);
            let graphic_settings = GraphicSettings::instance();
            if !graphic_settings.is_null() {
                GraphicSettings::Update3DRenderTexture(graphic_settings);
            }
        }
        apply_ui_scale();
    }
}

#[cfg(target_os = "windows")]
pub fn refresh_after_window_resize(width: i32, height: i32) {
    use super::{GraphicSettings, Screen, TapEffectController, WindowsGamepadControl};

    if width <= 0 || height <= 0 {
        return;
    }

    Screen::update_original_screen_size(width, height);
    WindowsGamepadControl::refresh_after_window_resize();

    let this = instance();
    if !this.is_null() {
        CreateRenderTextureFromScreen(this);
        let graphic_settings = GraphicSettings::instance();
        if !graphic_settings.is_null() {
            GraphicSettings::Update3DRenderTexture(graphic_settings);
        }
        apply_ui_scale();
    }

    let tap_effect_controller = TapEffectController::instance();
    TapEffectController::RefreshAll(tap_effect_controller);
}

def_detour! {
    #[cfg(target_os = "android")]
    WaitBootSetup_MoveNext(enumerator: *mut Il2CppObject) coroutine answer -> bool {
            use crate::il2cpp::symbols::MoveNextFn;
        let moved = get_orig_fn!(WaitBootSetup_MoveNext, MoveNextFn)(enumerator);
        // Published before `apply_ui_scale`: the boot coroutine's own step is the answer a trip in
        // the scale half owes the game.
        answer.publish(moved);
        if !moved {
            apply_ui_scale();
        }
        moved
    }
}

#[cfg(target_os = "android")]
type WaitBootSetupFn = extern "C" fn(this: *mut Il2CppObject) -> crate::il2cpp::symbols::IEnumerator;
def_detour! {
    #[cfg(target_os = "android")]
    WaitBootSetup(this: *mut Il2CppObject) answer -> crate::il2cpp::symbols::IEnumerator {
            let enumerator = get_orig_fn!(WaitBootSetup, WaitBootSetupFn)(this);
        answer.publish(crate::il2cpp::symbols::IEnumerator::from(enumerator.this));
        if Hachimi::instance().config.load().ui_scale == 1.0 { return enumerator; }

        if let Err(e) = enumerator.hook_move_next(WaitBootSetup_MoveNext) {
            error!("Failed to hook enumerator: {}", e);
        }

        enumerator
    }
}

#[cfg(target_os = "windows")]
static mut CREATERENDERTEXTUREFROMSCREEN_ADDR: usize = 0;
#[cfg(target_os = "windows")]
impl_addr_wrapper_fn!(CreateRenderTextureFromScreen, CREATERENDERTEXTUREFROMSCREEN_ADDR, (), this: *mut Il2CppObject);

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, UIManager);

    let SetHeaderTitleText_addr = get_method_overload_addr(UIManager, "SetHeaderTitleText",
        &[Il2CppTypeEnum_IL2CPP_TYPE_STRING, Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE]);

    new_hook!(SetHeaderTitleText_addr, SetHeaderTitleText);

    #[cfg(target_os = "windows")]
    {
        let ChangeResizeUIForPC_addr = get_method_addr(UIManager, c"ChangeResizeUIForPC", 2);

        new_hook!(ChangeResizeUIForPC_addr, ChangeResizeUIForPC);
    }

    #[cfg(target_os = "android")]
    {
        let WaitBootSetup_addr = get_method_addr(UIManager, c"WaitBootSetup", 0);

        new_hook!(WaitBootSetup_addr, WaitBootSetup);
    }

    unsafe {
        CLASS = UIManager;
        GETCANVASSCALERLIST_ADDR = get_method_addr(UIManager, c"GetCanvasScalerList", 0);

        _NOTICECANVAS_FIELD = get_field_from_name(UIManager, c"_noticeCanvas");
        _SYSTEMCANVAS_FIELD = get_field_from_name(UIManager, c"_systemCanvas");
        _MAINCANVAS_FIELD = get_field_from_name(UIManager, c"_mainCanvas");

        #[cfg(target_os = "windows")]
        {
            CREATERENDERTEXTUREFROMSCREEN_ADDR = get_method_addr(UIManager, c"CreateRenderTextureFromScreen", 0);
        }
    }
}
