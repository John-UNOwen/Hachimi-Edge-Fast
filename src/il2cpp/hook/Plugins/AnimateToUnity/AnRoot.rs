use std::path::Path;

use fnv::FnvHashMap;
use serde::Deserialize;
use widestring::Utf16Str;

use crate::{
    core::{ext::Utf16StringExt, hachimi::AssetInfo, Hachimi},
    il2cpp::{
        api::{il2cpp_class_get_type, il2cpp_type_get_object}, ext::{Il2CppStringExt, StringExt}, hook::{Plugins::AnimateToUnity::AnKeyParameter, UnityEngine_AssetBundleModule::AssetBundle, UnityEngine_CoreModule::Object}, symbols::{IList, get_field_from_name, get_field_object_value}, types::*, utils::replace_texture_with_diff
    }
};

use super::{
    AnMeshInfoParameterGroup, AnMeshParameter, AnMeshParameterGroup, AnMotionParameter, AnMotionParameterGroup,
    AnObjectParameterBase, AnRootParameter, AnTextParameter
};

static mut TYPE_OBJECT: *mut Il2CppObject = 0 as _;
pub fn type_object() -> *mut Il2CppObject {
    unsafe { TYPE_OBJECT }
}

// AnRootParameter
static mut _PARAMETER_FIELD: *mut FieldInfo = 0 as _;
pub fn get__parameter(this: *mut Il2CppObject) -> *mut Il2CppObject {
    get_field_object_value(this, unsafe { _PARAMETER_FIELD })
}

// AnMeshParameterGroup
static mut _MESHPARAMETERGROUP_FIELD: *mut FieldInfo = 0 as _;
pub fn get__meshParameterGroup(this: *mut Il2CppObject) -> *mut Il2CppObject {
    get_field_object_value(this, unsafe { _MESHPARAMETERGROUP_FIELD })
}

static mut _TOPOBJECT_FIELD: *mut FieldInfo = 0 as _;
pub fn get__topObject(this: *mut Il2CppObject) -> *mut Il2CppObject {
    get_field_object_value(this, unsafe { _TOPOBJECT_FIELD })
}

#[derive(Deserialize)]
pub struct AnRootData {
    #[serde(default)]
    motion_parameter_list: FnvHashMap<i32, AnMotionParameterData>
}

#[derive(Deserialize)]
struct AnMotionParameterData {
    #[serde(default)]
    text_param_list: FnvHashMap<i32, AnTextParameterData>,
    #[serde(default)]
    plane_param_list: FnvHashMap<i32, AnPlaneParameterData>
}

#[derive(Deserialize)]
struct AnObjectParameterBaseData {
    position_offset: Option<Vector3_t>,
    scale: Option<Vector3_t>,
    anim_pos_offset_adj: Option<Vector2_t>
}

#[derive(Deserialize)]
struct AnTextParameterData {
    text: Option<String>,

    #[serde(flatten)]
    base: AnObjectParameterBaseData
}

#[derive(Deserialize)]
struct AnPlaneParameterData {
    #[serde(flatten)]
    base: AnObjectParameterBaseData
}

pub fn on_LoadAsset(bundle: *mut Il2CppObject, this: *mut Il2CppObject, name: &Utf16Str) {
    // SAFETY: The asset path has been checked prior to this being called in GameObject::on_LoadAsset
    let base_path = name[AssetBundle::ASSET_PATH_PREFIX.len()..].path_basename();

    let localized_data = Hachimi::instance().localized_data.load();
    let asset_info: AssetInfo<AnRootData> = localized_data.load_asset_info(&base_path.to_string());
    if !AssetBundle::check_asset_bundle_name(bundle, asset_info.metadata_ref()) {
        return;
    }

    patch_asset(this, asset_info.data.as_ref());
}

