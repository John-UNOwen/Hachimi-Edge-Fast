use std::ffi::{c_void, CStr};
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use crate::il2cpp::{
    api::{il2cpp_class_get_fields, il2cpp_class_get_methods, il2cpp_field_get_name, il2cpp_field_get_offset, il2cpp_field_get_type},
    ext::{Il2CppObjectExt, Il2CppStringExt},
    symbols::{get_field_from_name, get_field_value, get_method_addr},
    types::*,
};

use crate::core::Hachimi;

use super::{AnimationSpeed, TrainingCuttProbe};

// Skill pop-up probes (ledger item 112). The skill banners on the right side of a race are drawn by one of these
// calls. `RaceManager.PlaySkillCutIn` showed none of them, so each candidate is logged with its raw arguments: the
// first POPUP_LOG_LIMIT calls on its own class. Passive: every detour calls the original with the arguments it was
// given, and the original's result is the game's own.
//
// The by-reference parameters (`CutInRequestInfo&`, `PlayDesc&`) are passed as pointers and logged as addresses only.
// They are looked up with `get_method_addr`, because `resolve_method` does not match a list that mixes by-reference
// and value parameters, and each of these names has one overload in the dump. An address shared with another probe
// is armed once (`fresh`), and each detour counts only calls on its own class, since a shared address reaches it with
// another class's `this` (ledger item 112).
// A whole race's pop-ups fit under this limit. At 60 the first race's log ran out before the proc at second 47.
const POPUP_LOG_LIMIT: usize = 200;

const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const ONE_CLASS: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS];
const BOOL_TWO_CLASS: &[Il2CppTypeEnum] = &[
    Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
    Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
    Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
];
const STRING_CLASS: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_STRING, Il2CppTypeEnum_IL2CPP_TYPE_CLASS];
const ADD_CUT_PARAMS: &[Il2CppTypeEnum] = &[
    Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
    Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE,
    Il2CppTypeEnum_IL2CPP_TYPE_I4,
    Il2CppTypeEnum_IL2CPP_TYPE_STRING,
    Il2CppTypeEnum_IL2CPP_TYPE_R4,
];

static mut EVENT_PLAYER_CLASS: *mut Il2CppClass = 0 as _;
static mut MESSAGE_PLATE_CLASS: *mut Il2CppClass = 0 as _;
static mut RACE_UI_CLASS: *mut Il2CppClass = 0 as _;
static mut MAIN_VIEW_CLASS: *mut Il2CppClass = 0 as _;
static mut CAMERA_MANAGER_CLASS: *mut Il2CppClass = 0 as _;
static mut CUT_HELPER_CLASS: *mut Il2CppClass = 0 as _;
static mut CUT_RESERVE_CLASS: *mut Il2CppClass = 0 as _;

static EVENT_TRY_SEEN: AtomicUsize = AtomicUsize::new(0);
static MESSAGE_REQUEST_SEEN: AtomicUsize = AtomicUsize::new(0);
static RACE_UI_MESSAGE_SEEN: AtomicUsize = AtomicUsize::new(0);
static MAIN_MESSAGE_SEEN: AtomicUsize = AtomicUsize::new(0);
static RACE_UI_BELT_SEEN: AtomicUsize = AtomicUsize::new(0);
static MAIN_BELT_SEEN: AtomicUsize = AtomicUsize::new(0);
static CAMERA_STEAM_SEEN: AtomicUsize = AtomicUsize::new(0);
static CUT_PLAY_SEEN: AtomicUsize = AtomicUsize::new(0);
static ADD_CUT_SEEN: AtomicUsize = AtomicUsize::new(0);

// `RaceEventPlayer.IsPlayableCutIn` decides, once per skill proc at race time, whether that cut-in plays (ledger item
// 112). Its result is logged here and passed through unchanged.
static PLAYABLE_SEEN: AtomicUsize = AtomicUsize::new(0);
const PLAYABLE_PARAMS: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_I4];

