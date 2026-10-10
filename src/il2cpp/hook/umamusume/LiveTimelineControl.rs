use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    core::Hachimi,
    windows::free_camera::{self, CameraScene},
    il2cpp::{
        symbols::get_method_addr,
        types::*,
    },
};

use super::{Director, LiveTimelineWorkSheet, LiveTimelineKeyPostFilmDataList};

static LIVE_TIMELINE_CONTROL: AtomicUsize = AtomicUsize::new(0);


#[repr(C)]
#[derive(Default)]
#[allow(dead_code)]
pub struct Vector4_t {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[repr(C)]
#[allow(dead_code)]
pub struct PostFilmUpdateInfo {
    pub filmMode: i32,
    pub colorType: i32,
    pub filmPower: f32,
    pub filmOffsetParam: Vector2_t,
    pub filmOptionParam: Vector4_t,
    pub color0: Color_t,
    pub color1: Color_t,
    pub color2: Color_t,
    pub color3: Color_t,
    pub depthPower: f32,
    pub DepthClip: f32,
    pub layerMode: i32,
    pub colorBlend: i32,
    pub inverseVignette: bool,
    pub colorBlendFactor: f32,
    pub movieResId: i32,
    pub movieFrameOffset: i32,
    pub movieTime: f32,
    pub movieReverse: bool,
    pub RollAngle: f32,
    pub FilmScale: Vector2_t,
}

impl Default for PostFilmUpdateInfo {
    fn default() -> Self {
        Self {
            filmMode: 0,
            colorType: 0,
            filmPower: 0.0,
            filmOffsetParam: Default::default(),
            filmOptionParam: Default::default(),
            color0: Color_t {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            color1: Color_t {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            color2: Color_t {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            color3: Color_t {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            },
            depthPower: 0.0,
            DepthClip: 0.0,
            layerMode: 0,
            colorBlend: 0,
            inverseVignette: false,
            colorBlendFactor: 0.0,
            movieResId: 0,
            movieFrameOffset: 0,
            movieTime: 0.0,
            movieReverse: false,
            RollAngle: 0.0,
            FilmScale: Default::default(),
        }
    }
}

#[repr(C)]
#[allow(dead_code)]
struct PostEffectUpdateInfo_DOF {
    pub IsEnableDOF: bool,
    pub forcalSize: f32,
    pub blurSpread: f32,
    pub forcalPosition: Vector3_t,
    pub dofQuality: i32,
    pub dofBlurType: i32,
    pub dofForegroundSize: f32,
    pub dofFocalPoint: f32,
    pub dofSoomthness: f32,
    pub isUseFocalPoint: bool,
    pub BallBlurCurveFactor: f32,
    pub BallBlurBrightnessThreshhold: f32,
    pub BallBlurBrightnessIntensity: f32,
    pub BallBlurSpread: f32,
    pub IsPointBallBlur: bool,
}

fn clear_live_screen_effects(sheet: *mut Il2CppObject) {
    if sheet.is_null() || !free_camera::should_remove_live_screen_effects() {
        return;
    }    

    let post_film_keys = LiveTimelineWorkSheet::get_postFilmKeys(sheet);
    if !post_film_keys.is_null() {
        LiveTimelineKeyPostFilmDataList::Clear(post_film_keys);
    }

    let post_film2_keys = LiveTimelineWorkSheet::get_postFilm2Keys(sheet);
    if !post_film2_keys.is_null() {
        LiveTimelineKeyPostFilmDataList::Clear(post_film2_keys);
    }

    let post_film3_keys = LiveTimelineWorkSheet::get_postFilm3Keys(sheet);
    if !post_film3_keys.is_null() {
        LiveTimelineKeyPostFilmDataList::Clear(post_film3_keys);
    }
}

pub fn set_current(this: *mut Il2CppObject) {
    if !this.is_null() {
        LIVE_TIMELINE_CONTROL.store(this as usize, Ordering::Relaxed);
    }
}

fn clear_current() {
    LIVE_TIMELINE_CONTROL.store(0, Ordering::Relaxed);
}

fn should_remove_live_camera_effects() -> bool {
    free_camera::set_live_active();
    free_camera::should_remove_camera_effects()
}

fn should_override_live_camera() -> bool {
    free_camera::set_live_active();
    free_camera::is_scene_enabled(CameraScene::Live)
}

fn apply_current_live_character_options() {
    let director = Director::instance();
    if !director.is_null() {
        Director::apply_live_character_options(director);
    }
}

type NoArgsFn = extern "C" fn(this: *mut Il2CppObject);

type LiveVoidFrameFn = extern "C" fn(this: *mut Il2CppObject, sheet: *mut Il2CppObject, current_frame: i32);
type LiveBoolFrameFn = extern "C" fn(this: *mut Il2CppObject, sheet: *mut Il2CppObject, current_frame: i32) -> bool;
type LiveVoidFrameTimeFn = extern "C" fn(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    current_time: f32,
);

type AlterUpdate_CameraPosFn = extern "C" fn(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    current_time: f32,
    sheet_index: i32,
    is_use_camera_motion: bool,
);
def_detour! {
    AlterUpdate_CameraPos(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    current_time: f32,
    sheet_index: i32,
    mut is_use_camera_motion: bool,
) {
            free_camera::set_live_active();
        clear_live_screen_effects(sheet);
        let free_camera_active = free_camera::is_scene_enabled(CameraScene::Live);
        let frame = if free_camera_active {
            is_use_camera_motion = false;
            0
        } else {
            current_frame
        };
        get_orig_fn!(AlterUpdate_CameraPos, AlterUpdate_CameraPosFn)(
            this,
            sheet,
            frame,
            current_time,
            sheet_index,
            is_use_camera_motion,
        );
    }
}

type AlterUpdate_CameraLookAtFn = extern "C" fn(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    current_time: f32,
    out_look_at: *mut Vector3_t,
);
def_detour! {
    AlterUpdate_CameraLookAt(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    current_time: f32,
    out_look_at: *mut Vector3_t,
) {
            free_camera::set_live_active();
        clear_live_screen_effects(sheet);
        get_orig_fn!(AlterUpdate_CameraLookAt, AlterUpdate_CameraLookAtFn)(
            this,
            sheet,
            current_frame,
            current_time,
            out_look_at,
        );

        set_current(this);
        if free_camera::is_scene_enabled(CameraScene::Live) && !out_look_at.is_null() {
            unsafe {
                *out_look_at = free_camera::camera_look_at();
            }
        }
    }
}

def_detour! {
    LiveTimelineControl_AlterLateUpdate(this: *mut Il2CppObject) {
            free_camera::set_live_active();
        free_camera::tick();
        get_orig_fn!(LiveTimelineControl_AlterLateUpdate, NoArgsFn)(this);
        apply_current_live_character_options();
        let director = Director::instance();
        if !director.is_null() {
            Director::enforce_live_free_camera_output(director);
        }
    }
}

def_detour! {
    LiveTimelineControl_OnDestroy(this: *mut Il2CppObject) {
            Director::restore_live_disabled_heads(0, true);
        clear_current();
        free_camera::end_scene(CameraScene::Live);
        get_orig_fn!(LiveTimelineControl_OnDestroy, NoArgsFn)(this);
    }
    bail {
                get_orig_fn!(LiveTimelineControl_OnDestroy, NoArgsFn)(this)
    }
}

def_detour! {
    AlterUpdate_RadialBlur(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
) {
            if !should_remove_live_camera_effects() {
            get_orig_fn!(AlterUpdate_RadialBlur, LiveVoidFrameFn)(this, sheet, current_frame);
        }
    }
}

type SetupPostFilmUpdateDataInfoFn = extern "C" fn(
    this: *mut Il2CppObject,
    updateInfo: *mut PostFilmUpdateInfo,
    curData: *mut Il2CppObject,
    nextData: *mut Il2CppObject,
    currentFrame: i32,
);
def_detour! {
    SetupPostFilmUpdateDataInfo(
    this: *mut Il2CppObject,
    updateInfo: *mut PostFilmUpdateInfo,
    curData: *mut Il2CppObject,
    nextData: *mut Il2CppObject,
    currentFrame: i32,
) {
            get_orig_fn!(SetupPostFilmUpdateDataInfo, SetupPostFilmUpdateDataInfoFn)(
            this, updateInfo, curData, nextData, currentFrame,
        );

        if should_remove_live_camera_effects() {
            unsafe { *updateInfo = PostFilmUpdateInfo::default(); }
        }
    }
}

type SetupDOFUpdateInfoFn = extern "C" fn(
    this: *mut Il2CppObject,
    update_info: *mut PostEffectUpdateInfo_DOF,
    cur_data: *mut Il2CppObject,
    next_data: *mut Il2CppObject,
    current_frame: i32,
    camera_look_at: Vector3_t,
);
def_detour! {
    SetupDOFUpdateInfo(
    this: *mut Il2CppObject,
    update_info: *mut PostEffectUpdateInfo_DOF,
    cur_data: *mut Il2CppObject,
    next_data: *mut Il2CppObject,
    current_frame: i32,
    camera_look_at: Vector3_t,
) {
            get_orig_fn!(SetupDOFUpdateInfo, SetupDOFUpdateInfoFn)(
            this,
            update_info,
            cur_data,
            next_data,
            current_frame,
            camera_look_at,
        );

        if should_remove_live_camera_effects() {
            // C9: `update_info` is a struct the game owns and hands in; a call that carries none has
            // nothing to switch off, and writing at address 0 is not how that is found out.
            if !update_info.is_null() {
                unsafe {
                    (*update_info).IsEnableDOF = false;
                    (*update_info).isUseFocalPoint = false;
                    (*update_info).IsPointBallBlur = false;
                }
            }
        }
    }
}

type SetupRadialBlurInfoFn = extern "C" fn(
    this: *mut Il2CppObject,
    update_info: *mut Il2CppObject,
    cur_data: *mut Il2CppObject,
    next_data: *mut Il2CppObject,
    current_frame: i32,
);
def_detour! {
    SetupRadialBlurInfo(
    this: *mut Il2CppObject,
    update_info: *mut Il2CppObject,
    cur_data: *mut Il2CppObject,
    next_data: *mut Il2CppObject,
    current_frame: i32,
) {
            if should_remove_live_camera_effects() {
            return;
        }
        get_orig_fn!(SetupRadialBlurInfo, SetupRadialBlurInfoFn)(
            this,
            update_info,
            cur_data,
            next_data,
            current_frame,
        );
    }
}

// C2: these four macros write 13 of the per-frame Live camera detours, and `init` below arms all of
// them unconditionally. Written the way they were - a plain `extern "C" fn`, its mod-side call and
// its hand-back to `get_orig_fn!` straight in the body - they were the hook boundaries the barrier
// C2 installs was not standing on: a panic or an access violation in either half crossed the FFI
// into the trampoline and the IL2CPP runtime that called it. That is not hypothetical at these
// sites. `should_override_live_camera` and `should_remove_live_camera_effects` read the free camera
// state through `STATE`, a `Mutex` taken with `unwrap()` in `src/windows/free_camera.rs`, so a
// thread that died holding it poisons the lock and turns *every later per-frame call of every one
// of these wrappers* into a panic across FFI; and `this` / `sheet` are game objects this wrapper
// does not own, which is what a fault reads.
//
// The first two now expand into `def_detour!`, so the barrier is the macro's - the same shape the
// other Live camera wrappers in this file already have - and the fallback follows the barrier's two
// answers. A body that *panicked* was stopped by the mod's own code: the game never received its
// call, so the bail hands it over, behind `detour_fallback`. A body that *faulted* was stopped by
// the state it was handed, and replaying that state into the same method faults again, so a fault
// answers with nothing. The two secondary-camera forms reach the same macro through its `prelude`
// arm, for the reason written under them: they are the wrappers that need something held in the
// wrapper frame, and that is what the arm is for - not a reason to write the barrier out by hand
// and answer a trip differently from every sibling in this file.
//
// Each macro has a second arm that takes its mod-side half as an expression - the predicate for the
// first two, the statement ahead of the game call for the last two. No shipped instantiation uses
// them: they are there so the tests at the end of this file can build a wrapper out of the very same
// macro with an injected fault. A unit test cannot reach a trampoline or `Hachimi::instance()`
// (AGENTS section 4), and the shipped body's `get_orig_fn!` walks into exactly that, so "the barrier
// is on this boundary" has to be shown with the mod-side half replaced and the game call left
// unreachable. The two secondary-camera forms go one step further and take the game half as an
// argument too, because which trip *replays* that call and which skips it is the whole difference
// between their answers and their siblings'.
macro_rules! live_skip_void_frame {
    ($hook:ident, $type:ty) => {
        live_skip_void_frame!($hook, $type, should_remove_live_camera_effects());
    };

    ($hook:ident, $type:ty, $remove_effects:expr) => {
        def_detour! {
            $hook(this: *mut Il2CppObject, sheet: *mut Il2CppObject, current_frame: i32) {
                if $remove_effects {
                    return;
                }

                get_orig_fn!($hook, $type)(this, sheet, current_frame);
            } bail {
                get_orig_fn!($hook, $type)(this, sheet, current_frame);
            }
        }
    };
}

macro_rules! live_main_camera_void_frame {
    ($hook:ident, $type:ty) => {
        live_main_camera_void_frame!($hook, $type, should_override_live_camera());
    };

    ($hook:ident, $type:ty, $override_camera:expr) => {
        def_detour! {
            $hook(this: *mut Il2CppObject, sheet: *mut Il2CppObject, current_frame: i32) {
                if $override_camera {
                    return;
                }

                get_orig_fn!($hook, $type)(this, sheet, current_frame);
            } bail {
                get_orig_fn!($hook, $type)(this, sheet, current_frame);
            }
        }
    };
}

// The two secondary-camera wrappers mark the depth the game's own nested calls read
// (`LiveTimelineKeyCameraPositionData::GetValue`, `LiveTimelineKeyCameraLookAtData::GetValue` and
// `GetValue2` answer differently inside a secondary camera update). The depth is taken in the
// wrapper frame and *not* inside the guarded body: the C frame stops a fault by returning out of
// the frames it wrapped, and those frames' destructors never run, so a guard taken inside the body
// would leave the Live secondary-camera depth raised for the rest of the session the first time the
// barrier stopped a fault. Out here the wrapper returns through the barrier's answer and the guard
// drops on the way out - and, because `def_detour!`'s `prelude` arm holds it across the whole call,
// the `Panicked` trip replays the game's update *inside* the depth that update's own nested calls
// read, which is the state the game handed this wrapper in the first place.
//
// That is the whole reason these two forms need an arm of their own, and it is not a reason to
// write the barrier call out at the site: written that way they were the seven boundaries whose
// `Panicked` answer skipped the game's Live camera update for that frame while every sibling in
// this file handed the call back. Through the arm they take the trip answers the family gives -
// `Panicked` replays the game's call behind `detour_fallback`, `Faulted` replays nothing - and the
// guard is the only thing this file adds to them.
//
// The second arm is the test shape: it takes the mod-side half and the game half as arguments, so
// a test can drive a wrapper built by this same macro with an injected panic or fault on one side
// and a game call it can count on the other. A unit test reaches no trampoline and no
// `Hachimi::instance()` (AGENTS section 4), and the shipped body's `get_orig_fn!` for a hook no
// `init` installed walks into exactly that. No shipped instantiation uses this arm.
macro_rules! live_secondary_camera_void_frame {
    ($hook:ident, $type:ty) => {
        def_detour! {
            $hook(this: *mut Il2CppObject, sheet: *mut Il2CppObject, current_frame: i32)
            prelude {
                free_camera::begin_live_secondary_camera_update()
            } {
                get_orig_fn!($hook, $type)(this, sheet, current_frame);
            } bail {
                get_orig_fn!($hook, $type)(this, sheet, current_frame);
            }
        }
    };

    ($hook:ident, $mod_side:expr, $game_fn:ident) => {
        def_detour! {
            $hook(this: *mut Il2CppObject, sheet: *mut Il2CppObject, current_frame: i32)
            prelude {
                free_camera::begin_live_secondary_camera_update()
            } {
                let _ = $mod_side;
                $game_fn(this, sheet, current_frame);
            } bail {
                $game_fn(this, sheet, current_frame);
            }
        }
    };
}

macro_rules! live_secondary_camera_void_frame_time {
    ($hook:ident, $type:ty) => {
        def_detour! {
            $hook(
                this: *mut Il2CppObject,
                sheet: *mut Il2CppObject,
                current_frame: i32,
                current_time: f32,
            )
            prelude {
                free_camera::begin_live_secondary_camera_update()
            } {
                get_orig_fn!($hook, $type)(this, sheet, current_frame, current_time);
            } bail {
                get_orig_fn!($hook, $type)(this, sheet, current_frame, current_time);
            }
        }
    };

    ($hook:ident, $mod_side:expr, $game_fn:ident) => {
        def_detour! {
            $hook(
                this: *mut Il2CppObject,
                sheet: *mut Il2CppObject,
                current_frame: i32,
                current_time: f32,
            )
            prelude {
                free_camera::begin_live_secondary_camera_update()
            } {
                let _ = $mod_side;
                $game_fn(this, sheet, current_frame, current_time);
            } bail {
                $game_fn(this, sheet, current_frame, current_time);
            }
        }
    };
}

live_secondary_camera_void_frame_time!(AlterUpdate_MultiCameraPosition, LiveVoidFrameTimeFn);
live_secondary_camera_void_frame_time!(AlterUpdate_MultiCameraLookAt, LiveVoidFrameTimeFn);
live_secondary_camera_void_frame!(AlterUpdate_MultiCameraRadialBlur, LiveVoidFrameFn);
live_secondary_camera_void_frame_time!(AlterUpdate_EyeCameraPosition, LiveVoidFrameTimeFn);
live_secondary_camera_void_frame_time!(AlterUpdate_MonitorCameraPosition, LiveVoidFrameTimeFn);
live_skip_void_frame!(AlterUpdate_PostEffect_BloomDiffusion, LiveVoidFrameFn);
live_skip_void_frame!(AlterUpdate_TiltShift, LiveVoidFrameFn);
live_main_camera_void_frame!(AlterUpdate_CameraLayer, LiveVoidFrameFn);
live_main_camera_void_frame!(AlterUpdate_CameraSwitcher, LiveVoidFrameFn);
live_main_camera_void_frame!(AlterUpdate_CameraMotion, LiveVoidFrameFn);
live_main_camera_void_frame!(AlterUpdate_HandShakeCamera, LiveVoidFrameFn);
live_secondary_camera_void_frame_time!(AlterUpdate_MonitorCameraLookAt, LiveVoidFrameTimeFn);
live_secondary_camera_void_frame_time!(AlterUpdate_EyeCameraLookAt, LiveVoidFrameTimeFn);

def_detour! {
    AlterLateUpdate_CameraMotion(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
) -> bool {
            if should_override_live_camera() {
            return false;
        }
        get_orig_fn!(AlterLateUpdate_CameraMotion, LiveBoolFrameFn)(this, sheet, current_frame)
    }
}

def_detour! {
    AlterUpdate_CameraFov(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
) {
            let trainer_live_landscape = Director::is_trainer_live() && Hachimi::instance().config.load().trainer_live_landscape;

        if should_override_live_camera() || trainer_live_landscape {
            return;
        }

        get_orig_fn!(AlterUpdate_CameraFov, LiveVoidFrameFn)(this, sheet, current_frame);
    }
}

def_detour! {
    AlterUpdate_CameraRoll(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
) {
            if should_override_live_camera() {
            return;
        }
        get_orig_fn!(AlterUpdate_CameraRoll, LiveVoidFrameFn)(this, sheet, current_frame);
    }
}

type LiveFormationOffsetFn = extern "C" fn(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    character_object_list: *mut Il2CppObject,
    change_visibility: bool,
);
def_detour! {
    AlterUpdate_FormationOffset(
    this: *mut Il2CppObject,
    sheet: *mut Il2CppObject,
    current_frame: i32,
    character_object_list: *mut Il2CppObject,
    mut change_visibility: bool,
) {
            free_camera::set_live_active();
        let disable_teleport = free_camera::should_disable_live_character_teleport();
        let frame = if disable_teleport { 0 } else { current_frame };
        if disable_teleport || free_camera::should_force_live_characters_visible() {
            change_visibility = false;
        }

        // Keep the formation-offset timeline at its initial pose when free camera
        // ignores the authored camera motion. This removes camera-directed teleports
        // without forcing character transform nodes to a shared local position.
        get_orig_fn!(AlterUpdate_FormationOffset, LiveFormationOffsetFn)(
            this,
            sheet,
            frame,
            character_object_list,
            change_visibility,
        );

        Director::apply_live_character_options_to_list(character_object_list);
        apply_current_live_character_options();
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, "Gallop.Live.Cutt", LiveTimelineControl);

    let AlterUpdate_CameraPos_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraPos", 5);
    new_hook!(AlterUpdate_CameraPos_addr, AlterUpdate_CameraPos);

    let AlterUpdate_CameraLookAt_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraLookAt", 4);
    new_hook!(AlterUpdate_CameraLookAt_addr, AlterUpdate_CameraLookAt);

    let LiveTimelineControl_AlterLateUpdate_addr = get_method_addr(LiveTimelineControl, c"AlterLateUpdate", 0);
    new_hook!(LiveTimelineControl_AlterLateUpdate_addr, LiveTimelineControl_AlterLateUpdate);

    let LiveTimelineControl_OnDestroy_addr = get_method_addr(LiveTimelineControl, c"OnDestroy", 0);
    new_hook!(LiveTimelineControl_OnDestroy_addr, LiveTimelineControl_OnDestroy);

    let AlterUpdate_RadialBlur_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_RadialBlur", 2);
    new_hook!(AlterUpdate_RadialBlur_addr, AlterUpdate_RadialBlur);

    let SetupPostFilmUpdateDataInfo_addr = get_method_addr(LiveTimelineControl, c"SetupPostFilmUpdateDataInfo", 4);
    new_hook!(SetupPostFilmUpdateDataInfo_addr, SetupPostFilmUpdateDataInfo);

    let SetupDOFUpdateInfo_addr = get_method_addr(LiveTimelineControl, c"SetupDOFUpdateInfo", 5);
    new_hook!(SetupDOFUpdateInfo_addr, SetupDOFUpdateInfo);

    let SetupRadialBlurInfo_addr = get_method_addr(LiveTimelineControl, c"SetupRadialBlurInfo", 4);
    new_hook!(SetupRadialBlurInfo_addr, SetupRadialBlurInfo);

    let AlterUpdate_MultiCameraRadialBlur_addr = get_method_addr(
        LiveTimelineControl,
        c"AlterUpdate_MultiCameraRadialBlur",
        2,
    );
    new_hook!(AlterUpdate_MultiCameraRadialBlur_addr, AlterUpdate_MultiCameraRadialBlur);

    let AlterUpdate_EyeCameraPosition_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_EyeCameraPosition", 3);
    new_hook!(AlterUpdate_EyeCameraPosition_addr, AlterUpdate_EyeCameraPosition);

    let AlterUpdate_MonitorCameraPosition_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_MonitorCameraPosition", 3);
    new_hook!(AlterUpdate_MonitorCameraPosition_addr, AlterUpdate_MonitorCameraPosition);

    let AlterUpdate_PostEffect_BloomDiffusion_addr = get_method_addr(
        LiveTimelineControl,
        c"AlterUpdate_PostEffect_BloomDiffusion",
        2,
    );
    new_hook!(AlterUpdate_PostEffect_BloomDiffusion_addr, AlterUpdate_PostEffect_BloomDiffusion);

    let AlterUpdate_TiltShift_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_TiltShift", 2);
    new_hook!(AlterUpdate_TiltShift_addr, AlterUpdate_TiltShift);

    let AlterUpdate_CameraLayer_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraLayer", 2);
    new_hook!(AlterUpdate_CameraLayer_addr, AlterUpdate_CameraLayer);

    let AlterUpdate_CameraFov_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraFov", 2);
    new_hook!(AlterUpdate_CameraFov_addr, AlterUpdate_CameraFov);

    let AlterUpdate_CameraRoll_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraRoll", 2);
    new_hook!(AlterUpdate_CameraRoll_addr, AlterUpdate_CameraRoll);

    let AlterUpdate_CameraMotion_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraMotion", 2);
    new_hook!(AlterUpdate_CameraMotion_addr, AlterUpdate_CameraMotion);

    let AlterLateUpdate_CameraMotion_addr = get_method_addr(LiveTimelineControl, c"AlterLateUpdate_CameraMotion", 2);
    new_hook!(AlterLateUpdate_CameraMotion_addr, AlterLateUpdate_CameraMotion);

    let AlterUpdate_HandShakeCamera_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_HandShakeCamera", 2);
    new_hook!(AlterUpdate_HandShakeCamera_addr, AlterUpdate_HandShakeCamera);

    let AlterUpdate_CameraSwitcher_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_CameraSwitcher", 2);
    new_hook!(AlterUpdate_CameraSwitcher_addr, AlterUpdate_CameraSwitcher);

    let AlterUpdate_MonitorCameraLookAt_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_MonitorCameraLookAt", 3);
    new_hook!(AlterUpdate_MonitorCameraLookAt_addr, AlterUpdate_MonitorCameraLookAt);

    let AlterUpdate_EyeCameraLookAt_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_EyeCameraLookAt", 3);
    new_hook!(AlterUpdate_EyeCameraLookAt_addr, AlterUpdate_EyeCameraLookAt);

    let AlterUpdate_MultiCameraPosition_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_MultiCameraPosition", 3);
    new_hook!(AlterUpdate_MultiCameraPosition_addr, AlterUpdate_MultiCameraPosition);

    let AlterUpdate_MultiCameraLookAt_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_MultiCameraLookAt", 3);
    new_hook!(AlterUpdate_MultiCameraLookAt_addr, AlterUpdate_MultiCameraLookAt);

    let AlterUpdate_FormationOffset_addr = get_method_addr(LiveTimelineControl, c"AlterUpdate_FormationOffset", 4);
    new_hook!(AlterUpdate_FormationOffset_addr, AlterUpdate_FormationOffset);
}

// The wrappers the tests below drive, built by the four macros themselves. They are instantiated
// here - at the scope the macros are defined in, which is the scope the names in their bodies
// resolve in - with the mod-side decision replaced by a fault or a panic, and, for the
// secondary-camera forms, with the game half replaced by a call that counts itself: `get_orig_fn!`
// for a hook no `init` installed falls back to `Hachimi::instance()`, which ends the process
// (AGENTS section 4). Counting it is what lets a test say which trip handed the game its Live
// camera update for that frame and which skipped it, instead of inferring that from a counter.
//
// The faulting read is the one `guard.rs` already uses for the same purpose, and only the target
// whose barrier compiles the SEH half builds any of that; the panicking half is portable.
#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
#[inline(never)]
fn freed_live_object() -> bool {
    let mut value: u64 = 0;
    unsafe { core::arch::asm!("mov rax, qword ptr [0x1000]", out("rax") value) };
    value != 0
}

#[cfg(test)]
#[inline(never)]
fn live_mod_side_that_panics() -> bool {
    // A `bool`, not a `!`: the divergence is inside the injected half, so the wrapper's own body
    // after it is ordinary code and the test drives the trip the way the game does.
    panic!("the mod's own half of a Live camera update stopped");
}

#[cfg(test)]
static SECONDARY_GAME_CALLS: AtomicUsize = AtomicUsize::new(0);
/// ... of those calls, the ones that ran while the Live secondary-camera depth was raised: the
/// state the wrapper's own guard is there to give the game's nested calls.
#[cfg(test)]
static SECONDARY_GAME_CALLS_INSIDE_DEPTH: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
fn secondary_game_calls() -> usize {
    SECONDARY_GAME_CALLS.load(Ordering::Relaxed)
}

#[cfg(test)]
fn secondary_game_calls_inside_depth() -> usize {
    SECONDARY_GAME_CALLS_INSIDE_DEPTH.load(Ordering::Relaxed)
}

#[cfg(test)]
fn count_secondary_game_call() {
    SECONDARY_GAME_CALLS.fetch_add(1, Ordering::Relaxed);

    if free_camera::is_live_secondary_camera_update() {
        SECONDARY_GAME_CALLS_INSIDE_DEPTH.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[inline(never)]
fn counted_secondary_camera_update(
    _this: *mut Il2CppObject,
    _sheet: *mut Il2CppObject,
    _current_frame: i32,
) {
    count_secondary_game_call();
}

#[cfg(test)]
#[inline(never)]
fn counted_secondary_camera_update_time(
    _this: *mut Il2CppObject,
    _sheet: *mut Il2CppObject,
    _current_frame: i32,
    _current_time: f32,
) {
    count_secondary_game_call();
}

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
live_skip_void_frame!(InjectedPostEffectBloomDiffusion, LiveVoidFrameFn, freed_live_object());

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
live_main_camera_void_frame!(InjectedCameraLayer, LiveVoidFrameFn, freed_live_object());

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
live_secondary_camera_void_frame!(
    InjectedSecondaryRadialBlurFaulting,
    freed_live_object(),
    counted_secondary_camera_update
);

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
live_secondary_camera_void_frame_time!(
    InjectedSecondaryEyePositionFaulting,
    freed_live_object(),
    counted_secondary_camera_update_time
);

#[cfg(test)]
live_secondary_camera_void_frame!(
    InjectedSecondaryMonitorPositionPanicking,
    live_mod_side_that_panics(),
    counted_secondary_camera_update
);

#[cfg(test)]
live_secondary_camera_void_frame_time!(
    InjectedSecondaryMonitorLookAtPanicking,
    live_mod_side_that_panics(),
    counted_secondary_camera_update_time
);

#[cfg(test)]
#[inline(never)]
fn live_mod_side_that_decides_nothing() -> bool {
    false
}

#[cfg(test)]
live_secondary_camera_void_frame!(
    InjectedSecondaryRadialBlurClean,
    live_mod_side_that_decides_nothing(),
    counted_secondary_camera_update
);

#[cfg(test)]
live_secondary_camera_void_frame_time!(
    InjectedSecondaryEyePositionClean,
    live_mod_side_that_decides_nothing(),
    counted_secondary_camera_update_time
);

#[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
mod tests {
    use crate::{
        il2cpp::{hook::guard, types::Il2CppObject},
        windows::free_camera,
    };

    use super::{
        InjectedCameraLayer, InjectedPostEffectBloomDiffusion, InjectedSecondaryEyePositionClean,
        InjectedSecondaryEyePositionFaulting, InjectedSecondaryMonitorLookAtPanicking,
        InjectedSecondaryMonitorPositionPanicking, InjectedSecondaryRadialBlurClean,
        InjectedSecondaryRadialBlurFaulting, secondary_game_calls,
        secondary_game_calls_inside_depth,
    };

    // The frame every Live frame pays for: one game call per boundary, taken while the wrapper's own
    // guard is raised, and no barrier trip at all. The arm's `Done` answer must not fall through to
    // the bail, or the game would get its camera update twice per frame.
    #[test]
    fn a_clean_secondary_camera_call_hands_the_game_exactly_one_update() {
        let _turn = guard::barrier_turn();
        let before_calls = secondary_game_calls();
        let before_inside_depth = secondary_game_calls_inside_depth();
        let before_panics = guard::panic_trip_count();
        let before_faults = guard::fault_trip_count();

        let radial: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32) =
            InjectedSecondaryRadialBlurClean;
        radial(std::ptr::null_mut(), std::ptr::null_mut(), 0);

        let timed: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32, f32) =
            InjectedSecondaryEyePositionClean;
        timed(std::ptr::null_mut(), std::ptr::null_mut(), 0, 0.0);

        assert_eq!(secondary_game_calls(), before_calls + 2, "one call per boundary, not two");
        assert_eq!(
            secondary_game_calls_inside_depth(),
            before_inside_depth + 2,
            "the game's update ran outside the Live secondary-camera depth the wrapper is there to \
             raise"
        );
        assert!(!free_camera::is_live_secondary_camera_update(), "the guard was not released");
        assert_eq!(guard::panic_trip_count(), before_panics);
        assert_eq!(guard::fault_trip_count(), before_faults);
    }

    #[test]
    fn a_live_camera_wrapper_fault_is_taken_at_the_boundary() {
        let _turn = guard::barrier_turn();
        let before_faults = guard::fault_trip_count();
        let before_panics = guard::panic_trip_count();

        let skip: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32) = InjectedPostEffectBloomDiffusion;
        skip(std::ptr::null_mut(), std::ptr::null_mut(), 0);

        let main: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32) = InjectedCameraLayer;
        main(std::ptr::null_mut(), std::ptr::null_mut(), 0);

        assert_eq!(guard::fault_trip_count(), before_faults + 2, "one fault taken per wrapper");
        assert_eq!(guard::panic_trip_count(), before_panics, "neither trip was counted as a panic");
        assert_eq!(guard::last_fault_code(), 0xC0000005, "the C frame stopped an access violation");
    }

    // The claim Barrier item 3 is about: these seven wrappers used to answer a `Panicked` trip by
    // ending, while every sibling in this file answers it by handing the game its call. Both now
    // come from `def_detour!`, and the answer is the arm's.
    #[test]
    fn a_panicking_secondary_camera_wrapper_hands_the_game_its_update() {
        let _turn = guard::barrier_turn();
        let before_panics = guard::panic_trip_count();
        let before_calls = secondary_game_calls();
        let before_calls_inside_depth = secondary_game_calls_inside_depth();

        let position: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32) =
            InjectedSecondaryMonitorPositionPanicking;
        position(std::ptr::null_mut(), std::ptr::null_mut(), 0);

        let look_at: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32, f32) =
            InjectedSecondaryMonitorLookAtPanicking;
        look_at(std::ptr::null_mut(), std::ptr::null_mut(), 0, 0.0);