pub fn patch_asset(this: *mut Il2CppObject, data_opt: Option<&AnRootData>) {
    /*** Texture set replacement ***/
    // C10 applied marker, per write rather than per handler. `patch_asset` is reached for one AnRoot
    // through three paths - this module's `on_LoadAsset`, `FlashActionPlayer::on_LoadAsset` and
    // `TweenAnimationTimelineComponent::on_LoadAsset` - and two of the writes below compound: a
    // texture diff composes onto whatever the texture already holds, and the key offset loop adds an
    // offset to `key_values.y` it may already have added. Each is marked on the object it was written
    // into (the texture, the plane param), so a second pass rewrites nothing while the writes that do
    // not compound - text, position offset, scale - still run. AGENTS section 5: remember the original
    // value, never write from the current one.

    let param_group = get__meshParameterGroup(this);
    let Some(param_list) = IList::<*mut Il2CppObject>::new(AnMeshParameterGroup::get__meshParameterList(param_group)) else {
        return;
    };

    let localized_data = Hachimi::instance().localized_data.load();
    for param in param_list.iter() {
        // C9: `param` is a slot of the game's own parameter list, and the name is the game's own
        // answer for it. A slot nobody filled, or a parameter the asset never named, is a
        // legitimate null: there is no name to build a texture set path from.
        if param.is_null() {
            continue;
        }

        let param_name = Object::get_name(param);
        if param_name.is_null() {
            continue;
        }

        let Some(group_list) = IList::new(AnMeshParameter::get__meshParameterGroupList(param)) else {
            return;
        };

        let amp_name = unsafe { (*param_name).as_utf16str() };
        let texture_sets_path = Path::new("an_texture_sets").join(&amp_name.to_string());

        for group in group_list.iter() {
            let texture_color = AnMeshInfoParameterGroup::get__textureSetColor(group);
            let texture_set_name = AnMeshInfoParameterGroup::get_TextureSetName(group);

            // C9: the texture set name is a value the game keeps in its own group. A group that
            // names no texture set is a legitimate null, and there is no file name to build from it.
            if texture_set_name.is_null() {
                continue;
            }

            let texture_set_name_utf16 = unsafe { (*texture_set_name).as_utf16str() };

            // Try to load a replacement
            let texture_set_filename = texture_set_name_utf16.to_string() + ".png";
            let rel_path = texture_sets_path.join(texture_set_filename);

            if let Some(path) = localized_data.get_assets_path(&rel_path) {
                // C10: marked on the texture the write went into, so the same texture is not given
                // this diff twice while a different texture set of the same asset is still patched.
                if !AssetBundle::asset_is_patched(texture_color, AssetBundle::PatchSite::AnRootTexture)
                    && replace_texture_with_diff(texture_color, &path, true)
                {
                    AssetBundle::mark_asset_patched(texture_color, AssetBundle::PatchSite::AnRootTexture);
                }
            }

            // Replace alpha texture (_A.png files)
            let texture_alpha = AnMeshInfoParameterGroup::get__textureSetA(group);
            if !texture_alpha.is_null() {
                let texture_set_filename_a = texture_set_name_utf16.to_string() + "_A.png";
                let rel_path_a = texture_sets_path.join(texture_set_filename_a);

                if let Some(path_a) = localized_data.get_assets_path(&rel_path_a) {
                    if !AssetBundle::asset_is_patched(texture_alpha, AssetBundle::PatchSite::AnRootTexture)
                        && replace_texture_with_diff(texture_alpha, &path_a, true)
                    {
                        AssetBundle::mark_asset_patched(texture_alpha, AssetBundle::PatchSite::AnRootTexture);
                    }
                }
            }
        }
    }

    /*** Asset data patches ***/
    // The absolute writes below (text, position offset, scale) are re-runnable, so only the additive
    // key offsets are gated on the marker; a second path reaching this AnRoot still gets its text
    // and scale patch, it just does not move an offset twice.
    if let Some(data) = data_opt {
        // quick escape!!!11
        if data.motion_parameter_list.is_empty() {
            return;
        }

        let root_param = get__parameter(this);
        let motion_param_group = AnRootParameter::get__motionParameterGroup(root_param);
        let Some(motion_param_list) = IList::new(AnMotionParameterGroup::get__motionParameterList(motion_param_group)) else {
            return;
        };

        for (i, motion_param_data) in data.motion_parameter_list.iter() {
            // quick escape!!!11
            if motion_param_data.text_param_list.is_empty() && motion_param_data.plane_param_list.is_empty() {
                continue;
            }

            let Some(motion_param) = motion_param_list.get(*i) else {
                warn!("motion param {} out of range (max {})", *i, motion_param_list.count());
                continue;
            };

            if !motion_param_data.text_param_list.is_empty() {
                let Some(text_param_list) = IList::new(AnMotionParameter::get__textParamList(motion_param)) else {
                    warn!("Failed to get text_param_list for motion param {}", *i);
                     continue;
                 };

                for (j, text_param_data) in motion_param_data.text_param_list.iter() {
                    let Some(text_param) = text_param_list.get(*j) else {
                        warn!("text param {} of motion param {} out of range (max {})", *j, *i, text_param_list.count());
                        continue;
                    };

                    if let Some(text) = &text_param_data.text {
                        AnTextParameter::set__text(text_param, text.to_il2cpp_string());
                    }

                    if let Some(position_offset) = &text_param_data.base.position_offset {
                        AnObjectParameterBase::set__positionOffset(text_param, position_offset);
                    }

                    if let Some(scale) = &text_param_data.base.scale {
                        AnObjectParameterBase::set__scale(text_param, scale);
                    }
                }
            }

            if !motion_param_data.plane_param_list.is_empty() {
                let Some(plane_param_list) = IList::new(AnMotionParameter::get__planeParamList(motion_param)) else {
                    warn!("Failed to get plane_param_list for motion param {}", *i);
                    continue;
                };

                for (j, plane_param_data) in motion_param_data.plane_param_list.iter() {
                    let Some(plane_param) = plane_param_list.get(*j) else {
                        warn!("plane param {} of motion param {} out of range (max {})", *j, *i, plane_param_list.count());
                        continue;
                    };

                    if let Some(position_offset) = &plane_param_data.base.position_offset {
                        AnObjectParameterBase::set__positionOffset(plane_param, position_offset);
                    }
                    if let Some(scale) = &plane_param_data.base.scale {
                        AnObjectParameterBase::set__scale(plane_param, scale);
                    }
                    if let Some(anim_pos_offset) = &plane_param_data.base.anim_pos_offset_adj {
                        // Count should be 3 if present, representing XYZ axes. We ignore Z.
                        if AssetBundle::asset_is_patched(plane_param, AssetBundle::PatchSite::AnRootKeyOffset) {
                            // C10: these key offsets have already been added to this plane param. The
                            // loops below write `key_values.y += offset`, so running them a second
                            // time over the same AnRoot moves an offset it already moved - the
                            // C22/C24 shape. Marked on the plane param, not on the AnRoot, so a plane
                            // param whose key list could not be read stays open for the next pass.
                        }
                        else if let Some(pos_offset_keyparam_list) = IList::new(AnObjectParameterBase::get__positionOffsetKeyParamList(plane_param)) {
                            AssetBundle::mark_asset_patched(plane_param, AssetBundle::PatchSite::AnRootKeyOffset);
                            if pos_offset_keyparam_list.count() > 1 {
                                let x_axis_key_param = pos_offset_keyparam_list.get(0).unwrap();
                                if let Some(x_axis_key_list) = IList::<Vector2_t>::new(AnKeyParameter::get__keyList(x_axis_key_param)) {
                                    for k in 0..x_axis_key_list.count() {
                                        let mut key_values = x_axis_key_list.get(k).unwrap();
                                        key_values.y += anim_pos_offset.x;
                                        x_axis_key_list.set(k, key_values);
                                    }
                                }
                                let y_axis_key_param = pos_offset_keyparam_list.get(1).unwrap();
                                if let Some(y_axis_key_list) = IList::<Vector2_t>::new(AnKeyParameter::get__keyList(y_axis_key_param)) {
                                    for k in 0..y_axis_key_list.count() {
                                        let mut key_values = y_axis_key_list.get(k).unwrap();
                                        key_values.y += anim_pos_offset.y;
                                        y_axis_key_list.set(k, key_values);
                                    }
                                }
                            }
                        }
                        else {
                            warn!("Failed to get pos_offset_keyparams for plane param {} of motion param {}", *j, *i);
                        }
                    }
                }
            }
        }
    }
}

pub fn init(Plugins: *const Il2CppImage) {
    get_class_or_return!(Plugins, AnimateToUnity, AnRoot);

    unsafe {
        TYPE_OBJECT = il2cpp_type_get_object(il2cpp_class_get_type(AnRoot));
        _PARAMETER_FIELD = get_field_from_name(AnRoot, c"_parameter");
        _MESHPARAMETERGROUP_FIELD = get_field_from_name(AnRoot, c"_meshParameterGroup");
        _TOPOBJECT_FIELD = get_field_from_name(AnRoot, c"_topObject");
    }
}