// Activation probes (ledger item 112). A skill proc is the change from not activated to activated on one skill
// object, read through the getters below before and after each activation call. The getters are looked up on the
// `SkillDetail` class and checked against their return type, so a read through them has the type this code expects.
const PROC_LOG_LIMIT: usize = 300;
static PROC_LOGGED: AtomicUsize = AtomicUsize::new(0);
static GET_SKILL_ID_ADDR: AtomicUsize = AtomicUsize::new(0);
static GET_IS_ACTIVATED_ADDR: AtomicUsize = AtomicUsize::new(0);
static mut SKILL_DETAIL_CLASS: *mut Il2CppClass = 0 as _;

// The skill effect queue (ledger item 112). `SkillEffectPlayer` keeps the effects it has yet to play in
// `_delayPlayList`, and a proc's effect is queued there. The spark probe's `UpdateDelayPlay` detour reads the count
// through `note_effect_queue`, so a change in the queue is logged on the frame it shows up, with no hook of its own.
const EFFECT_QUEUE_LOG_LIMIT: usize = 300;
static EFFECT_QUEUE_LAST: AtomicI32 = AtomicI32::new(-2);
static EFFECT_QUEUE_LOGGED: AtomicUsize = AtomicUsize::new(0);
static mut EFFECT_PLAYER_CLASS: *mut Il2CppClass = 0 as _;
static mut EFFECT_LIST_FIELD: *mut FieldInfo = 0 as _;
static mut EFFECT_LIST_SIZE_FIELD: *mut FieldInfo = 0 as _;

/// The call count when this call is on `class` and still under the log limit, `None` otherwise.
fn counted(seen: &AtomicUsize, class: *mut Il2CppClass, this: *mut Il2CppObject) -> Option<usize> {
    if this.is_null() || class.is_null() || unsafe { (*this).klass() } != class {
        return None;
    }

    let n = seen.fetch_add(1, Ordering::Relaxed) + 1;
    (n <= POPUP_LOG_LIMIT).then_some(n)
}

/// The name of an object's class, for the skill objects the probes see (`SkillBase` or another implementation).
fn class_name(obj: *mut Il2CppObject) -> String {
    if obj.is_null() {
        return String::from("(null)");
    }

    let klass = unsafe { (*obj).klass() };
    if klass.is_null() {
        return String::from("(no class)");
    }

    unsafe { CStr::from_ptr((*klass).name) }.to_string_lossy().into_owned()
}

/// Logs every method name and parameter count of one class, once, at startup. The dump filters these out, so the
/// names the next probe needs are read from the game here.
fn list_methods(label: &str, class: *mut Il2CppClass) {
    if class.is_null() {
        info!("Skill methods {label}: class not found");
        return;
    }

    let mut iter: *mut c_void = std::ptr::null_mut();
    loop {
        let method = il2cpp_class_get_methods(class, &mut iter);
        if method.is_null() {
            break;
        }

        let name = unsafe { CStr::from_ptr((*method).name) }.to_string_lossy();
        let arity = unsafe { (*method).parameters_count };
        info!("Skill methods {label}: {name}/{arity}");
    }
}

/// Logs every field of one class, once, at startup: its name, its offset and its IL2CPP type enum. Used on the plate
/// class's `PlayDesc`, whose fields the dump does not carry.
fn list_fields(label: &str, class: *mut Il2CppClass) {
    if class.is_null() {
        info!("Skill fields {label}: class not found");
        return;
    }

    let mut iter: *mut c_void = std::ptr::null_mut();
    loop {
        let field = il2cpp_class_get_fields(class, &mut iter);
        if field.is_null() {
            break;
        }

        let name = unsafe { CStr::from_ptr(il2cpp_field_get_name(field)) }.to_string_lossy();
        let offset = il2cpp_field_get_offset(field);
        let field_type = il2cpp_field_get_type(field);
        let type_enum: i64 = if field_type.is_null() { -1 } else { (unsafe { (*field_type).type_() }) as i64 };
        info!("Skill fields {label}: {name} at offset {offset:#x}, type enum {type_enum}");
    }
}

