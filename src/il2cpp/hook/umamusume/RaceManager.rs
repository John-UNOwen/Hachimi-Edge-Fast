use std::sync::atomic::{AtomicUsize, Ordering};

use crate::il2cpp::{
    symbols::{get_field_from_name, get_method_addr, SingletonLike},
    types::*
};

use crate::il2cpp::ext::Il2CppObjectExt;

use super::{AnimationSpeed, TrainingCuttProbe};

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

def_field_object_accessors!(get get__horseManager, _HORSEMANAGER_FIELD, Il2CppObject);

def_method_wrapper_fn!(get_RaceInfo, GET_RACE_INFO_ADDR, *mut Il2CppObject,);
def_method_wrapper_fn!(get_RaceSound, GET_RACE_SOUND_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);
def_method_wrapper_fn!(get_RaceView, GET_RACE_VIEW_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);
def_method_wrapper_fn!(get_RaceMainView, GET_RACEMAINVIEW_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);

// The skill cut-in probe (ledger item 112). `PlaySkillCutIn` is the call that starts a skill's banner or cut-in, so the
// first calls are logged with their raw arguments before the original runs: the two skill ints, the flag, the two
// enums as the game sent them, and the horse. Nothing is changed; the original gets the same arguments in the same order.
const SKILL_CUT_LOG_LIMIT: usize = 40;
static SKILL_CUTS_SEEN: AtomicUsize = AtomicUsize::new(0);
// The class each probe is armed on. Its address can be shared with other methods (ledger item 112, the stub at rva
// 0x5088f0 that the story plate functions also resolve to), and those callers reach the detour with another class's
// `this`, so only this class's calls are counted.
static mut SIMULATE_CLASS: *mut Il2CppClass = 0 as _;
static mut REPLAY_CLASS: *mut Il2CppClass = 0 as _;

const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const PLAY_SKILL_CUT_PARAMS: &[Il2CppTypeEnum] = &[
    Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
    Il2CppTypeEnum_IL2CPP_TYPE_I4,
    Il2CppTypeEnum_IL2CPP_TYPE_I4,
    Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
    Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE,
    Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE,
];

type PlaySkillCutFn = extern "C" fn(this: *mut Il2CppObject, horse: *mut Il2CppObject, skill_a: i32, skill_b: i32, flag: bool, category: i32, cut_in: i32);

fn log_skill_cut(source: &str, class: *mut Il2CppClass, this: *mut Il2CppObject, horse: *mut Il2CppObject, skill_a: i32, skill_b: i32, flag: bool, category: i32, cut_in: i32) {
    if this.is_null() || class.is_null() || unsafe { (*this).klass() } != class {
        return;
    }

    let seen = SKILL_CUTS_SEEN.fetch_add(1, Ordering::Relaxed) + 1;
    if seen <= SKILL_CUT_LOG_LIMIT {
        info!("Skill cut-in {seen} from {source}: horse {:#x}, skill args {skill_a} {skill_b}, flag {flag}, category {category}, cut-in {cut_in}", horse as usize);
    }
}

def_detour! {
    RaceManagerSimulate_PlaySkillCutIn(this: *mut Il2CppObject, horse: *mut Il2CppObject, skill_a: i32, skill_b: i32, flag: bool, category: i32, cut_in: i32) {
        log_skill_cut("RaceManagerSimulate", unsafe { SIMULATE_CLASS }, this, horse, skill_a, skill_b, flag, category, cut_in);
        get_orig_fn!(RaceManagerSimulate_PlaySkillCutIn, PlaySkillCutFn)(this, horse, skill_a, skill_b, flag, category, cut_in);
    }
    bail {
        get_orig_fn!(RaceManagerSimulate_PlaySkillCutIn, PlaySkillCutFn)(this, horse, skill_a, skill_b, flag, category, cut_in)
    }
}

def_detour! {
    RaceManagerReplayBase_PlaySkillCutIn(this: *mut Il2CppObject, horse: *mut Il2CppObject, skill_a: i32, skill_b: i32, flag: bool, category: i32, cut_in: i32) {
        log_skill_cut("RaceManagerReplayBase", unsafe { REPLAY_CLASS }, this, horse, skill_a, skill_b, flag, category, cut_in);
        get_orig_fn!(RaceManagerReplayBase_PlaySkillCutIn, PlaySkillCutFn)(this, horse, skill_a, skill_b, flag, category, cut_in);
    }
    bail {
        get_orig_fn!(RaceManagerReplayBase_PlaySkillCutIn, PlaySkillCutFn)(this, horse, skill_a, skill_b, flag, category, cut_in)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, RaceManager);

    unsafe {
        CLASS = RaceManager;
        _HORSEMANAGER_FIELD = get_field_from_name(RaceManager, c"_horseManager");
        GET_RACE_INFO_ADDR = get_method_addr(RaceManager, c"get_RaceInfo", 0);
        GET_RACE_SOUND_ADDR = get_method_addr(RaceManager, c"get_RaceSound", 0);
        GET_RACE_VIEW_ADDR = get_method_addr(RaceManager, c"get_RaceView", 0);
        GET_RACEMAINVIEW_ADDR = get_method_addr(RaceManager, c"get_RaceMainView", 0);
    }

    // The probe is armed on the two classes that override `PlaySkillCutIn`: the live race and the replay. A replay
    // address equal to the live one is not hooked twice.
    let simulate = match TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceManagerSimulate") {
        Some(class) => unsafe {
            SIMULATE_CLASS = class;
            AnimationSpeed::resolve_method(class, "PlaySkillCutIn", PLAY_SKILL_CUT_PARAMS, VOID)
        },
        None => 0,
    };
    if simulate != 0 {
        new_hook!(simulate, RaceManagerSimulate_PlaySkillCutIn);
    }

    let replay = match TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceManagerReplayBase") {
        Some(class) => unsafe {
            REPLAY_CLASS = class;
            AnimationSpeed::resolve_method(class, "PlaySkillCutIn", PLAY_SKILL_CUT_PARAMS, VOID)
        },
        None => 0,
    };
    if replay != 0 && replay != simulate {
        new_hook!(replay, RaceManagerReplayBase_PlaySkillCutIn);
    }
}
