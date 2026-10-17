use std::sync::atomic::{AtomicUsize, Ordering};

use crate::core::Hachimi;
use crate::il2cpp::{ext::Il2CppObjectExt, types::*};

use super::{AnimationSpeed, TrainingCuttProbe};

// The diamond the game floats above your Uma in a race (ledger item 118). It is the client's own race marker.
// `RaceHorseMarkerController` is the thing itself: its `MarkerType` is `Null`, `Player` or `Umaine`, it is initialised
// against a `RaceModelController` and a transform, and the marker objects sit in a `RaceViewBase.MarkerInfo`
// (`markerController`, `markerObject`, `MaterialArray`). The switch over the family is `VisibleMarkers(bool)`: the live
// race view `RaceViewReplay` declares its own body of it, `RaceViewBase` carries the body the views that do not override
// it run (inventory.txt:139434, 139668), `RaceMainViewController.SetVisibleMarkers(bool)` is the copy the race scene
// calls (:133668), and the race HUD mirrors the state with `RaceUI.SetMarkerVisible(bool)` over `_isMarkerVisible` and
// `_changeMarkerVisibleButton` (:137672-137676). The button the player clicks today is
// `RaceUI.OnClickChangeMarkerVisibleButton` -> `ChangeMarkerVisible`.
//
// `hide_race_markers` hands the hide that click hands: every visibility door passes `false` to the game's own method
// while the option is on. Nothing here removes a marker object, refuses a creation, or calls into game code, so no door
// is left holding a table this mod emptied (C64) and no door re-enters a detour of this file. At `false` a visibility door
// hands on the value it was given and an observe door only counts, which is the build the game has always had.
//
// Why only visibility doors, when the markers are also made: `RaceViewBase.InitializeMarker`, its Umaine twin,
// `CreateMarkerControllerTable` and `IsNeedMarker` are abstract declarations on that class (flags 0x05c4: abstract,
// virtual, protected, over an abstract class at 0x00100081) with no body for a trampoline to patch, while
// `RaceViewReplay` and `RaceViewStoryReplay` carry their own bodies of them (`IsNeedMarker` at :139604 and :139738, the
// three make doors at :139615-139617 and :139739-139741). A door armed on a body-less declaration is a hook that never
// fires while its comment says it runs, so the make side is armed observe only here: `IsNeedMarker`, the question a view
// asks itself before it makes them, and `RemoveMarkers`, its own teardown.
// Their counts are what the log offers the next edit. If a race shows a diamond with the option on and a visibility door
// answered an ask, the make side is where the second lever goes, armed on the two classes that have bodies and bound with
// `AnimationSpeed::resolve_generic_ref_method` (C48) rather than by name plus arity.
//
// Every door names the method the client owns, and every line it prints takes the next number of one shared counter, so
// one race's lines read in the order the client used them. Each line also names the class of the object that arrived: a
// door armed on a base body answers for a subclass that never overrides it, and the log states that rather than this file
// assuming it (A4: a door with no line was not the lever). `RaceViewNull` carries both view doors and is left out on
// purpose: it is the null object, whose bodies do nothing.
const MARKER_LOG_LIMIT: usize = 20;
// Past a door's first `MARKER_LOG_LIMIT` calls, one line every this many, so a door the client reaches hundreds of times
// is still visible in the log as a running total rather than gone quiet.
const MARKER_TOTAL_EVERY: usize = 64;

const ONE_FLAG: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN];
const NO_FLAG: &[Il2CppTypeEnum] = &[];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const BOOLEAN: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;

// The order the client used the doors, shared so one race's log is one sequence, plus one count per door. They are process
// wide and never reset: the totals arm above is what keeps a long session readable without a per race boundary.
static DOOR_ORDER: AtomicUsize = AtomicUsize::new(0);
static UI_ASK_SEEN: AtomicUsize = AtomicUsize::new(0);
static STORY_ASK_SEEN: AtomicUsize = AtomicUsize::new(0);
static MAIN_ASK_SEEN: AtomicUsize = AtomicUsize::new(0);
static VIEW_BASE_ASK_SEEN: AtomicUsize = AtomicUsize::new(0);
static VIEW_REPLAY_ASK_SEEN: AtomicUsize = AtomicUsize::new(0);
static BASE_REMOVE_SEEN: AtomicUsize = AtomicUsize::new(0);
static REPLAY_REMOVE_SEEN: AtomicUsize = AtomicUsize::new(0);
static REPLAY_NEED_SEEN: AtomicUsize = AtomicUsize::new(0);
static STORY_NEED_SEEN: AtomicUsize = AtomicUsize::new(0);
static CLICK_SEEN: AtomicUsize = AtomicUsize::new(0);