/// Reads one `PlayDesc` argument at the offsets `list_fields` found on the game: type 0x10, text 0x18, master skill
/// 0x20, chara 0x28, horse index 0x30, enemy 0x34. The argument is a reference to the object, so it is dereferenced
/// once, and a null at any step is reported rather than read.
fn describe_play(play: *mut c_void) -> String {
    if play.is_null() {
        return String::from("(null play)");
    }

    let desc = unsafe { *(play as *const *mut u8) };
    if desc.is_null() {
        return String::from("(null desc)");
    }

    let plate_type = unsafe { *(desc.add(0x10) as *const i32) };
    let text_ptr = unsafe { *(desc.add(0x18) as *const *mut Il2CppString) };
    let text = if text_ptr.is_null() { String::from("(null)") } else { unsafe { (*text_ptr).as_utf16str() }.to_string() };
    let master_skill = unsafe { *(desc.add(0x20) as *const *mut u8) };
    let chara_id = unsafe { *(desc.add(0x28) as *const i32) };
    let horse_index = unsafe { *(desc.add(0x30) as *const i32) };
    let is_enemy = unsafe { *(desc.add(0x34) as *const u8) } != 0;
    let skill = if master_skill.is_null() { "none" } else { "set" };
    format!("type {plate_type}, text {text:?}, master skill {skill}, chara {chara_id}, horse index {horse_index}, enemy {is_enemy}")
}

/// The belt's enable flag as the game should see it: forced off while `hide_skill_cut_ins` is on. Only the switch
/// changes, so the game's own call still runs and its completion behaviour is unchanged. Off when the game asked for
/// off already.
fn belt_enable(enable: bool) -> bool {
    enable && !Hachimi::instance().config.load().hide_skill_cut_ins
}

/// Whether a `PlayDesc` argument carries a master skill, read at the offset `describe_play` uses. A null anywhere is no.
fn plate_has_master_skill(play: *mut c_void) -> bool {
    if play.is_null() {
        return false;
    }

    let desc = unsafe { *(play as *const *mut u8) };
    if desc.is_null() {
        return false;
    }

    !unsafe { *(desc.add(0x20) as *const *mut u8) }.is_null()
}

/// The method pointer of `class`'s own method `name`, with `argc` parameters and return type `ret`, or 0. The return
/// type is checked, so a call through the address is made with the signature the caller writes.
fn method_addr(class: *mut Il2CppClass, name: &str, argc: u8, ret: Il2CppTypeEnum) -> usize {
    if class.is_null() {
        return 0;
    }

    let mut iter: *mut c_void = std::ptr::null_mut();
    loop {
        let method = il2cpp_class_get_methods(class, &mut iter);
        if method.is_null() {
            break;
        }

        let method_name = unsafe { CStr::from_ptr((*method).name) };
        if method_name.to_str() != Ok(name) || unsafe { (*method).parameters_count } != argc {
            continue;
        }

        let return_type = unsafe { (*method).return_type };
        if return_type.is_null() || unsafe { (*return_type).type_() } != ret {
            continue;
        }

        return unsafe { (*method).methodPointer };
    }

    0
}

/// The skill's ID and whether it is activated, when `this` is a `SkillDetail` and both getters resolved.
fn skill_state(this: *mut Il2CppObject) -> Option<(i32, bool)> {
    let id_addr = GET_SKILL_ID_ADDR.load(Ordering::Relaxed);
    let active_addr = GET_IS_ACTIVATED_ADDR.load(Ordering::Relaxed);
    if this.is_null() || id_addr == 0 || active_addr == 0 || unsafe { (*this).klass() } != unsafe { SKILL_DETAIL_CLASS } {
        return None;
    }

    let id = unsafe { std::mem::transmute::<usize, SkillIdFn>(id_addr)(this) };
    let active = unsafe { std::mem::transmute::<usize, SkillActiveFn>(active_addr)(this) };
    Some((id, active))
}

