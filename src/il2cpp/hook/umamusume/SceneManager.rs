use std::sync::atomic::{self, AtomicBool};
use crate::{
    core::{Hachimi, game::Region},
    il2cpp::{
        symbols::{get_field_from_name, get_method, get_method_addr, SingletonLike},
        types::*
    }
};
use super::SceneDefine::{ViewId, SceneId};

static SPLASH_SHOWN: AtomicBool = AtomicBool::new(false);
pub fn is_splash_shown() -> bool {
    SPLASH_SHOWN.load(atomic::Ordering::Acquire)
}

static HOME_INIT: AtomicBool = AtomicBool::new(false);
pub fn is_home_init() -> bool {
    HOME_INIT.load(atomic::Ordering::Acquire)
}

static mut CLASS: *mut Il2CppClass = 0 as _;
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

// `get_Instance` resolved once, kept as a number because a static cannot hold a raw pointer. The frame
// clock asks for the singleton once per game tick, and `SingletonLike::new` finds the method by walking
// the class's method table, which is a lookup per tick for no information the class does not already give.
static mut GET_INSTANCE_METHOD: usize = 0;

pub fn instance() -> *mut Il2CppObject {
    let cached = unsafe { GET_INSTANCE_METHOD };

    // The uncached route stays available for a call that arrives before init resolved anything.
    let singleton = if cached != 0 {
        SingletonLike::from_method_ptr(cached)
    }
    else {
        match SingletonLike::new(class()) {
            Some(singleton) => singleton,
            None => return 0 as _,
        }
    };

    singleton.instance()
}

def_field_object_accessors!(get_PhotoCheckObject, set_PhotoCheckObject, PHOTOCHECKOBJECT_FIELD, *mut Il2CppObject);
def_field_object_accessors!(get_PhotoLibraryObject, set_PhotoLibraryObject, PHOTOLIBRARYOBJECT_FIELD, *mut Il2CppObject);

static mut GETCURRENTVIEWID_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetCurrentViewId, GETCURRENTVIEWID_ADDR, i32, this: *mut Il2CppObject);

static mut GETCURRENTSCENEID_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetCurrentSceneId, GETCURRENTSCENEID_ADDR, SceneId, this: *mut Il2CppObject);

static SCENE_ID_CACHE: atomic::AtomicI32 = atomic::AtomicI32::new(0);

type AlterUpdateFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn AlterUpdate(this: *mut Il2CppObject) {
    get_orig_fn!(AlterUpdate, AlterUpdateFn)(this);
    SCENE_ID_CACHE.store(GetCurrentSceneId(this) as i32, atomic::Ordering::Release);
}

pub fn current_scene_id() -> SceneId {
    unsafe { std::mem::transmute(SCENE_ID_CACHE.load(atomic::Ordering::Acquire)) }
}

pub fn is_race_scene_family() -> bool {
    let id = current_scene_id();
    matches!(
        id,
        SceneId::Race
        | SceneId::DailyRace
        | SceneId::LegendRace
        | SceneId::TeamStadium
        | SceneId::Champions
        | SceneId::ChallengeMatch
        | SceneId::RoomMatch
        | SceneId::PracticeRace
        | SceneId::TrainingChallenge
        | SceneId::Heroes
        | SceneId::UltimateRace
    )
}

static mut GETCURRENTVIEWCONTROLLER_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetCurrentViewController, GETCURRENTVIEWCONTROLLER_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);

fn ChangeViewCommon(next_view_id: i32) {
    if next_view_id == ViewId::Splash {
        SPLASH_SHOWN.store(true, atomic::Ordering::Release);
        debug!("SPLASH_SHOWN: {}", SPLASH_SHOWN.load(atomic::Ordering::Acquire));
    }
    if next_view_id == ViewId::Home && !HOME_INIT.swap(true, atomic::Ordering::AcqRel) {
        #[cfg(target_os = "windows")]
        {
            use crate::windows::{smtc, wnd_hook::get_target_hwnd};
            if Hachimi::instance().config.load().windows.enable_smtc {
                smtc::init(get_target_hwnd());
            }
        }
    }
    debug!("next_view_id = {}", next_view_id);

    // Nothing speed related runs here. `AnimationSpeed::apply()` already ran in the ChangeView
    // wrapper before the original call, which is the only point that can shorten this
    // transition, because Gallop reads its fade constants while ChangeView runs. A second pass
    // after the change completes re-reads the config and re-runs the `HighSpeedSetting` getter
    // chain with its log line for groups `APPLIED_FACTORS` has already marked as written, and
    // it is what doubled the `HighSpeedSetting: max ...` snapshots the ledger counts scene
    // changes from.
    //
    // `Time::apply()` is not here either. Gallop sets its own Time.timeScale around view
    // changes, and the hook on set_timeScale scales every value the game writes, so a view
    // change has nothing to re-assert. Re-applying the configured lever here is what used to
    // overwrite a pause or a game fast forward with the config value; `Time::apply()` now runs
    // once per config change instead.
}

