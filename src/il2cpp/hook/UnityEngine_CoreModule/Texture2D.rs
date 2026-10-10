use std::{path::Path, ptr::null_mut};

use widestring::Utf16Str;

use crate::{core::{ext::Utf16StringExt, Hachimi}, il2cpp::{
    api::{il2cpp_object_new, il2cpp_resolve_icall},
    hook::{
        mscorlib,
        UnityEngine_AssetBundleModule::AssetBundle::{self, ASSET_PATH_PREFIX},
        UnityEngine_ImageConversionModule::ImageConversion
    },
    symbols::{get_method_addr, Array},
    ext::StringExt,
    types::*, utils
}};

use super::{Graphics, RenderTexture, Texture};

static mut CLASS: *mut Il2CppClass = null_mut();
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

static mut CTOR_ADDR: usize = 0;
impl_addr_wrapper_fn!(_ctor, CTOR_ADDR, (),
    this: *mut Il2CppObject, width: i32, height: i32
);

pub fn new(width: i32, height: i32) -> *mut Il2CppObject {
    let this = il2cpp_object_new(class());
    _ctor(this, width, height);
    this
}

pub fn from_image_file<P: AsRef<Path>>(path: P, _mip_chain: bool, mark_non_readable: bool) -> Option<*mut Il2CppObject> {
    let path_ref = path.as_ref();

    // check if file exists
    let metadata = std::fs::metadata(path_ref).ok()?;
    if !metadata.is_file() {
        return None;
    }

    // we've done everything we can, can't catch C# exceptions, yolo :)
    let path_str = path_ref.to_str()?;
    let bytes = mscorlib::File::ReadAllBytes(path_str.to_il2cpp_string());
    let texture = new(2, 2);
    if ImageConversion::LoadImage(texture, bytes, mark_non_readable) {
        Some(texture)
    }
    else {
        warn!("Failed to load texture: {}", path_str);
        None
    }
}

pub fn load_image_file<P: AsRef<Path>>(this: *mut Il2CppObject, path: P, mark_non_readable: bool) -> bool {
    let path_ref = path.as_ref();

    // check if file exists
    let Ok(metadata) = std::fs::metadata(path_ref) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    // we've done everything we can, can't catch C# exceptions, yolo :)
    unsafe { load_image_file_unsafe(this, path, mark_non_readable) }
}

pub unsafe fn load_image_file_unsafe<P: AsRef<Path>>(this: *mut Il2CppObject, path: P, mark_non_readable: bool) -> bool {
    if let Some(path_str) = path.as_ref().to_str() {
        let bytes = mscorlib::File::ReadAllBytes(path_str.to_il2cpp_string());
        if ImageConversion::LoadImage(this, bytes, mark_non_readable) {
            return true;
        }
        else {
            warn!("Failed to load texture: {}", path_str);
        }
    }

    false
}

pub fn render_to_texture(this: *mut Il2CppObject) -> *mut Il2CppObject {
    // Create a render texture
    let width = Texture::GetDataWidth(this);
    let height = Texture::GetDataHeight(this);
    let render_texture = RenderTexture::GetTemporary(width, height);

    // Blit this texture to the render texture
    Graphics::Blit2(this, render_texture);

    // Set the active render texture, backup the previous active texture
    let prev_active = RenderTexture::GetActive();
    RenderTexture::SetActive(render_texture);

    // Create a new texture and read the data from the render texture
    let output_texture = new(width, height);
    ReadPixels(
        output_texture,
        Rect_t { x: 0.0, y: 0.0, width: width as f32, height: height as f32 },
        0, 0
    );

    // Revert active texture, release temp render texture
    RenderTexture::SetActive(prev_active);
    RenderTexture::ReleaseTemporary(render_texture);

    output_texture
}