/// Logs a proc: the skill went from not activated to activated across one activation call.
fn note_proc(source: &str, this: *mut Il2CppObject, before: Option<(i32, bool)>, after: Option<(i32, bool)>) {
    if let (Some((id, false)), Some((_, true))) = (before, after) {
        let n = PROC_LOGGED.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= PROC_LOG_LIMIT {
            info!("Skill proc {n} from {source}: skill id {id}, object {:#x}", this as usize);
        }
    }
}

/// The number of effects waiting in the queue, or -1 when the field is not there.
fn effect_queue_size(this: *mut Il2CppObject) -> i32 {
    let list_field = unsafe { EFFECT_LIST_FIELD };
    if list_field.is_null() {
        return -1;
    }

    let list: *mut Il2CppObject = get_field_value(this, list_field);
    if list.is_null() {
        return 0;
    }

    // The `_size` field of the generic list, found once from the list's own class.
    let mut size_field = unsafe { EFFECT_LIST_SIZE_FIELD };
    if size_field.is_null() {
        size_field = get_field_from_name(unsafe { (*list).klass() }, c"_size");
        unsafe { EFFECT_LIST_SIZE_FIELD = size_field };
    }
    if size_field.is_null() {
        return -1;
    }

    get_field_value::<i32>(list, size_field)
}

/// Called from the spark probe's `UpdateDelayPlay` detour on each call. Logs only a change in the queue size.
pub(super) fn note_effect_queue(this: *mut Il2CppObject) {
    if this.is_null() || unsafe { (*this).klass() } != unsafe { EFFECT_PLAYER_CLASS } {
        return;
    }

    let size = effect_queue_size(this);
    let last = EFFECT_QUEUE_LAST.swap(size, Ordering::Relaxed);
    if size != last {
        let n = EFFECT_QUEUE_LOGGED.fetch_add(1, Ordering::Relaxed) + 1;
        if n <= EFFECT_QUEUE_LOG_LIMIT {
            info!("Skill effect queue {n}: {last} -> {size} on SkillEffectPlayer");
        }
    }
}

/// Whether `addr` is a new address to arm, with the list of armed addresses kept up to date.
fn fresh(armed: &mut Vec<usize>, addr: usize) -> bool {
    if addr == 0 || armed.contains(&addr) {
        return false;
    }

    armed.push(addr);
    true
}

type TryCutFn = extern "C" fn(this: *mut Il2CppObject, info: *mut c_void, category: i32, horse: *mut Il2CppObject, skill: *mut Il2CppObject);
type PlayDescFn = extern "C" fn(this: *mut Il2CppObject, play: *mut c_void);
type BeltFn = extern "C" fn(this: *mut Il2CppObject, enable: bool, texture: *mut Il2CppObject, material: *mut Il2CppObject);
type CameraFn = extern "C" fn(this: *mut Il2CppObject, camera: *mut Il2CppObject);
type VoidFn = extern "C" fn(this: *mut Il2CppObject);
type SkillIdFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
type SkillActiveFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
type CutNameFn = extern "C" fn(this: *mut Il2CppObject, name: *mut Il2CppString, transform: *mut Il2CppObject);
type PlayableFn = extern "C" fn(this: *mut Il2CppObject, horse: *mut Il2CppObject, skill: *mut Il2CppObject, category: i32) -> bool;