/// What a visibility door hands to the game: what it was asked for while the option is off, `false` while it is on. A fixed
/// value rather than a change of the current one, so a door asked twice hands the same thing twice (C22).
fn marker_answer(asked: bool, hidden: bool) -> bool {
    asked && !hidden
}

/// Counts one call of one door: the first `MARKER_LOG_LIMIT` of them, then one in `MARKER_TOTAL_EVERY`. A wrapper is a
/// trampoline for one method, so a call reaching it is a call of that door whatever class the object turned out to be, and
/// `class_name` in the line is what says so.
fn first_n(seen: &AtomicUsize) -> Option<usize> {
    let n = seen.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= MARKER_LOG_LIMIT || n % MARKER_TOTAL_EVERY == 0 {
        Some(n)
    } else {
        None
    }
}

/// The number of the line this file prints. It is shared, so one race's lines read in the order they were printed. It is
/// not a count of calls: a call past a door's reporting cap prints no line and takes no number.
fn line_no() -> usize {
    DOOR_ORDER.fetch_add(1, Ordering::Relaxed) + 1
}

/// The option, read once per call of a door that can change what it hands, and once per printed line of a door that
/// cannot. A visibility door is a race's marker switch and `IsNeedMarker` is the question a view asks once before it makes
/// them, so this is off the hot path the way `RaceUI.rs`'s plate hide reads it. Should the census this file prints show a
/// door reached every other frame, that door gets an atomic mirror the way `AnimationSpeed::factor()` does (section 6).
fn hidden_now() -> bool {
    Hachimi::instance().config.load().hide_race_markers
}

/// The name of an object's class, so a door armed on a base body says which view answered.
fn class_name(obj: *mut Il2CppObject) -> String {
    if obj.is_null() {
        return String::from("(null)");
    }

    let klass = unsafe { (*obj).klass() };
    if klass.is_null() {
        return String::from("(no class)");
    }

    unsafe { std::ffi::CStr::from_ptr((*klass).name) }.to_string_lossy().into_owned()
}

/// Whether `addr` is a new address to arm, with the list of armed addresses kept up to date. Two doors sharing one address
/// are armed once, and an unresolved target is never armed (section 2).
fn fresh(armed: &mut Vec<usize>, addr: usize) -> bool {
    if addr == 0 || armed.contains(&addr) {
        return false;
    }

    armed.push(addr);
    true
}

type MarkerSwitchFn = extern "C" fn(this: *mut Il2CppObject, visible: bool);
type MarkerVoidFn = extern "C" fn(this: *mut Il2CppObject);
type MarkerNeedFn = extern "C" fn(this: *mut Il2CppObject) -> bool;