// hook::UnityEngine_AssetBundleModule::AssetBundle
pub fn on_LoadAsset(bundle: *mut Il2CppObject, this: *mut Il2CppObject, name: &Utf16Str) {
    if !name.starts_with(ASSET_PATH_PREFIX) {
        debug!("non-resource texture: {}", name);
        return;
    }

    // C10 applied marker. This handler is reached twice for the same texture in the ordinary case:
    // `LoadAsset_Internal` and `AssetBundleRequest::GetResult` both end in `on_LoadAsset`. A texture
    // diff is a factor composed onto the original, so composing it a second time onto the image it
    // already produced writes the patch twice and then re-records that patched image as the original
    // it started from (`store_source_texture_hash` in `il2cpp/utils.rs`). AGENTS section 5: never
    // derive the new value from the current one.
    if AssetBundle::asset_is_patched(this, AssetBundle::PatchSite::TextureReplacement) {
        return;
    }

    let orig_path = &name[ASSET_PATH_PREFIX.len()..];
    let rel_replace_path = Path::new("textures").join(orig_path.to_string());
    let localized_data = Hachimi::instance().localized_data.load();
    let Some(replace_path) = localized_data.get_assets_path(&rel_replace_path) else {
        return;
    };

    // Common diff handling
    // ...chara/chrXXXX/petit/petit_chr_XXXX_YYYYYY_ZZZZ.png
    let petit_type = if orig_path.len() == 50 && orig_path.starts_with("chara/chr") && orig_path[13..30] == "/petit/petit_chr_" {
        Some(&orig_path[42..46])
    }
    else {
        None
    };
    // Only the two "Train" button petit textures have a common diff to fall back to.
    let petit_type = petit_type.filter(|type_| *type_ == "0070" || *type_ == "0071");

    let own_diff_path = utils::get_texture_diff_path(&replace_path);
    let common_diff_path = petit_type.and_then(|type_| localized_data.get_assets_path(
        &Path::new("textures").join(format!("chara/_chr/petit/petit_chr_{}.diff.png", type_))
    ));

    // Nothing on disk for this asset: there is no patch here to identify or apply. Checked before
    // the metadata read so an ordinary texture load does not open a json it does not have.
    if !replace_path.exists() && !own_diff_path.exists() && !common_diff_path.as_ref().is_some_and(|p| p.exists()) {
        return;
    }

    // C10: the identity check the guard stopped running. Confirm the asset standing here is the
    // asset this replacement was authored for before anything is written into it. The patch's own
    // metadata carries the identity, and a package that records none for this platform states
    // nothing to confirm (upstream #56).
    let metadata = localized_data.load_asset_metadata(&rel_replace_path);
    if !AssetBundle::check_asset_bundle_name(bundle, &metadata) {
        return;
    }

    if petit_type.is_some() {
        // Let texture's own diff take precedence
        // Don't allow direct loading fallback here, otherwise the common diff would be skipped
        // after the initial patch (when the texture has already been created)
        if utils::replace_texture_with_diff_ex(this, &replace_path, &own_diff_path, true, false) {
            AssetBundle::mark_asset_patched(this, AssetBundle::PatchSite::TextureReplacement);
            return;
        }

        // Try to load common diff for "Train" buttons
        let Some(common_diff_path) = common_diff_path else {
            return;
        };

        if utils::replace_texture_with_diff_ex(this, &replace_path, &common_diff_path, true, true) {
            AssetBundle::mark_asset_patched(this, AssetBundle::PatchSite::TextureReplacement);
        }
        return;
    }

    // Normal replacement procedure
    if utils::replace_texture_with_diff(this, &replace_path, true) {
        AssetBundle::mark_asset_patched(this, AssetBundle::PatchSite::TextureReplacement);
    }
}

static mut GETPIXELS32_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetPixels32, GETPIXELS32_ADDR, Array<Color32_t>, this: *mut Il2CppObject, mip_level: i32);

static mut READPIXELS_ADDR: usize = 0;
impl_addr_wrapper_fn!(ReadPixels, READPIXELS_ADDR, (), this: *mut Il2CppObject, source: Rect_t, dest_x: i32, dest_y: i32);

static mut APPLY_ADDR: usize = 0;
impl_addr_wrapper_fn!(Apply, APPLY_ADDR, (), this: *mut Il2CppObject);

pub fn init(UnityEngine_CoreModule: *const Il2CppImage) {
    get_class_or_return!(UnityEngine_CoreModule, UnityEngine, Texture2D);

    unsafe {
        CLASS = Texture2D;
        CTOR_ADDR = get_method_addr(Texture2D, c".ctor", 2);
        GETPIXELS32_ADDR = il2cpp_resolve_icall(c"UnityEngine.Texture2D::GetPixels32(System.Int32)".as_ptr());
        READPIXELS_ADDR = get_method_addr(Texture2D, c"ReadPixels", 3);
        APPLY_ADDR = get_method_addr(Texture2D, c"Apply", 0);
    }
}