def_detour! {
    SkillPopup_RaceEventPlayer_IsPlayableCutIn(this: *mut Il2CppObject, horse: *mut Il2CppObject, skill: *mut Il2CppObject, category: i32) -> bool {
        let value = get_orig_fn!(SkillPopup_RaceEventPlayer_IsPlayableCutIn, PlayableFn)(this, horse, skill, category);
        // The option is read per skill proc, not per frame, so the config load here is off the hot path.
        let answer = value && !Hachimi::instance().config.load().hide_skill_cut_ins;
        if let Some(n) = counted(&PLAYABLE_SEEN, unsafe { EVENT_PLAYER_CLASS }, this) {
            info!("Playable cut-in {n} from RaceEventPlayer.IsPlayableCutIn: category {category}, skill {}, horse {:#x}, game said {value}, answered {answer}", class_name(skill), horse as usize);
        }
        answer
    }
    bail {
        get_orig_fn!(SkillPopup_RaceEventPlayer_IsPlayableCutIn, PlayableFn)(this, horse, skill, category)
    }
}
def_detour! {
    SkillPopup_RaceEventPlayer_TryRequestCutIn(this: *mut Il2CppObject, info: *mut c_void, category: i32, horse: *mut Il2CppObject, skill: *mut Il2CppObject) {
        if let Some(n) = counted(&EVENT_TRY_SEEN, unsafe { EVENT_PLAYER_CLASS }, this) {
            let skill_id = skill_state(skill).map_or(-1, |state| state.0);
            info!("Skill pop-up {n} from RaceEventPlayer.TryRequestCutIn: category {category}, horse {:#x}, skill {:#x} ({}, id {skill_id}), request {:#x}", horse as usize, skill as usize, class_name(skill), info as usize);
        }
        get_orig_fn!(SkillPopup_RaceEventPlayer_TryRequestCutIn, TryCutFn)(this, info, category, horse, skill);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceEventPlayer_TryRequestCutIn, TryCutFn)(this, info, category, horse, skill)
    }
}

type AddCutFn = extern "C" fn(this: *mut Il2CppObject, horse: *mut Il2CppObject, category: i32, skill_id: i32, name: *mut Il2CppString, seconds: f32);

def_detour! {
    SkillPopup_RaceSkillCutInReserveCreator_AddCutInInfo(this: *mut Il2CppObject, horse: *mut Il2CppObject, category: i32, skill_id: i32, name: *mut Il2CppString, seconds: f32) {
        if let Some(n) = counted(&ADD_CUT_SEEN, unsafe { CUT_RESERVE_CLASS }, this) {
            let shown = if name.is_null() { String::from("(null)") } else { unsafe { (*name).as_utf16str() }.to_string() };
            info!("Skill pop-up {n} from RaceSkillCutInReserveCreator.AddCutInInfo: name {shown:?}, skill id {skill_id}, category {category}, seconds {seconds}, horse {:#x}", horse as usize);
        }
        get_orig_fn!(SkillPopup_RaceSkillCutInReserveCreator_AddCutInInfo, AddCutFn)(this, horse, category, skill_id, name, seconds);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceSkillCutInReserveCreator_AddCutInInfo, AddCutFn)(this, horse, category, skill_id, name, seconds)
    }
}

def_detour! {
    SkillPopup_RaceUIMessagePlate_RequestShowMessagePlate(this: *mut Il2CppObject, play: *mut c_void) {
        // A skill plate is not queued while `hide_skill_cut_ins` is on. Only a plate with a master skill is held back,
        // so status plates pass through. `PlayDesc` has no completion callback, so nothing waits on the skipped call.
        let hide = plate_has_master_skill(play) && Hachimi::instance().config.load().hide_skill_cut_ins;
        if let Some(n) = counted(&MESSAGE_REQUEST_SEEN, unsafe { MESSAGE_PLATE_CLASS }, this) {
            info!("Skill pop-up {n} from RaceUIMessagePlate.RequestShowMessagePlate: play {:#x}, {}, hidden {hide}", play as usize, describe_play(play));
        }
        if !hide {
            get_orig_fn!(SkillPopup_RaceUIMessagePlate_RequestShowMessagePlate, PlayDescFn)(this, play);
        }
    }
    bail {
        get_orig_fn!(SkillPopup_RaceUIMessagePlate_RequestShowMessagePlate, PlayDescFn)(this, play)
    }
}

