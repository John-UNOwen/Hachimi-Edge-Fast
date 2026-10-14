use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{
    core::Hachimi,
    il2cpp::{ext::{Il2CppObjectExt, Il2CppStringExt}, symbols::get_field_from_name, types::*},
};

use super::{AnimationSpeed, TrainingCuttProbe};

static mut CLASS: *mut Il2CppClass = 0 as _;
static mut STORY_CLASS: *mut Il2CppClass = 0 as _;
static mut MAIN_CLASS: *mut Il2CppClass = 0 as _;
pub fn class() -> *mut Il2CppClass {
    unsafe { CLASS }
}

def_field_object_accessors!(get get__minimap, MINIMAP_FIELD, Il2CppObject);

const ONE_FLAG: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
// `ShowRaceUIStatusPlate(int, string, MessagePlateType, Action)`. The enum is 4 bytes, so it travels in a general
// register and is read as an i32 (AGENTS section 5, A5).
const SHOW_PLATE_PARAMS: &[Il2CppTypeEnum] = &[
    Il2CppTypeEnum_IL2CPP_TYPE_I4,
    Il2CppTypeEnum_IL2CPP_TYPE_STRING,
    Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE,
    Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
];

// The race's right side status plates, the stack a skill activation is drawn in (ledger item 112). A plate is
// queued through `ShowRaceUIStatusPlate` and drawn by `RaceUIStatusPlateController`, and the group's own visibility
// switch is `SetVisibleRaceUIStatusPlate(bool)` on the same class. With `hide_race_status_plates` on, the switch is
// called with `false` straight after each show. The original show runs first, and its completion callback is handed on
// untouched, so the game's own flow keeps its order and its waits; only the group's visibility is set.
// Story races have their own pair on `StoryRaceUI`. The hook is armed only where the switch is a method of its own:
// where the pair resolves to one address, the switch is the show, and calling it re-enters the detour that called it
// (the stack overflow behind the settings save crash of the run after dcd87c0).
// `RaceMainViewController` is a second race view with its own copy of the pair, and it is where the skill banners
// on the right side come from in the career race: the run after 70803be showed no skill plate through `RaceUI`, and
// the same class carries `SetSteamSkillCutInBeltEnable`, the steam skill cut-in belt.
static mut RACE_SET_VISIBLE_ADDR: usize = 0;
static mut STORY_SET_VISIBLE_ADDR: usize = 0;
static mut MAIN_SET_VISIBLE_ADDR: usize = 0;
impl_addr_wrapper_fn!(race_set_plates_visible, RACE_SET_VISIBLE_ADDR, (), this: *mut Il2CppObject, visible: bool);
impl_addr_wrapper_fn!(story_set_plates_visible, STORY_SET_VISIBLE_ADDR, (), this: *mut Il2CppObject, visible: bool);
impl_addr_wrapper_fn!(main_set_plates_visible, MAIN_SET_VISIBLE_ADDR, (), this: *mut Il2CppObject, visible: bool);

type ShowPlateFn = extern "C" fn(this: *mut Il2CppObject, index: i32, text: *mut Il2CppString, kind: i32, done: *mut Il2CppObject);

// The first plates are named in the log, with the index, the kind the game sent and the text, so the run says which
// `MessagePlateType` value a skill activation carries. A count, not a per plate allocation, is kept for the rest.
const PLATE_LOG_LIMIT: usize = 20;
static PLATES_SHOWN: AtomicUsize = AtomicUsize::new(0);

// Set while a hide is calling the switch. The switch can run game code that shows another plate, and that plate's
// detour must not start a second hide from inside the first; with this set it runs untouched.
static HIDING: AtomicBool = AtomicBool::new(false);

/// Whether the switch is a method of its own. Zero hides nothing, and the show's own address is the show: calling it
/// goes back into the detour that made the call.
fn separate_from_show(show: usize, set_visible: usize) -> bool {
    set_visible != 0 && set_visible != show
}

/// Claims the hide for one switch call. `false` means a hide is already in progress on this call stack.
fn claim_hide(hiding: &AtomicBool) -> bool {
    !hiding.swap(true, Ordering::AcqRel)
}

fn after_show(source: &str, class: *mut Il2CppClass, set_visible: fn(*mut Il2CppObject, bool), this: *mut Il2CppObject, index: i32, text: *mut Il2CppString, kind: i32) {
    let shown = PLATES_SHOWN.fetch_add(1, Ordering::Relaxed) + 1;
    // Read once per plate, not per frame: a race shows a handful of plates. It is not mirrored into an atomic for that reason.
    let hide = Hachimi::instance().config.load().hide_race_status_plates;

    if shown <= PLATE_LOG_LIMIT {
        let text = if text.is_null() { String::from("(null)") } else { unsafe { (*text).as_utf16str() }.to_string() };
        info!("Race status plate {shown} from {source}: index {index}, kind {kind}, text {text:?}, hidden by option: {hide}");
    }

    // Only this hook's own class. A shared function can answer for another class's `this`, and that plate is not this
    // class's group to hide.
    let ours = !this.is_null() && !class.is_null() && unsafe { (*this).klass() } == class;
    if hide && ours && claim_hide(&HIDING) {
        set_visible(this, false);
        HIDING.store(false, Ordering::Release);
    }
}