type ChangeViewJpfn = extern "C" fn(
    this: *mut Il2CppObject, next_view_id: i32, view_info: *mut Il2CppObject,
    callback_on_change_view_cancel: *mut Il2CppObject, callback_on_change_view_accept: *mut Il2CppObject,
    force_change: bool, is_fast_destroy: bool, fade_in_duration: f32
);
extern "C" fn ChangeViewJp(
    this: *mut Il2CppObject, next_view_id: i32, view_info: *mut Il2CppObject,
    callback_on_change_view_cancel: *mut Il2CppObject, callback_on_change_view_accept: *mut Il2CppObject,
    force_change: bool, is_fast_destroy: bool, fade_in_duration: f32
) {
    // The one speed apply of this view change, and it has to sit before the original call:
    // the game reads its fade constants while ChangeView runs, so a change saved in Config
    // Editor has to be in place first. `ChangeViewCommon` does not apply it a second time.
    crate::il2cpp::hook::umamusume::AnimationSpeed::apply();

    get_orig_fn!(ChangeViewJp, ChangeViewJpfn)(
        this, next_view_id, view_info, callback_on_change_view_cancel,
        callback_on_change_view_accept, force_change, is_fast_destroy,
        fade_in_duration
    );
    ChangeViewCommon(next_view_id);
}

type ChangeViewOtherfn = extern "C" fn(
    this: *mut Il2CppObject, next_view_id: i32, view_info: *mut Il2CppObject,
    callback_on_change_view_cancel: *mut Il2CppObject, callback_on_change_view_accept: *mut Il2CppObject,
    force_change: bool
);
extern "C" fn ChangeViewOther(
    this: *mut Il2CppObject, next_view_id: i32, view_info: *mut Il2CppObject,
    callback_on_change_view_cancel: *mut Il2CppObject, callback_on_change_view_accept: *mut Il2CppObject,
    force_change: bool
) {
    // The one speed apply of a view change on this client, before the original call, for the
    // same reason as the Japan path: Gallop reads this transition's fade constants inside
    // ChangeView, and `ChangeViewCommon` does not apply it a second time.
    crate::il2cpp::hook::umamusume::AnimationSpeed::apply();

    get_orig_fn!(ChangeViewOther, ChangeViewOtherfn)(
        this, next_view_id, view_info, callback_on_change_view_cancel,
        callback_on_change_view_accept, force_change
    );
    ChangeViewCommon(next_view_id);
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, SceneManager);

    unsafe {
        CLASS = SceneManager;
        GET_INSTANCE_METHOD = get_method(SceneManager, c"get_Instance", 0)
            .map(|method| method as usize)
            .unwrap_or(0);
        GETCURRENTVIEWID_ADDR = get_method_addr(SceneManager, c"GetCurrentViewId", 0);
        GETCURRENTSCENEID_ADDR = get_method_addr(SceneManager, c"GetCurrentSceneId", 0);
        PHOTOCHECKOBJECT_FIELD = get_field_from_name(SceneManager, c"PhotoCheckObject");
        PHOTOLIBRARYOBJECT_FIELD = get_field_from_name(SceneManager, c"PhotoLibraryObject");

        let mut iter: *mut std::ffi::c_void = std::ptr::null_mut();
        loop {
            let method = crate::il2cpp::api::il2cpp_class_get_methods(SceneManager, &mut iter);
            if method.is_null() { break; }
            let name = std::ffi::CStr::from_ptr((*method).name).to_string_lossy();
            if name == "GetCurrentViewController" && (*method).is_generic() == 0 {
                GETCURRENTVIEWCONTROLLER_ADDR = (*method).methodPointer;
                break;
            }
        }

        if GETCURRENTVIEWCONTROLLER_ADDR == 0 {
            error!("Failed to find non-generic GetCurrentViewController on SceneManager");
        }
    }

    if Hachimi::instance().game.region == Region::Japan {
        let ChangeView_addr = get_method_addr(SceneManager, c"ChangeView", 7);
        new_hook!(ChangeView_addr, ChangeViewJp);
    }
    else {
        let ChangeView_addr = get_method_addr(SceneManager, c"ChangeView", 5);
        new_hook!(ChangeView_addr, ChangeViewOther);
    }

    let AlterUpdate_addr = get_method_addr(SceneManager, c"AlterUpdate", 0);
    new_hook!(AlterUpdate_addr, AlterUpdate);
}