def_detour! {
    SkillPopup_RaceUI_ShowMessagePlate(this: *mut Il2CppObject, play: *mut c_void) {
        if let Some(n) = counted(&RACE_UI_MESSAGE_SEEN, unsafe { RACE_UI_CLASS }, this) {
            info!("Skill pop-up {n} from RaceUI.ShowMessagePlate: play {:#x}", play as usize);
        }
        get_orig_fn!(SkillPopup_RaceUI_ShowMessagePlate, PlayDescFn)(this, play);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceUI_ShowMessagePlate, PlayDescFn)(this, play)
    }
}

def_detour! {
    SkillPopup_RaceMainViewController_ShowMessagePlate(this: *mut Il2CppObject, play: *mut c_void) {
        if let Some(n) = counted(&MAIN_MESSAGE_SEEN, unsafe { MAIN_VIEW_CLASS }, this) {
            info!("Skill pop-up {n} from RaceMainViewController.ShowMessagePlate: play {:#x}", play as usize);
        }
        get_orig_fn!(SkillPopup_RaceMainViewController_ShowMessagePlate, PlayDescFn)(this, play);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceMainViewController_ShowMessagePlate, PlayDescFn)(this, play)
    }
}

def_detour! {
    SkillPopup_RaceUI_SetSteamSkillCutInBeltEnable(this: *mut Il2CppObject, enable: bool, texture: *mut Il2CppObject, material: *mut Il2CppObject) {
        if let Some(n) = counted(&RACE_UI_BELT_SEEN, unsafe { RACE_UI_CLASS }, this) {
            info!("Skill pop-up {n} from RaceUI.SetSteamSkillCutInBeltEnable: enable {enable}");
        }
        get_orig_fn!(SkillPopup_RaceUI_SetSteamSkillCutInBeltEnable, BeltFn)(this, belt_enable(enable), texture, material);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceUI_SetSteamSkillCutInBeltEnable, BeltFn)(this, enable, texture, material)
    }
}

def_detour! {
    SkillPopup_RaceMainViewController_SetSteamSkillCutInBeltEnable(this: *mut Il2CppObject, enable: bool, texture: *mut Il2CppObject, material: *mut Il2CppObject) {
        if let Some(n) = counted(&MAIN_BELT_SEEN, unsafe { MAIN_VIEW_CLASS }, this) {
            info!("Skill pop-up {n} from RaceMainViewController.SetSteamSkillCutInBeltEnable: enable {enable}");
        }
        get_orig_fn!(SkillPopup_RaceMainViewController_SetSteamSkillCutInBeltEnable, BeltFn)(this, belt_enable(enable), texture, material);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceMainViewController_SetSteamSkillCutInBeltEnable, BeltFn)(this, enable, texture, material)
    }
}

def_detour! {
    SkillPopup_RaceCameraManager_StartSteamLandscapeSkillCutIn(this: *mut Il2CppObject, camera: *mut Il2CppObject) {
        if let Some(n) = counted(&CAMERA_STEAM_SEEN, unsafe { CAMERA_MANAGER_CLASS }, this) {
            info!("Skill pop-up {n} from RaceCameraManager.StartSteamLandscapeSkillCutIn: camera {:#x}", camera as usize);
        }
        get_orig_fn!(SkillPopup_RaceCameraManager_StartSteamLandscapeSkillCutIn, CameraFn)(this, camera);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceCameraManager_StartSteamLandscapeSkillCutIn, CameraFn)(this, camera)
    }
}

def_detour! {
    SkillPopup_RaceSkillCutInHelper_OnPlayCutIn(this: *mut Il2CppObject, name: *mut Il2CppString, transform: *mut Il2CppObject) {
        if let Some(n) = counted(&CUT_PLAY_SEEN, unsafe { CUT_HELPER_CLASS }, this) {
            let shown = if name.is_null() { String::from("(null)") } else { unsafe { (*name).as_utf16str() }.to_string() };
            info!("Skill pop-up {n} from RaceSkillCutInHelper.OnPlayCutIn: cut-in {shown:?}");
        }
        get_orig_fn!(SkillPopup_RaceSkillCutInHelper_OnPlayCutIn, CutNameFn)(this, name, transform);
    }
    bail {
        get_orig_fn!(SkillPopup_RaceSkillCutInHelper_OnPlayCutIn, CutNameFn)(this, name, transform)
    }
}