def_detour! {
    RaceUI_ShowRaceUIStatusPlate(this: *mut Il2CppObject, index: i32, text: *mut Il2CppString, kind: i32, done: *mut Il2CppObject) {
        get_orig_fn!(RaceUI_ShowRaceUIStatusPlate, ShowPlateFn)(this, index, text, kind, done);
        after_show("RaceUI", unsafe { CLASS }, race_set_plates_visible, this, index, text, kind);
    }
    bail {
        get_orig_fn!(RaceUI_ShowRaceUIStatusPlate, ShowPlateFn)(this, index, text, kind, done)
    }
}

def_detour! {
    StoryRaceUI_ShowRaceUIStatusPlate(this: *mut Il2CppObject, index: i32, text: *mut Il2CppString, kind: i32, done: *mut Il2CppObject) {
        get_orig_fn!(StoryRaceUI_ShowRaceUIStatusPlate, ShowPlateFn)(this, index, text, kind, done);
        after_show("StoryRaceUI", unsafe { STORY_CLASS }, story_set_plates_visible, this, index, text, kind);
    }
    bail {
        get_orig_fn!(StoryRaceUI_ShowRaceUIStatusPlate, ShowPlateFn)(this, index, text, kind, done)
    }
}

def_detour! {
    RaceMainViewController_ShowRaceUIStatusPlate(this: *mut Il2CppObject, index: i32, text: *mut Il2CppString, kind: i32, done: *mut Il2CppObject) {
        get_orig_fn!(RaceMainViewController_ShowRaceUIStatusPlate, ShowPlateFn)(this, index, text, kind, done);
        after_show("RaceMainViewController", unsafe { MAIN_CLASS }, main_set_plates_visible, this, index, text, kind);
    }
    bail {
        get_orig_fn!(RaceMainViewController_ShowRaceUIStatusPlate, ShowPlateFn)(this, index, text, kind, done)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, RaceUI);

    unsafe {
        CLASS = RaceUI;
        MINIMAP_FIELD = get_field_from_name(RaceUI, c"_minimap");
        RACE_SET_VISIBLE_ADDR = AnimationSpeed::resolve_method(RaceUI, "SetVisibleRaceUIStatusPlate", ONE_FLAG, VOID);
    }

    let show = unsafe { AnimationSpeed::resolve_method(RaceUI, "ShowRaceUIStatusPlate", SHOW_PLATE_PARAMS, VOID) };
    let set_visible = unsafe { RACE_SET_VISIBLE_ADDR };
    if show != 0 && separate_from_show(show, set_visible) {
        new_hook!(show, RaceUI_ShowRaceUIStatusPlate);
    } else {
        info!("RaceUI: plates not hooked, show {show:#x} and switch {set_visible:#x} are not two methods");
    }

    // Story races draw their plates through their own copy of both methods. A client without the class arms nothing.
    if let Some(story) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.StoryRaceUI") {
        let set_visible = unsafe { AnimationSpeed::resolve_method(story, "SetVisibleRaceUIStatusPlate", ONE_FLAG, VOID) };
        unsafe {
            STORY_CLASS = story;
            STORY_SET_VISIBLE_ADDR = set_visible;
        }

        let show = unsafe { AnimationSpeed::resolve_method(story, "ShowRaceUIStatusPlate", SHOW_PLATE_PARAMS, VOID) };
        if show != 0 && separate_from_show(show, set_visible) {
            new_hook!(show, StoryRaceUI_ShowRaceUIStatusPlate);
        } else {
            info!("StoryRaceUI: plates not hooked, show {show:#x} and switch {set_visible:#x} are not two methods");
        }
    }

    // The race main view, the one the career race draws its skill banners through (see the statics above).
    if let Some(main) = TrainingCuttProbe::class_for_label(umamusume, "Gallop.RaceMainViewController") {
        let set_visible = unsafe { AnimationSpeed::resolve_method(main, "SetVisibleRaceUIStatusPlate", ONE_FLAG, VOID) };
        unsafe {
            MAIN_CLASS = main;
            MAIN_SET_VISIBLE_ADDR = set_visible;
        }

        let show = unsafe { AnimationSpeed::resolve_method(main, "ShowRaceUIStatusPlate", SHOW_PLATE_PARAMS, VOID) };
        if show != 0 && separate_from_show(show, set_visible) {
            new_hook!(show, RaceMainViewController_ShowRaceUIStatusPlate);
        } else {
            info!("RaceMainViewController: plates not hooked, show {show:#x} and switch {set_visible:#x} are not two methods");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plate_hook_is_armed_only_where_the_switch_is_a_method_of_its_own() {
        assert!(separate_from_show(0x1000, 0x2000));
        assert!(!separate_from_show(0x1000, 0x1000), "a switch at the show's own address calls back into the hook it belongs to (ledger item 112)");
        assert!(!separate_from_show(0x1000, 0), "an unresolved switch hides nothing, so there is nothing to arm for it");
    }

    #[test]
    fn a_hide_is_claimed_once_until_it_is_released() {
        let hiding = AtomicBool::new(false);
        assert!(claim_hide(&hiding), "the first plate claims the hide");
        assert!(!claim_hide(&hiding), "a plate shown inside the switch finds the hide claimed and leaves the group alone");
        hiding.store(false, Ordering::Release);
        assert!(claim_hide(&hiding), "the next plate claims it again once the hide is released");
    }
}
