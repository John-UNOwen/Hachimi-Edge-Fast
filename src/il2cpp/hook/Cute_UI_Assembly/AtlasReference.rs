use std::ptr::null_mut;

use widestring::Utf16Str;

use crate::{core::{ext::Utf16StringExt, Hachimi}, il2cpp::{
    hook::{
        UnityEngine_AssetBundleModule::AssetBundle,
        UnityEngine_CoreModule::Sprite
    },
    symbols::{get_field_from_name, get_field_object_value, Array},
    types::*, utils::replace_texture_with_diff
}};

static mut CLASS: *mut Il2CppClass = null_mut();
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

static mut SPRITES_FIELD: *mut FieldInfo = null_mut();
fn get_sprites(this: *mut Il2CppObject) -> Array {
    Array::from(get_field_object_value(this, unsafe { SPRITES_FIELD }))
}

// hook::UnityEngine_AssetBundleModule::AssetBundle
// name: assets/_gallopresources/bundle/resources/atlas/**.asset
pub fn on_LoadAsset(bundle: *mut Il2CppObject, this: *mut Il2CppObject, name: &Utf16Str) {
    if !name.starts_with(AssetBundle::ASSET_PATH_PREFIX) {
        debug!("non-resource atlas: {}", name);
        return;
    }

    // C10 applied marker, on the atlas asset this patch's file is keyed by: the same atlas asset is
    // handed over on the sync load path and on the async one, and its replacement is a diff composed
    // onto whatever the atlas texture currently holds.
    if AssetBundle::asset_is_patched(this, AssetBundle::PatchSite::AtlasTexture) {
        return;
    }

    if Hachimi::instance().config.load().apply_atlas_workaround {
        return;
    }

    let base_path = name[AssetBundle::ASSET_PATH_PREFIX.len()..].path_basename();
    if !base_path.starts_with("atlas/") {
        debug!("bad path: {}", name);
        return;
    }
    let rel_replace_path = base_path.to_string() + ".png";
    let localized_data = Hachimi::instance().localized_data.load();
    let Some(replace_path) = localized_data.get_assets_path(&rel_replace_path) else {
        return;
    };
    let metadata = localized_data.load_asset_metadata(&rel_replace_path);
    if !AssetBundle::check_asset_bundle_name(bundle, &metadata) {
        return;
    }

    // All of the sprites in the atlas uses the same texture so we just need to replace one of them
    //
    // C9: `sprites` is the game's own array field. An atlas whose slot the bundle has not filled
    // hands back no array at all, and an array of references has slots nobody filled; both read
    // here as "there is no sprite to take a texture off", which is what this already stops at.
    let sprites = get_sprites(this);
    let Some(sprite) = unsafe { sprites.as_slice() }.iter().find(|sprite| !sprite.is_null()) else {
        return;
    };
    let texture = Sprite::get_texture(*sprite);
    // Mark only on a write that happened, so an atlas whose replacement failed to load is left open
    // for the next load of it.
    if replace_texture_with_diff(texture, replace_path, true) {
        AssetBundle::mark_asset_patched(this, AssetBundle::PatchSite::AtlasTexture);
    }
}

pub fn init(Cute_UI_Assembly: *const Il2CppImage) {
    get_class_or_return!(Cute_UI_Assembly, "Cute.UI", AtlasReference);

    unsafe {
        CLASS = AtlasReference;
        SPRITES_FIELD = get_field_from_name(AtlasReference, c"sprites")
    }
}