def_detour! {
    SkillProc_SkillBase_CheckTriggerAndActivate(this: *mut Il2CppObject) {
        let before = skill_state(this);
        get_orig_fn!(SkillProc_SkillBase_CheckTriggerAndActivate, VoidFn)(this);
        note_proc("SkillBase.CheckTriggerAndActivate", this, before, skill_state(this));
    }
    bail {
        get_orig_fn!(SkillProc_SkillBase_CheckTriggerAndActivate, VoidFn)(this)
    }
}

def_detour! {
    SkillProc_SkillBase_LotActivate(this: *mut Il2CppObject) {
        let before = skill_state(this);
        get_orig_fn!(SkillProc_SkillBase_LotActivate, VoidFn)(this);
        note_proc("SkillBase.LotActivate", this, before, skill_state(this));
    }
    bail {
        get_orig_fn!(SkillProc_SkillBase_LotActivate, VoidFn)(this)
    }
}

def_detour! {
    SkillProc_SkillDetail_Activate(this: *mut Il2CppObject) {
        let before = skill_state(this);
        get_orig_fn!(SkillProc_SkillDetail_Activate, VoidFn)(this);
        note_proc("SkillDetail.Activate", this, before, skill_state(this));
    }
    bail {
        get_orig_fn!(SkillProc_SkillDetail_Activate, VoidFn)(this)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    let mut armed: Vec<usize> = Vec::new();

    if let Some(player) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.SkillEffectPlayer") {
        unsafe {
            EFFECT_PLAYER_CLASS = player;
            EFFECT_LIST_FIELD = get_field_from_name(player, c"_delayPlayList");
        }
        if unsafe { EFFECT_LIST_FIELD }.is_null() {
            info!("Skill effect queue: _delayPlayList not found, the queue is not counted");
        }
    }

    // The skill object's own getters and activation calls. A class the game does not carry arms nothing, and says so.
    match TrainingCuttProbe::class_for_label(umamusume, "Gallop.SkillDetail") {
        Some(detail) => {
            unsafe { SKILL_DETAIL_CLASS = detail };
            list_methods("SkillDetail", detail);
            GET_SKILL_ID_ADDR.store(method_addr(detail, "get_SkillId", 0, Il2CppTypeEnum_IL2CPP_TYPE_I4), Ordering::Relaxed);
            GET_IS_ACTIVATED_ADDR.store(method_addr(detail, "get_IsActivated", 0, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN), Ordering::Relaxed);
            let activate = method_addr(detail, "Activate", 0, VOID);
            if fresh(&mut armed, activate) {
                new_hook!(activate, SkillProc_SkillDetail_Activate);
            }
        }
        None => info!("Skill proc: Gallop.SkillDetail not found, no activation probe armed"),
    }

    if let Some(base) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.SkillBase") {
        let check = method_addr(base, "CheckTriggerAndActivate", 0, VOID);
        if fresh(&mut armed, check) {
            new_hook!(check, SkillProc_SkillBase_CheckTriggerAndActivate);
        }
        let lot = method_addr(base, "LotActivate", 0, VOID);
        if fresh(&mut armed, lot) {
            new_hook!(lot, SkillProc_SkillBase_LotActivate);
        }
    }

    // The method names of the skill objects, read from the game once, since the dump does not carry these classes.
    list_methods("SkillBase", TrainingCuttProbe::class_for_label(umamusume, "Gallop.SkillBase").unwrap_or(std::ptr::null_mut()));
    list_methods("ISkillDetail", TrainingCuttProbe::class_for_label(umamusume, "Gallop.ISkillDetail").unwrap_or(std::ptr::null_mut()));

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceEventPlayer") {
        unsafe { EVENT_PLAYER_CLASS = class };
        let addr = get_method_addr(class, c"TryRequestCutIn", 4);
        if fresh(&mut armed, addr) {
            new_hook!(addr, SkillPopup_RaceEventPlayer_TryRequestCutIn);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceUIMessagePlate") {
        unsafe { MESSAGE_PLATE_CLASS = class };
        // The plate's argument type, read from the game: its fields are not in the dump.
        let desc = crate::il2cpp::symbols::find_nested_class(class, c"PlayDesc").unwrap_or(std::ptr::null_mut());
        list_fields("RaceUIMessagePlate.PlayDesc", desc);
        let addr = get_method_addr(class, c"RequestShowMessagePlate", 1);
        if fresh(&mut armed, addr) {
            new_hook!(addr, SkillPopup_RaceUIMessagePlate_RequestShowMessagePlate);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceUI") {
        unsafe { RACE_UI_CLASS = class };
        let message = get_method_addr(class, c"ShowMessagePlate", 1);
        if fresh(&mut armed, message) {
            new_hook!(message, SkillPopup_RaceUI_ShowMessagePlate);
        }
        let belt = unsafe { AnimationSpeed::resolve_method(class, "SetSteamSkillCutInBeltEnable", BOOL_TWO_CLASS, VOID) };
        if fresh(&mut armed, belt) {
            new_hook!(belt, SkillPopup_RaceUI_SetSteamSkillCutInBeltEnable);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceMainViewController") {
        unsafe { MAIN_VIEW_CLASS = class };
        let message = get_method_addr(class, c"ShowMessagePlate", 1);
        if fresh(&mut armed, message) {
            new_hook!(message, SkillPopup_RaceMainViewController_ShowMessagePlate);
        }
        let belt = unsafe { AnimationSpeed::resolve_method(class, "SetSteamSkillCutInBeltEnable", BOOL_TWO_CLASS, VOID) };
        if fresh(&mut armed, belt) {
            new_hook!(belt, SkillPopup_RaceMainViewController_SetSteamSkillCutInBeltEnable);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceCameraManager") {
        unsafe { CAMERA_MANAGER_CLASS = class };
        let addr = unsafe { AnimationSpeed::resolve_method(class, "StartSteamLandscapeSkillCutIn", ONE_CLASS, VOID) };
        if fresh(&mut armed, addr) {
            new_hook!(addr, SkillPopup_RaceCameraManager_StartSteamLandscapeSkillCutIn);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceSkillCutInReserveCreator") {
        unsafe { CUT_RESERVE_CLASS = class };
        let addr = unsafe { AnimationSpeed::resolve_method(class, "AddCutInInfo", ADD_CUT_PARAMS, VOID) };
        if addr == 0 {
            info!("Skill pop-up: RaceSkillCutInReserveCreator.AddCutInInfo(string) not armed");
        } else if fresh(&mut armed, addr) {
            new_hook!(addr, SkillPopup_RaceSkillCutInReserveCreator_AddCutInInfo);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceSkillCutInHelper") {
        unsafe { CUT_HELPER_CLASS = class };
        let addr = unsafe { AnimationSpeed::resolve_method(class, "OnPlayCutIn", STRING_CLASS, VOID) };
        if fresh(&mut armed, addr) {
            new_hook!(addr, SkillPopup_RaceSkillCutInHelper_OnPlayCutIn);
        }
    }

    if let Some(class) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceEventPlayer") {
        let addr = unsafe { AnimationSpeed::resolve_method(class, "IsPlayableCutIn", PLAYABLE_PARAMS, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN) };
        if addr == 0 {
            info!("Playable cut-in: RaceEventPlayer.IsPlayableCutIn not armed");
        } else if fresh(&mut armed, addr) {
            new_hook!(addr, SkillPopup_RaceEventPlayer_IsPlayableCutIn);
        }
    }
}