        assert_eq!(guard::panic_trip_count(), before_panics + 2, "one panic taken per wrapper");
        assert_eq!(
            secondary_game_calls(),
            before_calls + 2,
            "the game's Live camera update was skipped for that frame: the trip answered itself \
             instead of running the bail"
        );
        assert_eq!(
            secondary_game_calls_inside_depth(),
            before_calls_inside_depth + 2,
            "the replayed call ran outside the Live secondary-camera depth the wrapper raised, so \
             the game's own nested GetValue would have answered this frame as a main camera update"
        );
        assert!(
            !free_camera::is_live_secondary_camera_update(),
            "the depth guard was left raised"
        );
    }

    #[cfg(all(test, target_env = "msvc", target_arch = "x86_64"))]
    #[test]
    fn a_faulting_secondary_camera_wrapper_does_not_replay_the_update() {
        let _turn = guard::barrier_turn();
        let before_faults = guard::fault_trip_count();
        let before_panics = guard::panic_trip_count();
        let before_calls = secondary_game_calls();

        assert!(!free_camera::is_live_secondary_camera_update(), "the depth starts flat");

        let radial: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32) =
            InjectedSecondaryRadialBlurFaulting;
        radial(std::ptr::null_mut(), std::ptr::null_mut(), 0);
        assert!(
            !free_camera::is_live_secondary_camera_update(),
            "a stopped fault left the Live secondary-camera depth raised: the guard was taken \
             inside the barrier body"
        );

        let timed: extern "C" fn(*mut Il2CppObject, *mut Il2CppObject, i32, f32) =
            InjectedSecondaryEyePositionFaulting;
        timed(std::ptr::null_mut(), std::ptr::null_mut(), 0, 0.0);
        assert!(!free_camera::is_live_secondary_camera_update(), "the same holds for the timed form");

        assert_eq!(guard::fault_trip_count(), before_faults + 2, "one fault taken per wrapper");
        assert_eq!(guard::panic_trip_count(), before_panics, "neither trip was counted as a panic");
        assert_eq!(guard::last_fault_code(), 0xC0000005, "the C frame stopped an access violation");
        assert_eq!(
            secondary_game_calls(),
            before_calls,
            "a Faulted trip replayed the call whose state just faulted, into a method the body may \
             already have been inside"
        );
    }
}