// The five visibility doors. Each writes its line before the call it owes the game, so nothing this file does can sit
// between the game's answer and the game getting it, and a trip on one of those lines is the barrier saying so: the `bail`
// arm hands the game the ask it was given, which is the build the option was not asked for. The line names the value the
// door handed, so a counted call and a call the option changed are the same event in the log.
def_detour! {
    RaceMarker_RaceUI_SetMarkerVisible(this: *mut Il2CppObject, visible: bool) {
        let hidden = hidden_now();
        let handed = marker_answer(visible, hidden);
        if let Some(n) = first_n(&UI_ASK_SEEN) {
            info!("Race marker line {}: #{n} RaceUI.SetMarkerVisible asked {visible}, handed {handed}, option on {hidden}, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceUI_SetMarkerVisible, MarkerSwitchFn)(this, handed);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceUI_SetMarkerVisible, MarkerSwitchFn)(this, visible)
    }
}

def_detour! {
    RaceMarker_StoryRaceUI_SetMarkerVisible(this: *mut Il2CppObject, visible: bool) {
        let hidden = hidden_now();
        let handed = marker_answer(visible, hidden);
        if let Some(n) = first_n(&STORY_ASK_SEEN) {
            info!("Race marker line {}: #{n} StoryRaceUI.SetMarkerVisible asked {visible}, handed {handed}, option on {hidden}, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_StoryRaceUI_SetMarkerVisible, MarkerSwitchFn)(this, handed);
    }
    bail {
        get_orig_fn!(RaceMarker_StoryRaceUI_SetMarkerVisible, MarkerSwitchFn)(this, visible)
    }
}

def_detour! {
    RaceMarker_RaceMainViewController_SetVisibleMarkers(this: *mut Il2CppObject, visible: bool) {
        let hidden = hidden_now();
        let handed = marker_answer(visible, hidden);
        if let Some(n) = first_n(&MAIN_ASK_SEEN) {
            info!("Race marker line {}: #{n} RaceMainViewController.SetVisibleMarkers asked {visible}, handed {handed}, option on {hidden}, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceMainViewController_SetVisibleMarkers, MarkerSwitchFn)(this, handed);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceMainViewController_SetVisibleMarkers, MarkerSwitchFn)(this, visible)
    }
}

def_detour! {
    RaceMarker_RaceViewBase_VisibleMarkers(this: *mut Il2CppObject, visible: bool) {
        let hidden = hidden_now();
        let handed = marker_answer(visible, hidden);
        if let Some(n) = first_n(&VIEW_BASE_ASK_SEEN) {
            info!("Race marker line {}: #{n} RaceViewBase.VisibleMarkers asked {visible}, handed {handed}, option on {hidden}, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceViewBase_VisibleMarkers, MarkerSwitchFn)(this, handed);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceViewBase_VisibleMarkers, MarkerSwitchFn)(this, visible)
    }
}

// The live race view's own body of the same switch. Its address is not the base's: MinHook patches a method body, not a
// vtable slot, so an override this file did not arm would be untouched by the base door.
def_detour! {
    RaceMarker_RaceViewReplay_VisibleMarkers(this: *mut Il2CppObject, visible: bool) {
        let hidden = hidden_now();
        let handed = marker_answer(visible, hidden);
        if let Some(n) = first_n(&VIEW_REPLAY_ASK_SEEN) {
            info!("Race marker line {}: #{n} RaceViewReplay.VisibleMarkers asked {visible}, handed {handed}, option on {hidden}, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceViewReplay_VisibleMarkers, MarkerSwitchFn)(this, handed);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceViewReplay_VisibleMarkers, MarkerSwitchFn)(this, visible)
    }
}

// Observe only: the question a view asks itself before it makes the markers, and its own teardown. Neither is changed, so
// the log carries the make side of the marker family without this file touching it.
//
// The query doors publish the answer they got before they count it, the way `StoryFrameProbe`'s `IsSkipToTextClip` does:
// whether a view makes its markers is the game's decision, and a trip in this file's logging half must not leave the
// wrapper re-asking a method whose body this file has never read, or answering for it. With the answer published, a trip
// is answered from what the game said.
def_detour! {
    RaceMarker_RaceViewReplay_IsNeedMarker(this: *mut Il2CppObject) answer -> bool {
        let value = get_orig_fn!(RaceMarker_RaceViewReplay_IsNeedMarker, MarkerNeedFn)(this);
        answer.publish(value);
        if let Some(n) = first_n(&REPLAY_NEED_SEEN) {
            info!("Race marker line {}: #{n} RaceViewReplay.IsNeedMarker answered {value}, option on {}, on {}", line_no(), hidden_now(), class_name(this));
        }
        value
    }
}

def_detour! {
    RaceMarker_RaceViewStoryReplay_IsNeedMarker(this: *mut Il2CppObject) answer -> bool {
        let value = get_orig_fn!(RaceMarker_RaceViewStoryReplay_IsNeedMarker, MarkerNeedFn)(this);
        answer.publish(value);
        if let Some(n) = first_n(&STORY_NEED_SEEN) {
            info!("Race marker line {}: #{n} RaceViewStoryReplay.IsNeedMarker answered {value}, option on {}, on {}", line_no(), hidden_now(), class_name(this));
        }
        value
    }
}

def_detour! {
    RaceMarker_RaceViewBase_RemoveMarkers(this: *mut Il2CppObject) {
        if let Some(n) = first_n(&BASE_REMOVE_SEEN) {
            info!("Race marker line {}: #{n} RaceViewBase.RemoveMarkers, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceViewBase_RemoveMarkers, MarkerVoidFn)(this);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceViewBase_RemoveMarkers, MarkerVoidFn)(this)
    }
}

def_detour! {
    RaceMarker_RaceViewReplay_RemoveMarkers(this: *mut Il2CppObject) {
        if let Some(n) = first_n(&REPLAY_REMOVE_SEEN) {
            info!("Race marker line {}: #{n} RaceViewReplay.RemoveMarkers, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceViewReplay_RemoveMarkers, MarkerVoidFn)(this);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceViewReplay_RemoveMarkers, MarkerVoidFn)(this)
    }
}

// Observe only: the button the player clicks today. Touched by nothing here, so one race says whether the doors this
// option turns are the doors the manual click already uses.
def_detour! {
    RaceMarker_RaceUI_OnClickChangeMarkerVisibleButton(this: *mut Il2CppObject) {
        if let Some(n) = first_n(&CLICK_SEEN) {
            info!("Race marker line {}: #{n} RaceUI.OnClickChangeMarkerVisibleButton, on {}", line_no(), class_name(this));
        }
        get_orig_fn!(RaceMarker_RaceUI_OnClickChangeMarkerVisibleButton, MarkerVoidFn)(this);
    }
    bail {
        get_orig_fn!(RaceMarker_RaceUI_OnClickChangeMarkerVisibleButton, MarkerVoidFn)(this)
    }
}

/// The three shapes of door this file arms: the signature the client declares for it, and the wrapper written against that
/// signature. A door is never bound by name plus argument count (section 5), so every one of these carries its return
/// type in the request.
#[derive(Clone, Copy)]
enum Shape {
    /// `VisibleMarkers(bool)` / `SetMarkerVisible(bool)` / `SetVisibleMarkers(bool) -> void`: the switch.
    Switch,
    /// A void method that takes nothing: `RemoveMarkers`, `OnClickChangeMarkerVisibleButton`.
    Void,
    /// `IsNeedMarker() -> bool`: the question a view asks itself before it makes the markers.
    Needs,
}

/// Resolves and arms one door of one class, and says out loud which doors this client did not answer. A door sharing an
/// address with one armed above is armed once, so its calls are not counted by two wrappers.
fn arm(class: *mut Il2CppClass, label: &str, name: &str, shape: Shape, armed: &mut Vec<usize>, names: &mut Vec<String>) {
    let (params, ret) = match shape {
        Shape::Switch => (ONE_FLAG, VOID),
        Shape::Void => (NO_FLAG, VOID),
        Shape::Needs => (NO_FLAG, BOOLEAN),
    };

    let addr = unsafe { AnimationSpeed::resolve_method(class, name, params, ret) };
    if addr == 0 {
        info!("Race marker: {label}.{name} not armed");
        return;
    }

    if !fresh(armed, addr) {
        info!("Race marker: {label}.{name} shares its address with a door armed above, armed once");
        return;
    }

    match (label, name) {
        ("Gallop.RaceViewReplay", "VisibleMarkers") => { new_hook!(addr, RaceMarker_RaceViewReplay_VisibleMarkers); }
        ("Gallop.RaceViewReplay", "RemoveMarkers") => { new_hook!(addr, RaceMarker_RaceViewReplay_RemoveMarkers); }
        ("Gallop.RaceViewReplay", "IsNeedMarker") => { new_hook!(addr, RaceMarker_RaceViewReplay_IsNeedMarker); }
        ("Gallop.RaceViewBase", "VisibleMarkers") => { new_hook!(addr, RaceMarker_RaceViewBase_VisibleMarkers); }
        ("Gallop.RaceViewBase", "RemoveMarkers") => { new_hook!(addr, RaceMarker_RaceViewBase_RemoveMarkers); }
        ("Gallop.RaceViewStoryReplay", "IsNeedMarker") => { new_hook!(addr, RaceMarker_RaceViewStoryReplay_IsNeedMarker); }
        ("Gallop.RaceUI", "SetMarkerVisible") => { new_hook!(addr, RaceMarker_RaceUI_SetMarkerVisible); }
        ("Gallop.RaceUI", "OnClickChangeMarkerVisibleButton") => { new_hook!(addr, RaceMarker_RaceUI_OnClickChangeMarkerVisibleButton); }
        ("Gallop.StoryRaceUI", "SetMarkerVisible") => { new_hook!(addr, RaceMarker_StoryRaceUI_SetMarkerVisible); }
        ("Gallop.RaceMainViewController", "SetVisibleMarkers") => { new_hook!(addr, RaceMarker_RaceMainViewController_SetVisibleMarkers); }
        _ => {
            info!("Race marker: {label}.{name} at {addr:#x} has no wrapper written for it, not armed");
            return;
        }
    }

    names.push(format!("{label}.{name} {addr:#x}"));
}

/// Resolves and arms every door of one class. A class this client does not carry arms nothing, and says so.
fn arm_class(umamusume: *const Il2CppImage, label: &str, doors: &[(&str, Shape)], armed: &mut Vec<usize>, names: &mut Vec<String>) {
    let Some(class) = TrainingCuttProbe::class_for_label(umamusume, label) else {
        info!("Race marker: {label} not found, none of its doors are armed");
        return;
    };

    for &(name, shape) in doors {
        arm(class, label, name, shape, armed, names);
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    let mut armed: Vec<usize> = Vec::new();
    let mut names: Vec<String> = Vec::new();

    // The live race view's own bodies first, then the base body the views that do not override it run, then the story
    // replay's own answer to the same question, then the HUD and the scene copy of the switch.
    arm_class(umamusume, "Gallop.RaceViewReplay", &[
        ("VisibleMarkers", Shape::Switch),
        ("RemoveMarkers", Shape::Void),
        ("IsNeedMarker", Shape::Needs),
    ], &mut armed, &mut names);

    arm_class(umamusume, "Gallop.RaceViewBase", &[
        ("VisibleMarkers", Shape::Switch),
        ("RemoveMarkers", Shape::Void),
    ], &mut armed, &mut names);

    arm_class(umamusume, "Gallop.RaceViewStoryReplay", &[("IsNeedMarker", Shape::Needs)], &mut armed, &mut names);

    arm_class(umamusume, "Gallop.RaceUI", &[
        ("SetMarkerVisible", Shape::Switch),
        ("OnClickChangeMarkerVisibleButton", Shape::Void),
    ], &mut armed, &mut names);

    arm_class(umamusume, "Gallop.StoryRaceUI", &[("SetMarkerVisible", Shape::Switch)], &mut armed, &mut names);

    arm_class(umamusume, "Gallop.RaceMainViewController", &[("SetVisibleMarkers", Shape::Switch)], &mut armed, &mut names);

    if names.is_empty() {
        info!("Race marker: no marker door of this client answered, hide_race_markers arms nothing");
    } else {
        info!("Race marker: {} doors armed: {}", names.len(), names.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_visibility_door_hands_back_what_it_was_asked_while_the_option_is_off() {
        assert!(marker_answer(true, false), "the neutral build shows the marker the game asked to show");
        assert!(!marker_answer(false, false), "the neutral build hides the marker the game asked to hide");
    }

    #[test]
    fn a_visibility_door_hides_while_the_option_is_on() {
        assert!(!marker_answer(true, true), "the option turns the game's show ask into the hide the player's own click writes");
        assert!(!marker_answer(false, true), "and a door already asked to hide is not touched");
    }

    #[test]
    fn a_door_arming_is_refused_once_for_an_address_already_armed() {
        let mut armed: Vec<usize> = Vec::new();
        assert!(fresh(&mut armed, 0x1000));
        assert!(!fresh(&mut armed, 0x1000), "an address two doors share is armed once");
        assert!(!fresh(&mut armed, 0), "an unresolved target is never armed (section 2)");
        assert_eq!(armed, vec![0x1000]);
    }

    #[test]
    fn the_first_doors_are_printed_and_the_rest_arrive_as_a_running_total() {
        let seen = AtomicUsize::new(0);
        let mut printed: Vec<usize> = Vec::new();
        for _ in 0..(MARKER_LOG_LIMIT + 3 * MARKER_TOTAL_EVERY) {
            if let Some(n) = first_n(&seen) {
                printed.push(n);
            }
        }

        let named: Vec<usize> = (1..=MARKER_LOG_LIMIT).collect();
        assert_eq!(&printed[..MARKER_LOG_LIMIT], &named[..], "the first calls of a door are each named");
        assert_eq!(&printed[MARKER_LOG_LIMIT..], &[64, 128, 192], "past the limit a door keeps reporting itself as a total rather than going silent");
        assert!(printed.len() < MARKER_LOG_LIMIT + 3 * MARKER_TOTAL_EVERY, "the door's calls outpaced its lines, so the log stays bounded");
    }

    #[test]
    fn a_line_names_no_class_for_an_object_that_has_none() {
        assert_eq!(class_name(std::ptr::null_mut()), "(null)", "a call reaching a door with no object is still counted, and the line says what it could not name");
    }

    #[test]
    fn the_lines_of_two_doors_share_one_order() {
        let first = line_no();
        let second = line_no();
        assert_eq!(second, first + 1, "every line this file prints takes the next number, whatever door it came from");
    }
}
