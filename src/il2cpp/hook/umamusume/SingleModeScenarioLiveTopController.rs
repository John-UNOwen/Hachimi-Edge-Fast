// The career scenario live (view 1621 `SingleModeScenarioLiveTop`, with 1620 `SingleModeScenarioLiveSelect` in front
// of it, `SceneDefine.rs:42` and `:43`) plays a cutscene, and the player sees an icon skip button on it. A session on
// the player's other PC stood on that screen eleven times for 468 s, the longest stay 85 s. Run 70 is the first visit
// this fork measured end to end: 12.46 s on view 1621 holding one cut-in of 5,308 ms, and the player's own click landed
// on `SingleModeTrainingCutInHelper::SkipRuntime()` 134 ms before that cut closed (`run log/player-run70.log:2531` to
// `:2533`). That is the door this module presses. The live's own `SingleModeScenarioLiveCutControl::Skip()` was crossed
// zero times in that visit and zero times across the whole 61 min of the other session
// (`run log/player20261030c-otherpc.log`), which is ledger item 125 and the reason the cut press moved off it. With
// `skip_grand_live` on, this module makes that click, through the game's own door, on the object the game's own cut
// control holds.
//
// One door, the game's own, and the window it is pressed inside. `Gallop.SingleModeScenarioLiveCutControl`
// (`run log/introspect-run66.log:23221`) owns the live's cut through
// `field _helper [class<Gallop.SingleModeTrainingCutInHelper>]` (`:23240`), and run 70 measured where that cut actually
// runs: `OnPlayCutIn/0` (`introspect-run69.log:27262`) at 00:42:13.940 and `OnEndCutIn/0` (`:27266`) at 00:42:19.248 on
// view 1621 (`player-run70.log:2414`, `:2532`), while the control's own `Play/0` (`:23229`) crossed 0 times and
// `Stop/0` with `Cleanup/0` crossed 10 (`:2628`). So the window is the helper's own pair, `OnPlayCutIn` opening it and
// `OnEndCutIn` closing it, and the press is `SkipRuntime/0` (`:27255`) on the helper read out of `field _helper` on the
// frame the click lands on. `Play`, `Stop` and `Cleanup` on the control stay armed as counted doors, because a
// run has to tell a cut that ran from one that did not; the control's own `Skip` joins them counted, and since
// the park line it is also the escalation click - see the park paragraph below. A `Stop` or a `Cleanup` closes
// the window as a guard only
// where this module holds the helper's own pair: run 71's live had its cut-in open on screen while `StoryEventProbe`
// held that pair, and the guard closed the gate on all 54,648 frames (`player-run71.log:3542`).
//
//   The door that is counted and never pressed. `Gallop.SingleModeScenarioLiveTopGrandLive3DController` (`:23314`)
//   carries `OnLiveButtonClicked/0 -> void()` (`:23318`), the only click handler on the 3D controller the live's own
//   phase hands out. It crossed 0 times in run 70's visit, and a door no measured run has ever seen the game cross is
//   not a door this fork presses. It stays armed because a player's click on that icon is the only proof of what it is.
//
// What the earlier gates were, and what closed them. The first build gated on the controller's `field _isGrandLiveTop
// [bool]` (`:23308`), and that flag read false on all 3,501 frames of a 31 s visit while the skip button was on screen
// (`player-run67.log:2694` to `:2701`, census `:2740`): it separates the true Grand Live concert from the scenario's
// earlier lives, and the waits are on both sides of it, so it is read for the log only. The rewrite before this one
// gated on `_canSkipLive` (`:23266`) and `_isShowedLiveButton` (`:23264`) read off whatever phase object the screen was
// standing in, and run 70 counted that gate's answer: 1,751 of 1,751 frames of the visit sat on a phase declaring
// neither slot, because `PhaseLiveTop`, `PhaseLiveInWipe`, `PhaseLive` and `PhaseLiveOutWipe` declare no instance bool
// field at all (`player-run70.log:2338`, `:2339`, `:2421`, `:2577`, census `:2628`), and `PhaseLive` offers `Start/0`,
// `Update/1`, `PlayCutAndUIAnimation/0` (`:2421`). Those two slots belong to `PhaseGrandLiveAnimation` (`:23244`), the
// chapter's final concert, which no measured visit has ever stood in. Neither gate is part of the press any more. The
// phase object is still described once per class per visit, because it is the line the next door is chosen from, and the
// two slots are no longer looked up at all: on this client every lookup printed `get_field_from_name: _canSkipLive =
// NULL` as a warning, eight warnings for one visit (`:2336`, `:2408`, `:2418`, `:2574`).
//
// `class_for_label` cannot reach `PhaseGrandLiveAnimation` on this client (`player-run67.log:533`, while the probe
// reached the same class by walking the image at `:972`), which is C59's nested-type trap, so the phase class the
// lever is standing in is taken off the object in `field _currentPhase` (`:23307`) rather than looked up by label. It
// prints that class once per visit, and under `debug_mode` its own bool fields and method names: the line that named
// `PhaseLive` is the line the door pressed now was chosen from.
//
// What a stuck live looks like from this lever, and the one door added to see it. Run 74 lost two of its six visits on
// view 1621: both had the cut skip clicked, both closed their cut-in, neither printed a `PhaseLive` to `PhaseLiveOutWipe`
// step, and both were left by a menu soft reset 5,516 and 5,868 ms after the click (`run log/hachimi.log:14610` to
// `:14618`, `:16573` to `:16576`) with the music and the animation still running. Run 72's stuck visit had this lever
// clicking nothing at all across the whole session (`run log/player20261031-otherpc.log:5764` to `:5788`), so the click
// is not what creates the wait. The lever now keeps a clock on `field _currentPhase`: a new object in that slot is a
// move, the first phase of a visit is the concert and is never judged, and any later phase that stands `PARK_AFTER_MS`
// prints a park line with everything that crossed inside the wait. `SetNextPhase/2` (`introspect-run69.log:23283`) is
// armed as a counted door for one reason: `field _phaseDict` (`:23306`) is the `Dictionary<PhaseType, AbstractPhase>` a
// phase change resolves through, so a live that asked for its next phase and did not move and a live that never asked are
// different failures with different fixes, and only these two readings side by side tell them apart. The park line
// measures; the first line of a parked phase also makes the one click this lever had never made - the cut
// control's own `Skip()`, the game's own live skip button, which no measured run had ever crossed and which is
// what an ending that never arrives would otherwise wait for forever.
//
// Two of this lever's rules are the player's, not this file's. It never writes the game's own saved skip setting: the
// client keeps one (`ApplicationSettingSaveLoader::get_IsEnableScenarioLiveSkipGrandLive`,
// `introspect-run66.log:9248`), it is an obscured 12 byte struct (`:8648`) that A5 says does not travel through a call,
// and the player's ruling is that the fork presses doors rather than editing their save. And the press goes to
// pointers read in the frame they are used in: the controller instance is the one `UpdateView` handed, the phase and the
// cut control are read out of it on that same frame and the helper out of that cut control, and nothing here is called
// through a pointer saved from an earlier one. A class pointer and its field slots are metadata and are kept, which is
// what every other hook file in this fork does.
//
// Four of these doors are doors another module hooks under `debug_mode`: `ScenarioLiveProbe` holds `Skip()`,
// `StoryEventProbe` holds the helper's `OnPlayCutIn` and `OnEndCutIn`, `TrainingCuttProbe` holds the helper's
// `SkipRuntime`, and one address carries one hook. So this module arms its own counting on those three helper doors
// only when the probes are off, and when they are on it reads the window through `StoryEventProbe::open_cut_in_view()`
// and the crossing count through `TrainingCuttProbe::helper_skip_crossings()`. That window is one open run, so a live
// cut-in that opened inside another cut-in already open reads as not open here and the visit presses nothing, counted
// as frames with no cut-in open: a press missed rather than a press nobody asked for, and no measured run has nested
// two cut-ins. Every address this file presses through is resolved in both shapes: a lever that presses needs an
// address in a `debug_mode` build too, and run 69's build left the cut press address 0 in exactly that shape.
//
// The doors are armed in every build, not only under `debug_mode`, because the skip has to land for a player who runs
// with no probes. At `skip_grand_live` false, which is the default, the pump door does two atomic loads and calls the
// game's own `UpdateView` and nothing else (AGENTS section 2, neutral defaults).
use std::ffi::CStr;
use std::fmt::Write as _;
use std::os::raw::c_void;

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};

use crate::{
    core::Hachimi,
    il2cpp::{
        api::{il2cpp_class_get_fields, il2cpp_class_get_methods, il2cpp_field_get_flags, il2cpp_field_get_name, il2cpp_field_get_type, il2cpp_object_get_class},
        hook::umamusume::{
            AnimationSpeed, SceneManager,
            StoryEventProbe::{doors_armed, open_cut_in_view, training_cut_in_cut_doors},
            TrainingCuttProbe::{PROBE_DETAIL_LIMIT, REPORT_INTERVAL_SECS, class_for_label, gate_clock_us, helper_skip_crossings, report_due},
        },
        symbols::{get_field_from_name, get_field_ptr},
        types::*,
    },
};

const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const FIELD_ATTRIBUTE_STATIC: ::std::os::raw::c_int = 0x10;

// The phase door: `SetNextPhase/2 -> void(struct<Gallop.SingleModeScenarioLiveTopController.PhaseType:4B>,
// class<Gallop.SingleModeScenarioLiveTopController.AbstractPhase.Argument>)` (`introspect-run69.log:23283`). The enum is
// four bytes, the shape A5 confirms travels in a general register and that `resolve_method`'s value matcher proves
// before it binds, and the argument is a reference this wrapper holds as an address and hands back untouched. It is
// armed as a counted door: the park line has to be able to say whether the game asked for the next phase at all.
const VALUETYPE: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const PHASE_TYPE_AND_ARGUMENT: &[Il2CppTypeEnum] = &[VALUETYPE, CLASS];

// The two career live screens, so a visit boundary is read the same way `ScenarioLiveProbe` reads it and a lever line
// and a probe line about the same stay can be read against each other.
const VISIT_VIEWS: [i32; 2] = [1620, 1621];
const VIEW_NONE: i32 = i32::MIN;

/// How long the lever waits after a door is open before it clicks. The same settle the Inspiration click uses, for the
/// same reason: 250 ms is the order of the wait this build already keeps before asking a cut-in timeline for its
/// end (C64).
const PRESS_SETTLE_MS: i64 = 250;

/// How many of the live top's own `UpdateView` frames must pass over that window. A wall clock alone cannot tell a
/// frame that ran from a frame the screen never got.
const PRESS_SETTLE_FRAMES: i64 = 3;

/// How long a press that has not ended what it was pressed for is retried for, and how many times at most. A live
/// plays for tens of seconds when it is not skipped, so a re-ask is spaced rather than immediate, and the limit is
/// what keeps a wrong door from being pressed all the way through a live.
const PRESS_REASK_MS: i64 = 2_000;
const PRESS_LIMIT: usize = 3;

/// How long after a door opened a visit with no press is reported. Longer than any settle the screen needs and
/// shorter than the 85 s stay that is the defect, so a run that prints nothing here is a run where the click landed.
const PRESS_MISS_MS: i64 = 8_000;

/// How many distinct live phases one visit may describe. A screen walks a handful of phases, and past this the state
/// lines are the census's job rather than a per frame print (AGENTS section 6).
const PHASE_REPORT_LIMIT: usize = PROBE_DETAIL_LIMIT;

/// How many of a phase class's own bool fields and method names one `debug_mode` line carries. Enough to name the
/// live's slots and the doors near them, short enough that a report line stays readable.
const SURFACE_LIST_LIMIT: usize = 24;

/// How long the live top may stand in one phase, once this visit has already moved to a new phase at least once, before
/// the lever calls that a park. The phase a live plays its concert in is the first phase of a visit and is left alone
/// on purpose: run 74 measured `PhaseLiveTop` standing 7, 7, 21, 31, 49 and 49 s while the live ran
/// (`run log/hachimi.log:13854`, `:14232`, `:14541`, `:14993`, `:16086`, `:16524`), and the phases after it changed every
/// 0.2 to 2.5 s (`:16170` to `:16182`). The two visits that never reached `PhaseLiveOutWipe` stood in `PhaseLive` for
/// 5.5 and 5.9 s after the skip door was crossed (`:14541` to `:14618`, `:16524` to `:16576`), and the park run 72
/// counted with this lever clicking nothing stood 13.8 s (`run log/player20261031-otherpc.log:5764` to `:5788`).
const PARK_AFTER_MS: i64 = 4_000;

/// How often one park repeats its line while the same phase keeps standing, so a run can read whether the helper's
/// timeline door is still crossing through the wait instead of reading one frozen number.
const PARK_REPEAT_MS: i64 = 10_000;

/// How many park lines one session prints: `PROBE_DETAIL_LIMIT` stuck waits, each allowed its first line and two
/// re-reads 10 s apart. Run 74 lost two visits in 41 min (`run log/hachimi.log:14541`, `:16524`), and a cap at the plain
/// detail limit would spend the whole budget on the first one.
const PARK_REPORT_LIMIT: usize = PROBE_DETAIL_LIMIT * 3;

/// How many parked phases one session escalates: each stuck wait is clicked once, on its first park line, and
/// run 74 lost two visits in 41 min (`run log/hachimi.log:14541`, `:16524`) - the detail limit's number of
/// stuck waits is a budget no played session has come close to spending.
const ESCALATE_LIMIT: usize = PROBE_DETAIL_LIMIT;

// The press door: `SkipRuntime/0` on `Gallop.SingleModeTrainingCutInHelper` (`introspect-run69.log:27255`), the helper
// the live's cut control holds in `field _helper` (`introspect-run66.log:23240`) and the door the player's own click on
// the live's skip icon reached in run 70 (`player-run70.log:2531`). `def_method_wrapper_fn!` keeps the press inert when
// the door never resolved, which is the case the arming line reports as a door that did not install (A4). The address is
// resolved whether or not this module arms a hook on it, because pressing is what it is there for.
def_method_wrapper_fn!(press_live_cut_skip, LIVE_CUT_SKIP_ADDR, (), this: *mut Il2CppObject);

// The escalation door: `Skip/0` on the live's own cut control (`introspect-run66.log:23229`, the block naming
// `Play/Stop/Cleanup/Skip`), the live's own skip button, and the door no measured run has ever seen crossed -
// 0 in run 70's visit (`player-run70.log:2628`), 0 across the other PC's 61 min, 0 in run 74's census. A live
// whose cut runtime steps and never terminates waits for an ending nothing clicks, and this is the ending the
// game's own button would click. Inert while the address is 0 (A4), and the arming line says when it did not resolve.
def_method_wrapper_fn!(press_control_skip, LIVE_CONTROL_SKIP_ADDR, (), this: *mut Il2CppObject);

/// Whether the cut door may be clicked this frame. The live's own cut-in has to be open, and the live's cut control has
/// to be holding the helper the press is aimed at. An open window with no helper is a press with nothing to click, and a
/// helper held while no cut is running is a skip nobody asked for.
pub(crate) fn cut_press_allowed(cut_open: bool, helper_held: bool) -> bool {
    cut_open && helper_held
}

// The controller's own slots, resolved at init on the class the dump prints. A field handle resolved on a class is
// that class's own slot, so a gate read is one load per flag (C9: `get_field_ptr` answers null for a handle that
// never resolved, and the readers below treat that as "no answer" rather than reading page 0).
static mut FIELD_IS_GRAND_LIVE_TOP: *mut FieldInfo = std::ptr::null_mut();
static mut FIELD_CURRENT_PHASE: *mut FieldInfo = std::ptr::null_mut();
static mut FIELD_GRAND_LIVE_3D: *mut FieldInfo = std::ptr::null_mut();
static mut FIELD_CUT_CONTROL: *mut FieldInfo = std::ptr::null_mut();

// The phase class is named by the object the game is standing in rather than looked up by label, because C59's nested
// type trap makes `class_for_label` miss it. One entry of cache: the class is compared by pointer and described only
// when the screen moves to another phase, which is a handful of times per visit and not per frame. Its two live slots
// are no longer resolved at all. Run 70 counted 1,751 frames of one visit on a phase declaring neither of them, and
// every lookup of them printed a `get_field_from_name ... = NULL` warning (`player-run70.log:2336`, `:2408`, `:2418`,
// `:2574`).
static PHASE_CLASS_SEEN: AtomicUsize = AtomicUsize::new(0);

// The phase watch the park line reads. `_currentPhase` is polled on every frame the lever stands on a live screen, so a
// new object in that slot is a phase change and the clock since the last one is how long the live top has been standing
// where it is. `SetNextPhase` is armed beside that poll because the two answers are different facts: a screen that never
// asks for the next phase and a screen that asks and does not move need different fixes, and only the pair of them says
// which one a stuck live is. `PHASE_SNAP_*` is what each counted door had read when this phase began, so a park line
// reports what crossed inside the wait instead of a session total a reader has to subtract by hand.
static PHASE_OBJECT_SEEN: AtomicUsize = AtomicUsize::new(0);
static PHASE_CHANGED_MS: AtomicI64 = AtomicI64::new(-1);
static PHASE_CHANGES: AtomicUsize = AtomicUsize::new(0);
// The session total of those moves, which is what the census reports: `PHASE_CHANGES` belongs to one visit.
static PHASE_MOVES: AtomicUsize = AtomicUsize::new(0);
static PHASE_SETS: AtomicUsize = AtomicUsize::new(0);
static PHASE_TYPE_ASKED: AtomicI32 = AtomicI32::new(-1);
static PHASE_SNAP_SETS: AtomicUsize = AtomicUsize::new(0);
static PHASE_SNAP_SKIPS: AtomicUsize = AtomicUsize::new(0);
static PHASE_SNAP_OUR_SKIPS: AtomicUsize = AtomicUsize::new(0);
static PHASE_SNAP_PLAYS: AtomicUsize = AtomicUsize::new(0);
static PHASE_SNAP_STOPS: AtomicUsize = AtomicUsize::new(0);
static PHASE_SNAP_CUT_DOORS: [AtomicUsize; 3] = [AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0)];

// When this visit's live cut-in last closed, and how many park lines this session has already printed. A park that keeps
// going repeats on `PARK_REPEAT_MS` so its numbers can be read as moving or frozen.
static CUT_CLOSED_MS: AtomicI64 = AtomicI64::new(-1);
static PARKS_REPORTED: AtomicUsize = AtomicUsize::new(0);
static PARK_LAST_MS: AtomicI64 = AtomicI64::new(-1);

// The helper the press door belongs to. It is read off the cut control on the frame the click lands on, never kept.
static mut FIELD_CUT_HELPER: *mut FieldInfo = std::ptr::null_mut();

// The window the press lives in: the live's own cut-in, opened by the helper's `OnPlayCutIn()` and closed by its
// `OnEndCutIn()`. Run 70 measured both ends of one on view 1621 (`player-run70.log:2414`, `:2532`) while the control's
// own `Play()` never crossed, so the control's bracket is not the window. Its `Stop()` and `Cleanup()` are kept as a
// guard that closes the window early where this module holds the helper's own pair, and run 71 is why that guard is
// honoured in that shape only: with `StoryEventProbe` holding the pair, 13 stops on the control closed the gate on all
// 54,648 frames of a live that did have its cut-in open on screen (`player-run71.log:3542`, `:3228`, `:3263`). Its
// `Play()` is kept as a counted line. The control's own `field <IsPlayingBGM>k__BackingField [bool]`
// (`introspect-run66.log:23242`) is not part of the gate; it is read only where a line is printed, so a run can tell
// whether a cut the control started was actually playing.
static CUT_PLAYING: AtomicBool = AtomicBool::new(false);
// Which shape is watching the window: this module's own pair when the probes are off, `StoryEventProbe`'s open cut-in
// run when they are on. Resolved once at init, read once a frame.
static CUT_WATCHED_HERE: AtomicBool = AtomicBool::new(false);
static CUT_STOP_GUARD: AtomicBool = AtomicBool::new(false);
// Whether this lever is standing inside an open window right now. Only the edges of this move the counted cut-ins, so
// the census says what the press saw in either build shape rather than what one shape's own doors could see.
static CUT_WINDOW_OPEN: AtomicBool = AtomicBool::new(false);
static CUT_OPEN_MS: AtomicI64 = AtomicI64::new(-1);
static CUT_OPEN_FRAME: AtomicI64 = AtomicI64::new(0);
static mut FIELD_CUT_BGM_PLAYING: *mut FieldInfo = std::ptr::null_mut();
static CUT_PLAYS_BGM_TRUE: AtomicUsize = AtomicUsize::new(0);
static CUT_PLAYS_BGM_FALSE: AtomicUsize = AtomicUsize::new(0);
static CUT_PLAYS_BGM_UNKNOWN: AtomicUsize = AtomicUsize::new(0);

// The visit the lever is pressing on: which live screen the game is standing on, how many of its frames have passed,
// and what each door has already done about it.
static VISIT_VIEW: AtomicI32 = AtomicI32::new(VIEW_NONE);
// The `SceneManager::view_changes()` reading taken when this visit opened. `UpdateView` on the live top is not called
// while the game stands on another screen, so no frame of this lever can watch one live end, and the live that follows
// reads back the view id this visit already holds. That count moving is the live ending (C84).
static VISIT_VIEW_CHANGES: AtomicUsize = AtomicUsize::new(0);
static VISITS_SEEN: AtomicUsize = AtomicUsize::new(0);
static PHASES_SEEN: AtomicUsize = AtomicUsize::new(0);
static VISIT_FRAMES: AtomicI64 = AtomicI64::new(0);
static CUT_PRESSES_MADE: AtomicUsize = AtomicUsize::new(0);
static CUT_LAST_PRESS_MS: AtomicI64 = AtomicI64::new(-1);
static CUT_MISS_REPORTED: AtomicBool = AtomicBool::new(false);
static STOOD_DOWN: AtomicBool = AtomicBool::new(false);

// Held across a press call. Both doors are hooked too, so this is how the module tells its own press from a click the
// player made, without a second copy of either handler.
static PRESS_INSIDE: AtomicBool = AtomicBool::new(false);

static HOST_TICKS: AtomicUsize = AtomicUsize::new(0);
static GAME_CLICKS: AtomicUsize = AtomicUsize::new(0);
static PLAYER_BEAT_US: AtomicUsize = AtomicUsize::new(0);
static PRESS_DROPPED: AtomicUsize = AtomicUsize::new(0);
static CUT_PLAYS: AtomicUsize = AtomicUsize::new(0);
static CUT_STOPS: AtomicUsize = AtomicUsize::new(0);
static CUT_GAME_SKIPS: AtomicUsize = AtomicUsize::new(0);
// The clicks this build made on the cut control's own `Skip()` after a first park line. Kept apart from the
// door's crossings so the census can say how many stuck waits this lever escalated and how many crossings
// the game or a player made on their own.
static CONTROL_ESCALATIONS: AtomicUsize = AtomicUsize::new(0);
static CUT_OUR_SKIPS: AtomicUsize = AtomicUsize::new(0);
static CUT_OUR_SKIP_CROSSINGS: AtomicUsize = AtomicUsize::new(0);
// The helper's own pair, counted apart from the control's `Play()`: run 70 put 0 openings on `Play()` and one on the
// helper, and a census that cannot tell those apart cannot say why a press did or did not happen.
static CUT_INS_OPENED: AtomicUsize = AtomicUsize::new(0);
static CUT_INS_CLOSED: AtomicUsize = AtomicUsize::new(0);
// Crossings of the press door that this module watched rather than made, so a run can tell its own click from the
// game's or the player's. In a `debug_mode` build the same number comes from `TrainingCuttProbe` instead.
static CUT_SKIPS_WATCHED: AtomicUsize = AtomicUsize::new(0);
// The count that door had when this visit opened. `SingleModeTrainingCutInHelper::SkipRuntime()` is the training cut-in's
// skip door too, so a session's crossings are not this live's crossings: a stand-down that read the raw total would
// stand a live visit down the first time any training cut had ever been skipped.
static CUT_SKIP_BASELINE: AtomicUsize = AtomicUsize::new(0);
// How many frames the screen stood as the true Grand Live concert. Counted apart from the gate because run 67 is the
// reason it is no longer part of the gate: the scenario's intermediate lives are the ones the player waits on, and
// they sit on the other side of this flag.
static GRAND_TOP_FRAMES: AtomicUsize = AtomicUsize::new(0);
// Why nothing was pressed, counted apart because they have different fixes: a live cut that is not running where this
// build can see it, a stop on the cut control that closed the window this module was watching, and a live cut control
// holding no helper to click.
static REFUSE_NO_CUT: AtomicUsize = AtomicUsize::new(0);
static REFUSE_CUT_STOPPED: AtomicUsize = AtomicUsize::new(0);
static REFUSE_NO_HELPER: AtomicUsize = AtomicUsize::new(0);

// The census line's own clock, on the probe reports' rule: printed when the totals moved and no more often than
// every `REPORT_INTERVAL_SECS`.
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

/// What the game's own state said about one frame of one live screen. The phase object is reported and not gated on:
/// run 70 counted 1,751 frames of one visit on a phase that declares no live skip slot at all (`player-run70.log:2628`).
#[derive(Clone, Copy, Default)]
struct LiveGate {
    grand_live_top: bool,
    phase_present: bool,
    /// The phase object this frame's `field _currentPhase` held. Read on this frame and used on this frame only, which
    /// is what the park line names.
    phase: *mut Il2CppObject,
}

/// Which half of the press gate held it shut on a frame. Counted apart because the three have different fixes, and a
/// census that names only "no cut-in open" cannot tell a live that never had a cut-in from a live whose cut-in was open
/// on screen and was refused anyway, which is what run 71's 54,648 frames turned out to be (`player-run71.log:3542`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GateHold {
    /// No cut-in of the live's own helper is standing open on this screen.
    NoCutOpen,
    /// The cut control just ended a cut, and where this module holds the helper's own pair that stop closes the window
    /// until the helper opens the next one.
    CutStopped,
    /// The cut control is there and holds no `field _helper` for this build to click.
    NoHelper,
}

/// Why the press gate is closed on this frame, or `None` when it is open. `stopped` is the guard a `Stop()` or a
/// `Cleanup()` on the cut control leaves behind, and it is honoured only where this module holds the helper's own pair.
/// With the probes on, `probe_view` is the view `StoryEventProbe`'s open cut-in run was opened on (`0` when nothing is
/// open), which is the game's own door saying a cut-in is open on this screen right now, and a stop left behind by an
/// earlier cut cannot outvote it: run 71 is exactly that shape, `StoryEventProbe` holding the live's cut-in open on 1621
/// from 01:34:08 and closed there after 6,220 ms (`player-run71.log:3228`, `:3263`) while 13 stops on the control kept
/// the gate closed on every frame of a visit that pressed nothing (`:3542`). `playing` is this module's own reading of
/// the helper's pair and decides the gate in the other shape.
pub(crate) fn press_gate(watched_here: bool, playing: bool, stopped: bool, probe_view: i32, view: i32) -> Option<GateHold> {
    if !watched_here {
        return if probe_view == view { None } else { Some(GateHold::NoCutOpen) };
    }

    if stopped {
        return Some(GateHold::CutStopped);
    }

    if playing {
        return None;
    }

    Some(GateHold::NoCutOpen)
}

/// What one frame of the window does to the counted cut-ins. A count moves on the edge only, so a window that stayed as
/// it was moves nothing. These two counts are the window as this lever saw it, which is a fact both build shapes can
/// report: run 71's census said 0 cut-ins opened on a screen where `StoryEventProbe` had one open for 6,220 ms
/// (`player-run71.log:3542`, `:3263`) because only the doors this module arms itself could move them.
pub(crate) fn cut_window_edges(was_open: bool, is_open: bool) -> (bool, bool) {
    (is_open && !was_open, !is_open && was_open)
}

/// Whether one door is due its click. `armed_ms` is when that door opened for this visit, `-1` when it never has,
/// `frames` is how many of the screen's frames have passed since it opened, `presses` what this build has already
/// clicked on that door, and `last_press_ms` when the newest of those landed, `-1` before the first. The switch itself
/// is the caller's fast exit, and a door that is not open is never due a click.
pub(crate) fn live_press_due(armed_ms: i64, frames: i64, now_ms: i64, presses: usize, last_press_ms: i64, door_open: bool) -> bool {
    if !door_open || armed_ms < 0 || presses >= PRESS_LIMIT {
        return false;
    }

    if presses == 0 {
        return now_ms - armed_ms >= PRESS_SETTLE_MS && frames >= PRESS_SETTLE_FRAMES;
    }

    now_ms - last_press_ms >= PRESS_REASK_MS
}

/// Whether a live top standing in one phase is a park. `phase_changes` is how many times this visit has already moved to
/// a new phase object, `phase_stood_ms` how long the phase it is standing in has been that same object, and
/// `last_park_ms` when this phase's last park line printed, `-1` before it has printed one. The first phase of a visit is
/// not judged: that is the concert, and run 74 measured it standing 7 to 49 s (`run log/hachimi.log:13854` to `:16524`).
/// Every phase after the first changed every 0.2 to 2.5 s in the visits that finished (`:16170` to `:16182`), so a phase
/// that stands past `PARK_AFTER_MS` is the live not moving rather than a live still playing.
pub(crate) fn park_due(phase_changes: usize, phase_stood_ms: i64, now_ms: i64, last_park_ms: i64) -> bool {
    phase_changes > 0 && phase_stood_ms >= PARK_AFTER_MS && (last_park_ms < 0 || now_ms - last_park_ms >= PARK_REPEAT_MS)
}

/// Whether a park line is followed by this build's one click on the cut control's own `Skip()`: the click rides
/// the phase's first park line (`first_line`, before any repeat has printed), needs the control this frame's
/// controller handed, and stays inside the session's escalation budget. The repeat lines then read whether the
/// wait moved after the click or stood through it.
pub(crate) fn escalate_due(first_line: bool, control_held: bool, escalations: usize, limit: usize) -> bool {
    first_line && control_held && escalations < limit
}

/// Whether a visit whose door opened has gone too long without a click to leave unreported.
pub(crate) fn live_wait_reported(armed_ms: i64, now_ms: i64, presses: usize, already_reported: bool) -> bool {
    !already_reported && armed_ms >= 0 && presses == 0 && now_ms - armed_ms >= PRESS_MISS_MS
}

fn flag_text(answer: Option<bool>) -> &'static str {
    match answer {
        Some(true) => "true",
        Some(false) => "false",
        None => "no such slot",
    }
}

fn now_ms() -> i64 {
    gate_clock_us() / 1000
}

fn live_view(view: i32) -> bool {
    VISIT_VIEWS.contains(&view)
}

// A bool slot read as a byte, so a value the client wrote that is not a valid `bool` cannot turn the read into a
// panic, and a null handle or a null object answered as "no answer" rather than as a fault (C9, and the same shape
// `CutStateProbe::read_flag_value` reads a bool in).
unsafe fn read_flag(obj: *mut Il2CppObject, field: *mut FieldInfo) -> Option<bool> {
    if obj.is_null() || field.is_null() {
        return None;
    }

    let slot = get_field_ptr::<u8>(obj, field);

    if slot.is_null() {
        return None;
    }

    Some(*slot != 0)
}

unsafe fn read_object(obj: *mut Il2CppObject, field: *mut FieldInfo) -> *mut Il2CppObject {
    if obj.is_null() || field.is_null() {
        return std::ptr::null_mut();
    }

    unsafe { *get_field_ptr::<*mut Il2CppObject>(obj, field) }
}

/// A class spelled the way a report line has to spell it: a class with an empty namespace prints with no owner, the
/// same way `introspect.log` prints it (A27).
unsafe fn class_label(class: *mut Il2CppClass) -> String {
    let name = unsafe { CStr::from_ptr((*class).name) }.to_string_lossy().into_owned();
    let namespace = unsafe { CStr::from_ptr((*class).namespaze) }.to_string_lossy().into_owned();

    if namespace.is_empty() {
        name
    }
    else {
        format!("{namespace}.{name}")
    }
}

// What one phase class is on its own: its name and, under `debug_mode`, the bool fields and method names it declares.
// Once per phase class per visit, never per frame. This is the line that named `PhaseLive` with its
// `PlayCutAndUIAnimation()` (`run log/player-run68.log:2809`), and it is the line the press door was chosen from. The
// two live slots are no longer looked up here: on this client every lookup answered null and printed its own warning
// (`player-run70.log:2336`, `:2408`, `:2418`, `:2574`), and run 70 counted the answer they would have given.
unsafe fn describe_phase(phase: *mut Il2CppObject, class: *mut Il2CppClass) {
    let label = class_label(class);

    PHASE_CLASS_SEEN.store(class as usize, Ordering::Release);

    let seen = PHASES_SEEN.fetch_add(1, Ordering::Relaxed) + 1;

    if seen > PHASE_REPORT_LIMIT {
        return;
    }

    info!("Grand Live skip phase {seen} of this visit: {label} on {phase:p} is the phase the live top is standing in");

    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    info!("Grand Live skip phase {seen} surfaces: {}", surfaces_of(class));
}

// A class's own instance bool fields and its method names, each list capped. `debug_mode` only, and walked through
// the same iterators `introspect.log` writes the class blocks with.
unsafe fn surfaces_of(class: *mut Il2CppClass) -> String {
    let mut flags: Vec<String> = Vec::new();
    let mut methods: Vec<String> = Vec::new();
    let mut iter: *mut c_void = std::ptr::null_mut();

    while flags.len() < SURFACE_LIST_LIMIT {
        let field = il2cpp_class_get_fields(class, &mut iter);

        if field.is_null() {
            break;
        }

        if il2cpp_field_get_flags(field) & FIELD_ATTRIBUTE_STATIC != 0 {
            continue;
        }

        let field_type = il2cpp_field_get_type(field);

        if field_type.is_null() || (*field_type).type_() != Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN {
            continue;
        }

        flags.push(CStr::from_ptr(il2cpp_field_get_name(field)).to_string_lossy().into_owned());
    }

    let mut iter: *mut c_void = std::ptr::null_mut();

    while methods.len() < SURFACE_LIST_LIMIT {
        let method = il2cpp_class_get_methods(class, &mut iter);

        if method.is_null() {
            break;
        }

        let name = CStr::from_ptr((*method).name).to_string_lossy().into_owned();

        // A compiler generated helper is not a door a lever can aim at, and it crowds the list out of the names a
        // reader needs.
        if name.starts_with('<') {
            continue;
        }

        let arity = unsafe { (*method).parameters_count };
        methods.push(format!("{name}/{arity}"));
    }

    let mut line = String::new();
    let label = class_label(class);
    let _ = write!(line, "{label} bool fields [{}], methods [{}]", flags.join(", "), methods.join(", "));
    line
}

/// The button gate as the game left it on this frame: the controller's own phase object, read through the class that
/// object itself carries, and the two live slots of that class. Nothing here is looked up by label at a frame
/// boundary, and no pointer read here is kept for a later frame.
unsafe fn gate_of(this: *mut Il2CppObject) -> LiveGate {
    let mut gate = LiveGate {
        grand_live_top: read_flag(this, FIELD_IS_GRAND_LIVE_TOP).unwrap_or(false),
        ..LiveGate::default()
    };

    if gate.grand_live_top {
        GRAND_TOP_FRAMES.fetch_add(1, Ordering::Relaxed);
    }

    let phase = read_object(this, FIELD_CURRENT_PHASE);
    gate.phase = phase;

    if phase.is_null() {
        return gate;
    }

    let class = il2cpp_object_get_class(phase);

    if class.is_null() {
        return gate;
    }

    gate.phase_present = true;

    if PHASE_CLASS_SEEN.load(Ordering::Acquire) != class as usize {
        describe_phase(phase, class);
    }

    // A new object in the slot is a move. The class alone cannot say that: a client that hands back the same phase
    // instance for a second pass through the same phase would not re-describe it, and the park clock has to restart.
    if PHASE_OBJECT_SEEN.load(Ordering::Acquire) != phase as usize {
        note_phase_change(phase);
    }

    gate
}

/// The live top moved to another phase object. Restart the clock the park line measures, take the door snapshots it
/// reports against, and count the move. The first phase of a visit is not a move: it is the concert, and run 74's six
/// visits opened in `PhaseLiveTop` and stood in it 7, 7, 21, 31, 49 and 49 s (`run log/hachimi.log:13854`, `:14232`,
/// `:14541`, `:14993`, `:16086`, `:16524`), which is a wait and not a park. A new phase also gets its own park report.
fn note_phase_change(phase: *mut Il2CppObject) {
    let first = PHASE_OBJECT_SEEN.swap(phase as usize, Ordering::AcqRel) == 0;

    if !first {
        PHASE_CHANGES.fetch_add(1, Ordering::Relaxed);
        PHASE_MOVES.fetch_add(1, Ordering::Relaxed);
    }

    PHASE_CHANGED_MS.store(now_ms(), Ordering::Relaxed);
    PHASE_SNAP_SETS.store(PHASE_SETS.load(Ordering::Relaxed), Ordering::Relaxed);
    PHASE_SNAP_SKIPS.store(skip_door_crossings(), Ordering::Relaxed);
    PHASE_SNAP_OUR_SKIPS.store(CUT_OUR_SKIPS.load(Ordering::Relaxed), Ordering::Relaxed);
    PHASE_SNAP_PLAYS.store(CUT_PLAYS.load(Ordering::Relaxed), Ordering::Relaxed);
    PHASE_SNAP_STOPS.store(CUT_STOPS.load(Ordering::Relaxed), Ordering::Relaxed);
    let (steps, cleanup, terminate) = training_cut_in_cut_doors();
    PHASE_SNAP_CUT_DOORS[0].store(steps, Ordering::Relaxed);
    PHASE_SNAP_CUT_DOORS[1].store(cleanup, Ordering::Relaxed);
    PHASE_SNAP_CUT_DOORS[2].store(terminate, Ordering::Relaxed);
    PARK_LAST_MS.store(-1, Ordering::Relaxed);
}

/// The press door's session total, from whichever module is holding it: this module's own watch when the probes are off,
/// `TrainingCuttProbe`'s count when they are on (one address, one hook).
fn skip_door_crossings() -> usize {
    if CUT_WATCHED_HERE.load(Ordering::Relaxed) { CUT_SKIPS_WATCHED.load(Ordering::Relaxed) } else { helper_skip_crossings() }
}

/// The first `PROBE_DETAIL_LIMIT` refusals of each kind per visit, so a stay that pressed nothing says which half of
/// the gate closed it. A per frame door does not get to format a sentence every frame, so this is counted first and
/// printed only while the count is inside the limit, and how many frames each reason held the screen for is the census
/// line's job (AGENTS section 6).
fn report_refusal(hold: GateHold) {
    let (counter, name) = match hold {
        GateHold::NoCutOpen => (
            &REFUSE_NO_CUT,
            "the live's own cut-in is not open: no `OnPlayCutIn()` on the helper the live's cut control holds is standing open on this screen right now",
        ),
        GateHold::CutStopped => (
            &REFUSE_CUT_STOPPED,
            "the live's cut control just crossed `Stop()` or `Cleanup()`: this build waits for the helper to open the next cut-in",
        ),
        GateHold::NoHelper => (&REFUSE_NO_HELPER, "the live's cut control holds no `field _helper` for this build to click"),
    };

    let seen = counter.fetch_add(1, Ordering::Relaxed) + 1;

    if seen <= PROBE_DETAIL_LIMIT {
        info!("Grand Live skip gate held {seen}: {name}");
    }
}

/// The park line: what the live top is standing in, how long it has stood there, and what crossed while it stood. Every
/// number is a delta since this phase began, so the line says whether the game is still working at the cut or has stopped
/// asking for the next phase. It prints when the wait passes `PARK_AFTER_MS` and again on `PARK_REPEAT_MS` while the same
/// phase keeps standing, capped at `PARK_REPORT_LIMIT` for the session, and it prints in a build with no probes.
unsafe fn report_park_if_due(this: *mut Il2CppObject, phase: *mut Il2CppObject, frames: i64, now: i64) {
    let stood = now - PHASE_CHANGED_MS.load(Ordering::Relaxed);

    if !park_due(PHASE_CHANGES.load(Ordering::Relaxed), stood, now, PARK_LAST_MS.load(Ordering::Relaxed)) {
        return;
    }

    let reported = PARKS_REPORTED.fetch_add(1, Ordering::Relaxed) + 1;

    if reported > PARK_REPORT_LIMIT {
        return;
    }

    let first_line = PARK_LAST_MS.load(Ordering::Relaxed) < 0;

    PARK_LAST_MS.store(now, Ordering::Relaxed);

    let class = il2cpp_object_get_class(phase);
    let phase_label = if class.is_null() { "a phase this build cannot name".to_string() } else { class_label(class) };
    let closed_ms = CUT_CLOSED_MS.load(Ordering::Relaxed);
    let closed_note = if closed_ms >= 0 {
        format!("{} ms after its live cut-in closed", now - closed_ms)
    }
    else {
        "with no live cut-in closed in this visit".to_string()
    };
    let our_skips = CUT_OUR_SKIPS.load(Ordering::Relaxed).saturating_sub(PHASE_SNAP_OUR_SKIPS.load(Ordering::Relaxed));
    let other_skips = skip_door_crossings()
        .saturating_sub(PHASE_SNAP_SKIPS.load(Ordering::Relaxed))
        .saturating_sub(our_skips);
    let sets = PHASE_SETS.load(Ordering::Relaxed).saturating_sub(PHASE_SNAP_SETS.load(Ordering::Relaxed));
    let asked = PHASE_TYPE_ASKED.load(Ordering::Relaxed);
    let asked_note = if asked >= 0 { format!("the last phase it asked for numbered {asked}") } else { "and no phase has been asked for this session".to_string() };
    let plays = CUT_PLAYS.load(Ordering::Relaxed).saturating_sub(PHASE_SNAP_PLAYS.load(Ordering::Relaxed));
    let stops = CUT_STOPS.load(Ordering::Relaxed).saturating_sub(PHASE_SNAP_STOPS.load(Ordering::Relaxed));
    let (steps, cleanup, terminate) = training_cut_in_cut_doors();
    let cut_doors = if doors_armed() {
        format!(
            "the helper crossed `OnTimelineUpdatePost` {} more times, `CleanupPlaying` {} and `OnTerminateRuntime` {}",
            steps.saturating_sub(PHASE_SNAP_CUT_DOORS[0].load(Ordering::Relaxed)),
            cleanup.saturating_sub(PHASE_SNAP_CUT_DOORS[1].load(Ordering::Relaxed)),
            terminate.saturating_sub(PHASE_SNAP_CUT_DOORS[2].load(Ordering::Relaxed)),
        )
    }
    else {
        "the helper's cut doors are not counted in a build without `debug_mode`".to_string()
    };
    let top_note = flag_text(read_flag(this, FIELD_IS_GRAND_LIVE_TOP));
    let control = read_object(this, FIELD_CUT_CONTROL);
    let bgm_note = flag_text(read_flag(control, FIELD_CUT_BGM_PLAYING));

    warn!(
        "Grand Live skip park {reported}: the live top has stood in {phase_label} on {phase:p} for {stood} ms on frame {frames} of its `UpdateView`, {closed_note}; inside this phase `SetNextPhase` crossed {sets} times, {asked_note}; the skip door crossed {other_skips} times by the game or the player and {our_skips} by this build; the cut control crossed `Play()` {plays} and `Stop()` or `Cleanup()` {stops}; {cut_doors} since this phase began; `_isGrandLiveTop` reads {top_note} and the cut control's `_isPlayingBGM` reads {bgm_note}",
    );

    // The escalation the park line was built to make possible: a live whose cut runtime steps and never
    // terminates waits for an ending nothing clicks, so this build clicks the ending the game's own button
    // would - the cut control's `Skip()`, the door no measured run has ever seen crossed. Once per parked
    // phase, on its first park line, through the game's own door on the object this frame's controller
    // handed; the repeat park lines then say whether `SetNextPhase` crossed after the click or not.
    if escalate_due(first_line, !control.is_null(), CONTROL_ESCALATIONS.load(Ordering::Relaxed), ESCALATE_LIMIT) {
        if PRESS_INSIDE.swap(true, Ordering::AcqRel) {
            let dropped = PRESS_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;

            if dropped <= PROBE_DETAIL_LIMIT {
                warn!("Grand Live skip escalation {reported} dropped: a press is already inside a live skip door for {control:p}");
            }
        }
        else {
            press_control_skip(control);

            PRESS_INSIDE.store(false, Ordering::Release);

            let escalated = CONTROL_ESCALATIONS.fetch_add(1, Ordering::Relaxed) + 1;

            warn!(
                "Grand Live skip escalate {escalated}: clicked the live cut control's own `Skip()` on {control:p} {stood} ms into the parked {phase_label} on {phase:p}, the door no measured run had ever seen crossed; the repeat park lines of this phase will say whether `SetNextPhase` crossed after the click",
            );
        }
    }
    else if first_line && control.is_null() {
        warn!("Grand Live skip escalate {reported} not made: this frame's live top handed no `field _cutControl` to click");
    }
}

fn open_visit(view: i32) {
    VISITS_SEEN.fetch_add(1, Ordering::Relaxed);
    VISIT_FRAMES.store(0, Ordering::Relaxed);
    CUT_OPEN_MS.store(-1, Ordering::Relaxed);
    CUT_OPEN_FRAME.store(0, Ordering::Relaxed);
    CUT_PRESSES_MADE.store(0, Ordering::Relaxed);
    CUT_LAST_PRESS_MS.store(-1, Ordering::Relaxed);
    CUT_MISS_REPORTED.store(false, Ordering::Relaxed);
    CUT_PLAYING.store(false, Ordering::Relaxed);
    CUT_STOP_GUARD.store(false, Ordering::Relaxed);
    CUT_WINDOW_OPEN.store(false, Ordering::Relaxed);
    STOOD_DOWN.store(false, Ordering::Relaxed);
    PHASES_SEEN.store(0, Ordering::Relaxed);
    PHASE_CLASS_SEEN.store(0, Ordering::Relaxed);
    PHASE_OBJECT_SEEN.store(0, Ordering::Relaxed);
    PHASE_CHANGED_MS.store(-1, Ordering::Relaxed);
    PHASE_CHANGES.store(0, Ordering::Relaxed);
    CUT_CLOSED_MS.store(-1, Ordering::Relaxed);
    PARK_LAST_MS.store(-1, Ordering::Relaxed);
    CUT_SKIP_BASELINE.store(
        if CUT_WATCHED_HERE.load(Ordering::Relaxed) { CUT_SKIPS_WATCHED.load(Ordering::Relaxed) } else { helper_skip_crossings() },
        Ordering::Relaxed,
    );
    VISIT_VIEW_CHANGES.store(SceneManager::view_changes(), Ordering::Relaxed);

    for counter in [&REFUSE_NO_CUT, &REFUSE_CUT_STOPPED, &REFUSE_NO_HELPER] {
        counter.store(0, Ordering::Relaxed);
    }

    info!(
        "Grand Live skip visit {}: the game is standing on view {}, and this build presses `SingleModeTrainingCutInHelper::SkipRuntime()` on the helper its own cut control holds in `field _helper` while that cut-in is open on this screen, {PRESS_SETTLE_MS} ms and {PRESS_SETTLE_FRAMES} frames after it opens and up to {PRESS_LIMIT} clicks {PRESS_REASK_MS} ms apart",
        VISITS_SEEN.load(Ordering::Relaxed),
        view,
    );
}

// The press itself, on a helper read in this frame and a door resolved at init. `PRESS_INSIDE` is what keeps the
// crossing this build's own press leaves on that door from being counted as a click the game or the player made.
unsafe fn press(helper: *mut Il2CppObject, armed_ms: i64, frames: i64) {
    if PRESS_INSIDE.swap(true, Ordering::AcqRel) {
        let dropped = PRESS_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;

        if dropped <= PROBE_DETAIL_LIMIT {
            warn!("Grand Live skip press {dropped} dropped: a press is already inside the live cut skip door for {helper:p}");
        }

        return;
    }

    press_live_cut_skip(helper);

    PRESS_INSIDE.store(false, Ordering::Release);

    // The visit's own budget and the run's census, both counted here, so `PRESS_LIMIT` is a bound the press loop
    // actually sees rather than a number a log line quotes.
    CUT_PRESSES_MADE.fetch_add(1, Ordering::Relaxed);
    let count = CUT_OUR_SKIPS.fetch_add(1, Ordering::Relaxed) + 1;

    if count <= PROBE_DETAIL_LIMIT {
        info!(
            "Grand Live skip press {count} on the live cut door: clicked `SingleModeTrainingCutInHelper::SkipRuntime()` on {helper:p} {} ms after the live's cut-in opened and on frame {frames} of the live top's UpdateView",
            now_ms() - armed_ms,
        );
    }
}

/// The window this lever is standing in, read once a frame. Only an edge moves anything: an opening counts a cut-in and
/// arms the press on its own clock, a closing counts the end and takes the arm away, and a window that stayed as it was
/// writes nothing (AGENTS section 6). Both build shapes report these two counts, which is what run 71's census could
/// not do: it said 0 cut-ins opened on a live whose cut-in `StoryEventProbe` held open on that screen for 6,220 ms
/// (`player-run71.log:3542`, `:3263`) because only the helper doors this module arms itself could move them, and in a
/// `debug_mode` build those doors are armed on `StoryEventProbe`. The pointer is only named in the line.
fn note_window_edge(this: *mut Il2CppObject, cut_open: bool) {
    let (opened, closed) = cut_window_edges(CUT_WINDOW_OPEN.load(Ordering::Relaxed), cut_open);

    if !opened && !closed {
        return;
    }

    CUT_WINDOW_OPEN.store(cut_open, Ordering::Relaxed);
    CUT_OPEN_MS.store(-1, Ordering::Relaxed);
    CUT_OPEN_FRAME.store(0, Ordering::Relaxed);

    if closed {
        CUT_INS_CLOSED.fetch_add(1, Ordering::Relaxed);
        CUT_CLOSED_MS.store(now_ms(), Ordering::Relaxed);

        return;
    }

    let opens = CUT_INS_OPENED.fetch_add(1, Ordering::Relaxed) + 1;

    if opens <= PROBE_DETAIL_LIMIT {
        info!(
            "Grand Live skip cut opened {opens} on {this:p}: the live's cut-in is open on the helper its own cut control holds in `field _helper`, {PRESS_SETTLE_MS} ms and {PRESS_SETTLE_FRAMES} frames before this build clicks its `SkipRuntime()`",
        );
    }
}

/// Whether the live the pump is standing on needs a visit of its own. The view id alone is not an answer: the live
/// top's door is not called while the game is off the live screen, so `VISIT_VIEW` still holds 1621 when the next live
/// starts and the same number reads back as the same visit. Run 72 counted 18 lives against 1 visit that way, one
/// stand-down ruling all of them (`run log/player20261031-otherpc.log:15867`). A view change behind the reading is the
/// live ending, and the live after it gets its own visit, its own skip baseline and its own stand-down (C84).
pub(crate) fn new_live_visit(visit_view: i32, view: i32, changes_at_open: usize, changes_now: usize) -> bool {
    visit_view != view || changes_now != changes_at_open
}

/// Reached from the live top's own per frame door, so `this` is a controller instance the game is holding right now.
unsafe fn pump_press(this: *mut Il2CppObject) {
    let view = SceneManager::requested_view_id();

    if !live_view(view) {
        // Off a live screen there is nothing to read and nothing to press. One store when the visit was open, not one
        // per frame (AGENTS section 6).
        if VISIT_VIEW.load(Ordering::Relaxed) != VIEW_NONE {
            VISIT_VIEW.store(VIEW_NONE, Ordering::Relaxed);
            CUT_OPEN_MS.store(-1, Ordering::Relaxed);
            CUT_PLAYING.store(false, Ordering::Relaxed);
            CUT_STOP_GUARD.store(false, Ordering::Relaxed);
            CUT_WINDOW_OPEN.store(false, Ordering::Relaxed);
            PHASE_CLASS_SEEN.store(0, Ordering::Relaxed);
        }

        return;
    }

    if new_live_visit(
        VISIT_VIEW.load(Ordering::Relaxed),
        view,
        VISIT_VIEW_CHANGES.load(Ordering::Relaxed),
        SceneManager::view_changes(),
    ) {
        VISIT_VIEW.store(view, Ordering::Relaxed);
        open_visit(view);

        return;
    }

    HOST_TICKS.fetch_add(1, Ordering::Relaxed);
    let frames = VISIT_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    let now = now_ms();

    // Read every frame the switch is on and the screen is a live screen, for the census, the phase lines and the park
    // line only: what the live top's own state says no longer decides whether anything is pressed.
    let gate = gate_of(this);

    // A screen that has stopped moving is the thing a played build has to be able to describe, so this is read before
    // the press gate and before a stand-down, and it costs a few atomic loads on a frame that is not parked.
    if !gate.phase.is_null() {
        report_park_if_due(this, gate.phase, frames, now);
    }

    // A visit the player or the game already skipped is done: read nothing more of the game.
    if STOOD_DOWN.load(Ordering::Relaxed) {
        return;
    }

    let hold = press_gate(
        CUT_WATCHED_HERE.load(Ordering::Relaxed),
        CUT_PLAYING.load(Ordering::Relaxed),
        CUT_STOP_GUARD.load(Ordering::Relaxed),
        open_cut_in_view(),
        view,
    );
    let cut_open = hold.is_none();

    note_window_edge(this, cut_open);

    if let Some(hold) = hold {
        report_refusal(hold);

        return;
    }

    // The helper is read here and used here: the cut control out of the controller this frame handed, and the helper
    // out of that control. Nothing is pressed through a pointer kept from an earlier frame.
    let control = read_object(this, FIELD_CUT_CONTROL);
    let helper = read_object(control, FIELD_CUT_HELPER);

    if !cut_press_allowed(cut_open, !helper.is_null()) {
        report_refusal(GateHold::NoHelper);

        return;
    }

    if cut_skip_already_made() {
        stand_down(this);

        return;
    }

    let armed_ms = CUT_OPEN_MS.load(Ordering::Relaxed);
    let presses_made = CUT_PRESSES_MADE.load(Ordering::Relaxed);
    let last_press_ms = CUT_LAST_PRESS_MS.load(Ordering::Relaxed);

    if armed_ms < 0 {
        CUT_OPEN_MS.store(now, Ordering::Relaxed);
        CUT_OPEN_FRAME.store(frames, Ordering::Relaxed);

        info!(
            "Grand Live skip armed: the live's own cut-in is open on {this:p}, waiting {PRESS_SETTLE_MS} ms and {PRESS_SETTLE_FRAMES} frames of this screen before clicking `SingleModeTrainingCutInHelper::SkipRuntime()` on the helper its cut control holds"
        );

        return;
    }

    let elapsed = frames - CUT_OPEN_FRAME.load(Ordering::Relaxed);

    if !live_press_due(armed_ms, elapsed, now, presses_made, last_press_ms, true) {
        if live_wait_reported(armed_ms, now, presses_made, CUT_MISS_REPORTED.load(Ordering::Relaxed)) {
            CUT_MISS_REPORTED.store(true, Ordering::Relaxed);

            warn!(
                "Grand Live skip still waiting on the live cut door: the cut-in opened {} ms ago and no click has landed yet, on the {}th frame of this screen's UpdateView",
                now - armed_ms,
                frames,
            );
        }

        return;
    }

    press(helper, armed_ms, frames);
    CUT_LAST_PRESS_MS.store(now, Ordering::Relaxed);
}

/// Whether the press door has already been crossed by someone other than this build: the game's own skip, or the
/// player's click on the live's skip icon. That crossing is the outcome this lever was asked for, so a press after it
/// would be a press nobody asked for. With the probes off this module's own counting on that door is the answer; with
/// them on `TrainingCuttProbe` is the module holding it, and its count is.
fn cut_skip_already_made() -> bool {
    skip_made_since(skip_door_crossings(), CUT_SKIP_BASELINE.load(Ordering::Relaxed), CUT_OUR_SKIPS.load(Ordering::Relaxed))
}

/// Whether the press door has been crossed by anyone else since this visit opened. The door is shared with the training
/// cut-in's skip, so the raw session total is not an answer: only what moved after this live screen came up belongs to
/// this live, and a crossing this build pressed itself is not someone else's click.
pub(crate) fn skip_made_since(seen: usize, baseline: usize, ours: usize) -> bool {
    seen.saturating_sub(baseline) > ours
}

fn stand_down(this: *mut Il2CppObject) {
    if STOOD_DOWN.swap(true, Ordering::AcqRel) {
        return;
    }

    let armed_ms = CUT_OPEN_MS.load(Ordering::Relaxed);
    let waited = if armed_ms >= 0 { now_ms() - armed_ms } else { 0 };

    info!(
        "Grand Live skip stood down: the live cut skip door was crossed without this build pressing it, {} ms after the live's cut-in opened on {this:p}",
        waited,
    );
}

type ScenarioLiveVoidFn = extern "C" fn(this: *mut Il2CppObject);
type ScenarioLivePhaseFn = extern "C" fn(this: *mut Il2CppObject, phase_type: i32, argument: *mut Il2CppObject);

// The live top's own per frame door. With the switch off this is two atomic loads and the game's own call, which is
// the whole cost of the lever on a build that has it turned off.
def_detour! {
    ScenarioLiveTop_UpdateView(this: *mut Il2CppObject) {
        if AnimationSpeed::skip_grand_live() && !this.is_null() {
            unsafe { pump_press(this) };
        }

        get_orig_fn!(ScenarioLiveTop_UpdateView, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(ScenarioLiveTop_UpdateView, ScenarioLiveVoidFn)(this)
    }
}

// The phase door, counted and never pressed. `SetNextPhase/2 -> void(struct<PhaseType:4B>, class<AbstractPhase.Argument>)`
// (`introspect-run69.log:23283`) is how the live top asks for the phase it wants next. It is armed because the park line
// has to tell two failures apart: a live whose game never asked for the next phase, and a live that asked and did not
// move. Both the four byte enum and the argument reference are handed back untouched. No other module hooks this address.
def_detour! {
    ScenarioLiveTop_SetNextPhase(this: *mut Il2CppObject, phase_type: i32, argument: *mut Il2CppObject) {
        let sets = PHASE_SETS.fetch_add(1, Ordering::Relaxed) + 1;
        let asked = PHASE_TYPE_ASKED.load(Ordering::Relaxed);
        PHASE_TYPE_ASKED.store(phase_type, Ordering::Relaxed);

        if sets <= PROBE_DETAIL_LIMIT {
            let changed = PHASE_CHANGED_MS.load(Ordering::Relaxed);
            let since = if changed >= 0 { now_ms() - changed } else { 0 };

            info!(
                "Grand Live skip phase asked: `SetNextPhase({phase_type})` on {this:p}, crossing {sets} this session, {since} ms into the phase this screen is standing in{}",
                if asked >= 0 && asked != phase_type { format!(", after {asked}") } else { String::new() },
            );
        }

        get_orig_fn!(ScenarioLiveTop_SetNextPhase, ScenarioLivePhaseFn)(this, phase_type, argument);
    }
    bail {
        get_orig_fn!(ScenarioLiveTop_SetNextPhase, ScenarioLivePhaseFn)(this, phase_type, argument)
    }
}

// The live's own button door, counted and never pressed. Run 70 crossed it 0 times in a 12.46 s visit
// (`player-run70.log:2628`), so no press is aimed at it. It stays armed for two things a run needs: a player's click on
// that icon is the only proof this fork has that the icon reaches this handler, and such a click stands the visit down
// instead of competing with it.
def_detour! {
    ScenarioLiveTopGrandLive3D_OnLiveButtonClicked(this: *mut Il2CppObject) {
        let clicks = GAME_CLICKS.fetch_add(1, Ordering::Relaxed) + 1;
        let armed_ms = CUT_OPEN_MS.load(Ordering::Relaxed);

        if armed_ms >= 0 && !STOOD_DOWN.load(Ordering::Relaxed) {
            STOOD_DOWN.store(true, Ordering::Release);

            let beaten = PLAYER_BEAT_US.fetch_add(1, Ordering::Relaxed) + 1;

            if beaten <= PROBE_DETAIL_LIMIT {
                info!("Grand Live skip stood down: the player clicked the live's own button on {this:p} {} ms after the live's cut-in opened", now_ms() - armed_ms);
            }
        }

        if clicks > PROBE_DETAIL_LIMIT && clicks % PROBE_DETAIL_LIMIT == 0 {
            info!("Grand Live skip button door: {clicks} clicks on the live's own button this run");
        }

        get_orig_fn!(ScenarioLiveTopGrandLive3D_OnLiveButtonClicked, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(ScenarioLiveTopGrandLive3D_OnLiveButtonClicked, ScenarioLiveVoidFn)(this)
    }
}

// The live's own cut-in, which is the window the press lives in: `OnPlayCutIn` opens it and `OnEndCutIn` closes it, both
// doors on the helper the cut control holds. Run 70 measured one opening at 00:42:13.940 and closing at 00:42:19.248 on
// view 1621 (`player-run70.log:2414`, `:2532`). These three wrappers are armed only where the probes are off, because
// `StoryEventProbe` holds the pair and `TrainingCuttProbe` holds `SkipRuntime` in a `debug_mode` build, and one address
// carries one hook. Each wrapper is its own `def_detour!` call: that macro defines one wrapper per call (AGENTS section 5).
def_detour! {
    LiveCutWatch_OnPlayCutIn(this: *mut Il2CppObject) {
        CUT_PLAYING.store(true, Ordering::Release);
        CUT_STOP_GUARD.store(false, Ordering::Relaxed);

        get_orig_fn!(LiveCutWatch_OnPlayCutIn, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(LiveCutWatch_OnPlayCutIn, ScenarioLiveVoidFn)(this)
    }
}

def_detour! {
    LiveCutWatch_OnEndCutIn(this: *mut Il2CppObject) {
        CUT_PLAYING.store(false, Ordering::Release);

        get_orig_fn!(LiveCutWatch_OnEndCutIn, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(LiveCutWatch_OnEndCutIn, ScenarioLiveVoidFn)(this)
    }
}

// The press door, watched only where no probe module is holding it. A crossing that is not ours is the game's own skip
// or the player's click, and it ends the visit.
def_detour! {
    LiveCutWatch_SkipRuntime(this: *mut Il2CppObject) {
        if PRESS_INSIDE.load(Ordering::Acquire) {
            CUT_OUR_SKIP_CROSSINGS.fetch_add(1, Ordering::Relaxed);
        }
        else {
            CUT_SKIPS_WATCHED.fetch_add(1, Ordering::Relaxed);

            stand_down(this);
        }

        get_orig_fn!(LiveCutWatch_SkipRuntime, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(LiveCutWatch_SkipRuntime, ScenarioLiveVoidFn)(this)
    }
}

// The control's own doors, armed as evidence rather than as a gate. Run 70 counted 0 crossings of `Play()` and 10 of
// `Stop()` with `Cleanup()` on a live that played a 5,308 ms cut (`player-run70.log:2628`), which is what moved the
// window off this bracket. `Stop` and `Cleanup` still close the window as a guard where this module holds the helper's
// own pair: a control that has just ended its cut has nothing left for a press to do. Run 71 is the shape where that
// guard was the whole failure - 13 stops on the control, the live's own cut-in open on the screen, and the gate closed
// on all 54,648 frames of the visit (`player-run71.log:3542`), because the pair that would have cleared it was armed on
// `StoryEventProbe`. With the probes on, their open cut-in on this screen is the window and a stop is a counted door.
def_detour! {
    ScenarioLiveCutControl_Play(this: *mut Il2CppObject) {
        CUT_OPEN_MS.store(-1, Ordering::Relaxed);
        let plays = CUT_PLAYS.fetch_add(1, Ordering::Relaxed) + 1;

        // Read on the frame the game started the cut, not on every frame after it (AGENTS section 6). Counted apart
        // because a control that starts a cut while saying nothing is playing is a different problem from a cut that
        // runs through the helper without the control crossing `Play()` at all, and run 70 is the second one.
        let bgm = unsafe { read_flag(this, FIELD_CUT_BGM_PLAYING) };

        match bgm {
            Some(true) => { CUT_PLAYS_BGM_TRUE.fetch_add(1, Ordering::Relaxed); }
            Some(false) => { CUT_PLAYS_BGM_FALSE.fetch_add(1, Ordering::Relaxed); }
            None => { CUT_PLAYS_BGM_UNKNOWN.fetch_add(1, Ordering::Relaxed); }
        }

        if plays <= PROBE_DETAIL_LIMIT {
            info!(
                "Grand Live skip control cut started on {this:p}: the live's own cut control crossed `Play()` with its `field <IsPlayingBGM>k__BackingField` reading {}",
                flag_text(bgm),
            );
        }

        get_orig_fn!(ScenarioLiveCutControl_Play, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(ScenarioLiveCutControl_Play, ScenarioLiveVoidFn)(this)
    }
}

def_detour! {
    ScenarioLiveCutControl_Stop(this: *mut Il2CppObject) {
        CUT_STOP_GUARD.store(true, Ordering::Release);
        CUT_PLAYING.store(false, Ordering::Release);
        CUT_OPEN_MS.store(-1, Ordering::Relaxed);
        CUT_STOPS.fetch_add(1, Ordering::Relaxed);

        get_orig_fn!(ScenarioLiveCutControl_Stop, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(ScenarioLiveCutControl_Stop, ScenarioLiveVoidFn)(this)
    }
}

def_detour! {
    ScenarioLiveCutControl_Cleanup(this: *mut Il2CppObject) {
        CUT_STOP_GUARD.store(true, Ordering::Release);
        CUT_PLAYING.store(false, Ordering::Release);
        CUT_OPEN_MS.store(-1, Ordering::Relaxed);
        CUT_STOPS.fetch_add(1, Ordering::Relaxed);

        get_orig_fn!(ScenarioLiveCutControl_Cleanup, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(ScenarioLiveCutControl_Cleanup, ScenarioLiveVoidFn)(this)
    }
}

// The control's own `Skip()`, counted, and - since the park line named an ending that never arrives - clicked
// once per parked phase on that phase's first park line. No measured run had seen the game cross it: 0 in run
// 70's visit (`player-run70.log:2628`) and 0 across 61 min on the other PC. Armed only where `ScenarioLiveProbe`
// is not already holding that address; pressed through the address in both shapes, guarded by `PRESS_INSIDE` so
// this build's own click is not counted as the game's.
def_detour! {
    ScenarioLiveCutControl_Skip(this: *mut Il2CppObject) {
        if !PRESS_INSIDE.load(Ordering::Acquire) {
            let clicks = CUT_GAME_SKIPS.fetch_add(1, Ordering::Relaxed) + 1;

            if clicks <= PROBE_DETAIL_LIMIT {
                info!("Grand Live skip: the game crossed `SingleModeScenarioLiveCutControl::Skip()` on {this:p}, click {clicks} this run");
            }
        }

        get_orig_fn!(ScenarioLiveCutControl_Skip, ScenarioLiveVoidFn)(this);
    }
    bail {
        get_orig_fn!(ScenarioLiveCutControl_Skip, ScenarioLiveVoidFn)(this)
    }
}

/// What this lever has done, on the same tick and the same quiet path rule as the probe reports. A build with the
/// switch off returns on one atomic load and prints nothing at all, so the census is not a cost a player who did not
/// turn the lever on pays (AGENTS section 6). It is the line a run reads to tell "the door was armed" from "the door
/// was armed and the game let the click through" (A4).
pub fn report_if_due() {
    if !AnimationSpeed::skip_grand_live() {
        return;
    }

    let now_us = gate_clock_us();
    let now_sec = now_us / 1_000_000;
    let totals = HOST_TICKS.load(Ordering::Relaxed) + CUT_OUR_SKIPS.load(Ordering::Relaxed) + CUT_INS_OPENED.load(Ordering::Relaxed) + GAME_CLICKS.load(Ordering::Relaxed) + CUT_GAME_SKIPS.load(Ordering::Relaxed) + CONTROL_ESCALATIONS.load(Ordering::Relaxed);
    let last_sec = LAST_REPORT_SEC.load(Ordering::Relaxed);
    let last_totals = LAST_TOTALS.load(Ordering::Relaxed);

    if !report_due(now_sec, last_sec, totals, last_totals, REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, Ordering::Relaxed);
    LAST_TOTALS.store(totals, Ordering::Relaxed);

    let skips_watched = skip_door_crossings();

    info!(
        "Grand Live skip census at {now_sec} s: live top frames {host} over {visits} visits and {phases} live phases described, {cut_ins} live cut-in windows opened and {cut_ins_closed} closed as this lever saw them on the helper, {cut_presses} clicks this build made on `SingleModeTrainingCutInHelper::SkipRuntime()` across {our_crossings} crossings of it this module watched, {skip_watched} crossings of that door by the game or the player this session, the training cut-ins sharing it, {control_skips} crossings of the control's own `Skip()` ({escalated} clicked by this build on a park line), {plays} control `Play()` ({bgm_true} with the controls own `_isPlayingBGM` true, {bgm_false} false, {bgm_none} unreadable) and {stops} `Stop()` or `Cleanup()`, {button_clicks} clicks on the live's own button, {dropped} presses dropped, {grand_top} frames a true Grand Live top | press gate closed: {no_cut} frames with no live cut-in open on this screen, {cut_stopped} frames held by the control's own `Stop()` or `Cleanup()` waiting for the next cut-in, {no_helper} frames with no `field _helper` on the cut control | live motion: {moves} phase changes read off `field _currentPhase`, {phase_sets} crossings of `SetNextPhase()` with the last phase asked for numbered {phase_asked}, {parks} park lines printed",
        host = HOST_TICKS.load(Ordering::Relaxed),
        visits = VISITS_SEEN.load(Ordering::Relaxed),
        phases = PHASES_SEEN.load(Ordering::Relaxed),
        cut_ins = CUT_INS_OPENED.load(Ordering::Relaxed),
        cut_ins_closed = CUT_INS_CLOSED.load(Ordering::Relaxed),
        cut_presses = CUT_OUR_SKIPS.load(Ordering::Relaxed),
        our_crossings = CUT_OUR_SKIP_CROSSINGS.load(Ordering::Relaxed),
        skip_watched = skips_watched,
        control_skips = CUT_GAME_SKIPS.load(Ordering::Relaxed),
        escalated = CONTROL_ESCALATIONS.load(Ordering::Relaxed),
        plays = CUT_PLAYS.load(Ordering::Relaxed),
        bgm_true = CUT_PLAYS_BGM_TRUE.load(Ordering::Relaxed),
        bgm_false = CUT_PLAYS_BGM_FALSE.load(Ordering::Relaxed),
        bgm_none = CUT_PLAYS_BGM_UNKNOWN.load(Ordering::Relaxed),
        stops = CUT_STOPS.load(Ordering::Relaxed),
        button_clicks = GAME_CLICKS.load(Ordering::Relaxed),
        dropped = PRESS_DROPPED.load(Ordering::Relaxed),
        grand_top = GRAND_TOP_FRAMES.load(Ordering::Relaxed),
        no_cut = REFUSE_NO_CUT.load(Ordering::Relaxed),
        cut_stopped = REFUSE_CUT_STOPPED.load(Ordering::Relaxed),
        no_helper = REFUSE_NO_HELPER.load(Ordering::Relaxed),
        moves = PHASE_MOVES.load(Ordering::Relaxed),
        phase_sets = PHASE_SETS.load(Ordering::Relaxed),
        phase_asked = PHASE_TYPE_ASKED.load(Ordering::Relaxed),
        parks = PARKS_REPORTED.load(Ordering::Relaxed),
    );
}

pub fn init(umamusume: *const Il2CppImage) {
    // Armed in every build: a player who never turns `debug_mode` on still gets the skip.
    let Some(top_controller) = class_for_label(umamusume, "Gallop.SingleModeScenarioLiveTopController") else {
        warn!("Grand Live skip: no SingleModeScenarioLiveTopController in this client, the live keeps its own button");

        return;
    };

    let Some(grand_live_3d) = class_for_label(umamusume, "Gallop.SingleModeScenarioLiveTopGrandLive3DController") else {
        warn!("Grand Live skip: no SingleModeScenarioLiveTopGrandLive3DController in this client, the live keeps its own button");

        return;
    };

    let Some(cut_control) = class_for_label(umamusume, "Gallop.SingleModeScenarioLiveCutControl") else {
        warn!("Grand Live skip: no SingleModeScenarioLiveCutControl in this client, the live cut keeps its own skip");

        return;
    };

    // The press door belongs to another class than the live screen: `SkipRuntime/0` sits on
    // `Gallop.SingleModeTrainingCutInHelper` (`introspect-run69.log:27255`), reached from the live's own
    // `field _cutControl` and that control's `field _helper` (`introspect-run66.log:23240`). It is the door the
    // player's own click on the live's skip icon crossed in run 70 (`player-run70.log:2531`).
    let Some(cut_helper) = class_for_label(umamusume, "Gallop.SingleModeTrainingCutInHelper") else {
        warn!("Grand Live skip: no SingleModeTrainingCutInHelper in this client, the live cut keeps its own skip");

        return;
    };

    // `StoryEventProbe` holds the cut-in pair and `TrainingCuttProbe` holds `SkipRuntime()` in a `debug_mode` build, and
    // one address carries one hook, so this module watches the window and the press door itself only where the probes
    // are off, and reads them out of those modules where they are on. The press address is resolved in both shapes: a
    // lever that presses needs an address in a `debug_mode` build too, and leaving it 0 there was a press that could
    // never happen on the build this fork is played on. `TrainingCuttProbe.rs:5057` resolved its own call door the same
    // way for the same reason.
    let cut_watch_here = !Hachimi::instance().config.load().debug_mode;
    let skip_door_here = cut_watch_here;

    let update_addr = unsafe { AnimationSpeed::resolve_method(top_controller, "UpdateView", NO_PARAMS, VOID) };
    let phase_addr = unsafe { AnimationSpeed::resolve_method(top_controller, "SetNextPhase", PHASE_TYPE_AND_ARGUMENT, VOID) };
    let button_addr = unsafe { AnimationSpeed::resolve_method(grand_live_3d, "OnLiveButtonClicked", NO_PARAMS, VOID) };
    let play_addr = unsafe { AnimationSpeed::resolve_method(cut_control, "Play", NO_PARAMS, VOID) };
    let stop_addr = unsafe { AnimationSpeed::resolve_method(cut_control, "Stop", NO_PARAMS, VOID) };
    let cleanup_addr = unsafe { AnimationSpeed::resolve_method(cut_control, "Cleanup", NO_PARAMS, VOID) };
    let skip_addr = unsafe { AnimationSpeed::resolve_method(cut_control, "Skip", NO_PARAMS, VOID) };
    let cut_in_play_addr = unsafe { AnimationSpeed::resolve_method(cut_helper, "OnPlayCutIn", NO_PARAMS, VOID) };
    let cut_in_end_addr = unsafe { AnimationSpeed::resolve_method(cut_helper, "OnEndCutIn", NO_PARAMS, VOID) };
    let cut_skip_addr = unsafe { AnimationSpeed::resolve_method(cut_helper, "SkipRuntime", NO_PARAMS, VOID) };

    unsafe {
        LIVE_CUT_SKIP_ADDR = cut_skip_addr;
        LIVE_CONTROL_SKIP_ADDR = skip_addr;
        FIELD_IS_GRAND_LIVE_TOP = get_field_from_name(top_controller, c"_isGrandLiveTop");
        FIELD_CURRENT_PHASE = get_field_from_name(top_controller, c"_currentPhase");
        FIELD_GRAND_LIVE_3D = get_field_from_name(top_controller, c"_grandLive3DController");
        FIELD_CUT_CONTROL = get_field_from_name(top_controller, c"_cutControl");
        FIELD_CUT_HELPER = get_field_from_name(cut_control, c"_helper");
        FIELD_CUT_BGM_PLAYING = get_field_from_name(cut_control, c"<IsPlayingBGM>k__BackingField");
    }

    CUT_WATCHED_HERE.store(cut_watch_here, Ordering::Release);

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;
    let doors = if cut_watch_here { 10 } else { 6 };

    if update_addr != 0 {
        new_hook!(update_addr, ScenarioLiveTop_UpdateView);
        installed += 1;
    }
    else {
        missing.push("SingleModeScenarioLiveTopController::UpdateView()");
    }

    // Armed in both build shapes: no probe module holds this address, and the park line has to be able to say whether
    // the game asked for the next phase in a build played without `debug_mode`.
    if phase_addr != 0 {
        new_hook!(phase_addr, ScenarioLiveTop_SetNextPhase);
        installed += 1;
    }
    else {
        missing.push("SingleModeScenarioLiveTopController::SetNextPhase(phase, argument)");
    }

    if button_addr != 0 {
        new_hook!(button_addr, ScenarioLiveTopGrandLive3D_OnLiveButtonClicked);
        installed += 1;
    }
    else {
        missing.push("SingleModeScenarioLiveTopGrandLive3DController::OnLiveButtonClicked()");
    }

    for (label, addr) in [
        ("SingleModeScenarioLiveCutControl::Play()", play_addr),
        ("SingleModeScenarioLiveCutControl::Stop()", stop_addr),
        ("SingleModeScenarioLiveCutControl::Cleanup()", cleanup_addr),
    ] {
        if addr != 0 {
            installed += 1;
        }
        else {
            missing.push(label);
        }
    }

    // The control's own doors are armed in the same `new_hook!` pass as the rest, so the wrapper names them one by one.
    if play_addr != 0 {
        new_hook!(play_addr, ScenarioLiveCutControl_Play);
    }

    if stop_addr != 0 {
        new_hook!(stop_addr, ScenarioLiveCutControl_Stop);
    }

    if cleanup_addr != 0 {
        new_hook!(cleanup_addr, ScenarioLiveCutControl_Cleanup);
    }

    // The arming is the half that changes with `debug_mode`, not the address. The probe modules hold these targets in a
    // `debug_mode` build and the registry refuses a second hook on one address.
    if skip_addr != 0 && skip_door_here {
        new_hook!(skip_addr, ScenarioLiveCutControl_Skip);
        installed += 1;
    }
    else if skip_addr == 0 {
        missing.push("SingleModeScenarioLiveCutControl::Skip()");
    }

    if cut_watch_here {
        if cut_in_play_addr != 0 {
            new_hook!(cut_in_play_addr, LiveCutWatch_OnPlayCutIn);
            installed += 1;
        }
        else {
            missing.push("SingleModeTrainingCutInHelper::OnPlayCutIn()");
        }

        if cut_in_end_addr != 0 {
            new_hook!(cut_in_end_addr, LiveCutWatch_OnEndCutIn);
            installed += 1;
        }
        else {
            missing.push("SingleModeTrainingCutInHelper::OnEndCutIn()");
        }

        if cut_skip_addr != 0 {
            new_hook!(cut_skip_addr, LiveCutWatch_SkipRuntime);
            installed += 1;
        }
        else {
            missing.push("SingleModeTrainingCutInHelper::SkipRuntime()");
        }
    }

    // The live flags are not resolved here any more, and the phase class is not looked up by label either: C59's nested
    // type trap makes `class_for_label` miss it (the launch of 2026-10-30 22:54Z printed
    // `no class or no matching overload or no slot: PhaseGrandLiveAnimation` for this lever's own lookup,
    // `run log/player-run67.log:533`), and run 70 counted 1,751 frames on a phase that declares neither flag.
    for (label, field) in [
        ("SingleModeScenarioLiveTopController::field _isGrandLiveTop", unsafe { FIELD_IS_GRAND_LIVE_TOP }),
        ("SingleModeScenarioLiveTopController::field _currentPhase", unsafe { FIELD_CURRENT_PHASE }),
        ("SingleModeScenarioLiveTopController::field _grandLive3DController", unsafe { FIELD_GRAND_LIVE_3D }),
        ("SingleModeScenarioLiveTopController::field _cutControl", unsafe { FIELD_CUT_CONTROL }),
        ("SingleModeScenarioLiveCutControl::field _helper", unsafe { FIELD_CUT_HELPER }),
        ("SingleModeScenarioLiveCutControl::field <IsPlayingBGM>k__BackingField", unsafe { FIELD_CUT_BGM_PLAYING }),
    ] {
        if field.is_null() {
            missing.push(label);
        }
    }

    info!(
        "Grand Live skip: {installed} of {doors} doors armed on SingleModeScenarioLiveTopController, its Grand Live 3D controller, its SingleModeScenarioLiveCutControl and the SingleModeTrainingCutInHelper that control holds in `field _helper` in every build; the lever clicks `SingleModeTrainingCutInHelper::SkipRuntime()` on that helper while the live's own cut-in is open on the live screen, after {PRESS_SETTLE_MS} ms and {PRESS_SETTLE_FRAMES} frames and up to {PRESS_LIMIT} clicks a visit {PRESS_REASK_MS} ms apart; the cut control's `Play`, `Stop` and `Cleanup`, the live's own `OnLiveButtonClicked`, and the controller's own `SetNextPhase(phase, argument)` are armed as counted doors and are never clicked, and the control's own `Skip` is armed counted and is clicked only where a park line says a phase has stopped: once, on the phase's first park line, up to {ESCALATE_LIMIT} times a session; a phase that stands {PARK_AFTER_MS} ms after this visit last moved to a new phase object prints a park line naming what crossed inside the wait, repeated on {PARK_REPEAT_MS} ms while that phase keeps standing; the cut control's own `field <IsPlayingBGM>k__BackingField` is read only on the frames this module prints a line on and is never written; `_isGrandLiveTop` is read for the log only, because run 67 showed the intermediate Grand Live scenario lives answer false; the game's own saved skip setting is never written",
    );

    if !cut_watch_here {
        info!("Grand Live skip: StoryEventProbe arms SingleModeTrainingCutInHelper::OnPlayCutIn() and ::OnEndCutIn() and TrainingCuttProbe arms ::SkipRuntime() in this debug_mode build, so this module reads the live's cut-in window and the skip crossings from those modules and presses through the same addresses rather than arming a second hook on them (one address, one hook)");
    }

    if !missing.is_empty() {
        info!("Grand Live skip: incomplete, no class or no matching overload or no slot: {}", missing.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_press_needs_the_open_cut_in_and_the_helper_it_belongs_to() {
        assert!(cut_press_allowed(true, true));
        assert!(!cut_press_allowed(false, true), "a helper held while no cut-in is running is a skip nobody asked for");
        assert!(!cut_press_allowed(true, false), "an open cut-in with no helper read off the cut control is a press with nothing to click");
        assert!(!cut_press_allowed(false, false), "no window and no object is no click, and no press is ever a guess");
    }

    #[test]
    fn the_window_is_the_helpers_own_cut_in_and_a_closed_control_closes_it() {
        // Run 70: the live's cut-in opened on the helper at 00:42:13.940 and closed at 00:42:19.248 on view 1621
        // (`player-run70.log:2414`, `:2532`) while the cut control crossed `Play()` 0 times over the whole visit
        // (`:2628`). The window is the helper's, and the control's `Stop()` or `Cleanup()` only closes it early.
        assert_eq!(press_gate(true, true, false, 0, 1621), None, "this module watched the helper open it");
        assert_eq!(
            press_gate(true, false, false, 1621, 1621),
            Some(GateHold::NoCutOpen),
            "this module watched it close, and an open run on another module does not override that"
        );
        assert_eq!(press_gate(true, true, true, 0, 1621), Some(GateHold::CutStopped), "a control that just ended its cut has nothing left for a press to do");
        assert_eq!(press_gate(false, false, false, 1621, 1621), None, "the probes hold the pair, and their open cut-in is on this screen");
        assert_eq!(
            press_gate(false, false, false, 1501, 1621),
            Some(GateHold::NoCutOpen),
            "an Inspiration cut-in open on another view is not this live's window"
        );
        assert_eq!(press_gate(false, false, false, 0, 1621), Some(GateHold::NoCutOpen), "nothing open is nothing to press");
    }

    #[test]
    fn a_live_cut_in_open_on_this_screen_outranks_the_stop_guard_the_last_cut_left() {
        // Run 71, the shape run 70 never reached. The live stood on view 1621 from 01:33:23 and was still there at the
        // last line this session wrote (`player-run71.log:3145`, `:3542`), 54,648 frames of `UpdateView` with the press
        // gate closed on every one. Its cut control crossed `Stop()` or `Cleanup()` 13 times and `Play()` 0 times
        // (`:3542`), two of those stops landing after this visit opened, while `StoryEventProbe` had the live's own
        // cut-in open on 1621 - `OnPlayCutIn()` crossing 19 with a run open 1,016 ms at 01:34:09 (`:3228`), the same
        // run closed on that screen after 6,220 ms (`:3263`). The guard those stops left behind is what closed the
        // gate: with the probes holding the helper's own door, their open cut-in on this screen is the window.
        assert_eq!(
            press_gate(false, false, true, 1621, 1621),
            None,
            "the control ended an earlier cut, and the helper has a new one open on this screen right now"
        );
        assert_eq!(
            press_gate(false, false, true, 1101, 1621),
            Some(GateHold::NoCutOpen),
            "a cut-in open on another screen is not this live's window, so the gate stays closed"
        );
        assert_eq!(
            press_gate(false, false, true, 0, 1621),
            Some(GateHold::NoCutOpen),
            "with nothing open on this screen there is no opening for a stop to be outranked by"
        );
    }

    #[test]
    fn the_counted_cut_ins_move_on_the_window_edge_rather_than_on_one_shapes_doors() {
        // Run 71's census said "0 live cut-ins opened" about a live whose cut-in `StoryEventProbe` held open on 1621
        // from 01:34:08 until it closed there after 6,220 ms (`player-run71.log:3542`, `:3263`). Only the helper doors
        // this module arms itself moved that count, and in a `debug_mode` build those doors are armed on
        // `StoryEventProbe`, so the line denied a cut the run had proof of. The count belongs to the window this lever
        // stands in, so it moves on the edges of that window and on nothing else.
        assert_eq!(cut_window_edges(false, true), (true, false), "an opening counts one cut-in");
        assert_eq!(cut_window_edges(true, false), (false, true), "a closing counts its end");
        assert_eq!(cut_window_edges(true, true), (false, false), "a window still open counts nothing again");
        assert_eq!(cut_window_edges(false, false), (false, false), "a window still shut counts nothing");

        let mut was_open = false;
        let mut opened = 0;
        let mut closed = 0;

        // The window run 71's live actually had: open for the frames of the cut, closed when the cut ended, and shut
        // for the rest of a visit that pressed nothing.
        for is_open in [true, true, true, false, false, false] {
            let (edge_opened, edge_closed) = cut_window_edges(was_open, is_open);
            was_open = is_open;
            opened += usize::from(edge_opened);
            closed += usize::from(edge_closed);
        }

        assert_eq!((opened, closed), (1, 1), "one cut-in over six frames, which is what that visit should have reported");
    }

    #[test]
    fn a_true_grand_live_concert_is_not_what_the_press_asks() {
        // Run 67: an intermediate cutscene of a Grand Live scenario stood on view 1621 with the skip icon on screen and
        // `_isGrandLiveTop` false on all 3,501 frames (`run log/player-run67.log:2694` to `:2701`). Run 70 pressed
        // nothing across 1,751 frames of four phases, none of them a phase the flags live in (`:2628`). The flag is
        // census evidence, and the gate asks the cut-in.
        let gate = LiveGate { grand_live_top: false, phase_present: true, ..LiveGate::default() };

        assert!(!gate.grand_live_top);
        assert!(cut_press_allowed(press_gate(true, true, false, 0, 1621).is_none(), true));
    }

    #[test]
    fn a_press_waits_for_the_settle_and_never_for_a_closed_door() {
        // Armed at 1000 ms. The frame count and the clock both have to pass, and a closed door is never due.
        assert!(!live_press_due(1_000, 0, 1_400, 0, -1, true), "no frames yet");
        assert!(!live_press_due(1_000, 5, 1_100, 0, -1, true), "the settle has not passed");
        assert!(live_press_due(1_000, 3, 1_250, 0, -1, true), "both bounds are met at the same time");
        assert!(!live_press_due(1_000, 9, 9_000, 0, -1, false), "a door that closed is not due a click");
        assert!(!live_press_due(-1, 9, 9_000, 0, -1, true), "a door that never opened is never armed");
    }

    #[test]
    fn a_press_that_did_not_end_the_live_is_retried_spaced_and_then_stopped() {
        assert!(!live_press_due(1_000, 9, 2_000, 1, 1_400, true), "one press is not retried inside its own re-ask window");
        assert!(live_press_due(1_000, 9, 3_400, 1, 1_400, true), "the re-ask window has passed");
        assert!(live_press_due(1_000, 9, 3_400, 2, 1_400, true));
        assert!(!live_press_due(1_000, 9, 3_400, PRESS_LIMIT, 1_400, true), "a door that does not end the live is not pressed all the way through it");
    }

    #[test]
    fn a_training_cut_skip_earlier_in_the_session_is_not_this_lives_skip() {
        // `SingleModeTrainingCutInHelper::SkipRuntime()` is the training cut-in's skip door as well as the live's, so the
        // session total is not this visit's answer. Run 70 crossed it once all session (`player-run70.log:2531`); a
        // career session crosses it many times before any live screen is up.
        assert!(skip_made_since(9, 8, 0), "one crossing since this visit opened, and none of them ours");
        assert!(!skip_made_since(9, 9, 0), "crossings from before this visit opened belong to other screens");
        assert!(!skip_made_since(9, 8, 1), "the crossing this build pressed is not someone else's click");
        assert!(!skip_made_since(2, 8, 0), "a count that read backwards is not a crossing");
    }

    #[test]
    fn a_second_live_on_the_same_view_id_opens_its_own_visit() {
        // Run 72 put the game on view 1621 18 times and the lever opened 1 visit
        // (`run log/player20261031-otherpc.log:15867`). `UpdateView` on the live top is not called while the game
        // stands on another screen, so `VISIT_VIEW` still held 1621 when the next live started and one stand-down at
        // 07:56:21 ruled the other 17 lives (C84). The view change count is what says the live in front of the pump
        // is a new one.
        assert!(new_live_visit(1621, 1621, 18, 24), "the live at 07:59:43 came in behind 07:56:23 of other screens");
        assert!(!new_live_visit(1621, 1621, 24, 24), "the live the pump is standing on is the live it is visiting");
        assert!(new_live_visit(VIEW_NONE, 1621, 24, 24), "a live screen with no visit open opens one");
        assert!(new_live_visit(1620, 1621, 7, 7), "the other live screen is its own visit");
    }

    #[test]
    fn a_visit_that_pressed_nothing_is_reported_once() {
        assert!(live_wait_reported(1_000, 9_000, 0, false));
        assert!(!live_wait_reported(1_000, 7_999, 0, false), "the bound is the bound");
        assert!(!live_wait_reported(1_000, 9_000, 1, false), "a visit that clicked is not a visit that waited");
        assert!(!live_wait_reported(1_000, 9_000, 0, true), "the report is once per visit, not once per frame");
    }

    #[test]
    fn a_live_that_stopped_moving_is_a_park_and_a_live_that_is_playing_is_not() {
        // The two visits run 74 lost. `PhaseLive` stood 5,516 ms from the click to the player's reset in the first
        // (`run log/hachimi.log:14610` press, `:14618` reset) and 5,868 ms in the second (`:16573`, `:16576`), with no
        // `PhaseLiveOutWipe` line printed in either. Against them the visit that left: press at 08:18:31.140 and
        // `PhaseLiveOutWipe` 1,238 ms later (`:16179`, `:16180`).
        assert!(park_due(2, 5_516, 100_000, -1), "the phase a skipped live never left");
        assert!(!park_due(2, 1_238, 100_000, -1), "the phase a live left on its own is not a park");
        // The concert phase, which is the first phase of a visit and is not judged: run 74 measured it at 49 s
        // (`run log/hachimi.log:14993`, `:16086`).
        assert!(!park_due(0, 49_000, 100_000, -1), "a live still playing its concert is not a live that stopped");
        assert!(park_due(1, PARK_AFTER_MS, 100_000, -1), "the bound is the bound");
        assert!(!park_due(1, PARK_AFTER_MS - 1, 100_000, -1), "one ms short is not a park");
        // A park that keeps going is read again on the interval so its numbers can be seen moving or frozen, and not
        // before it.
        assert!(!park_due(2, 14_000, 110_000, 100_001), "a park does not print again the moment it prints");
        assert!(park_due(2, 14_000, 110_000, 100_000), "and it does print again on the repeat interval");
    }

    #[test]
    fn a_parked_phase_is_escalated_once_by_the_control_its_frame_handed() {
        assert!(escalate_due(true, true, 0, ESCALATE_LIMIT), "the first park line of a stuck phase clicks the live's own button once");
        assert!(!escalate_due(false, true, 0, ESCALATE_LIMIT), "a repeat park line reads whether that click moved the wait rather than clicking again");
        assert!(!escalate_due(true, false, 0, ESCALATE_LIMIT), "a frame that hands no cut control has nothing to click");
        assert!(!escalate_due(true, true, ESCALATE_LIMIT, ESCALATE_LIMIT), "the session budget is the bound");
    }

    #[test]
    fn the_two_career_live_screens_are_the_only_visits() {
        assert!(live_view(1620), "SingleModeScenarioLiveSelect, `SceneDefine.rs:42`");
        assert!(live_view(1621), "SingleModeScenarioLiveTop, `SceneDefine.rs:43`");
        assert!(!live_view(1501), "the Inspiration screen has its own lever and its own door");
        assert!(!live_view(VIEW_NONE));
    }

    #[test]
    fn this_lever_clicks_one_door_and_counts_the_rest_and_writes_no_setting() {
        // The doors this module arms, spelled as the dump spells them, split into the one it clicks and the ones it
        // only counts, and what it must never reach for. The client's own Grand Live skip setting is a saved flag and
        // stays the player's.
        let pressed = ["SingleModeTrainingCutInHelper::SkipRuntime()"];
        let counted = [
            "SingleModeScenarioLiveTopController::UpdateView()",
            "SingleModeScenarioLiveTopGrandLive3DController::OnLiveButtonClicked()",
            "SingleModeScenarioLiveCutControl::Play()",
            "SingleModeScenarioLiveCutControl::Stop()",
            "SingleModeScenarioLiveCutControl::Cleanup()",
            "SingleModeScenarioLiveCutControl::Skip()",
            "SingleModeTrainingCutInHelper::OnPlayCutIn()",
            "SingleModeTrainingCutInHelper::OnEndCutIn()",
            "SingleModeScenarioLiveTopController::SetNextPhase(phase, argument)",
        ];
        let fields = ["_isGrandLiveTop", "_currentPhase", "_grandLive3DController", "_cutControl", "_helper", "<IsPlayingBGM>k__BackingField"];

        // Run 70 put 1 crossing on the door this lever clicks - the door the player's own click on the skip icon
        // reached (`player-run70.log:2531`) - and 0 on the control's own `Skip()` across the whole session (`:2628`),
        // the same answer the other PC's 61 min session gave.
        assert_eq!(pressed.len(), 1, "one door is clicked, and the run says which one");
        assert!(!counted.contains(&pressed[0]), "the door clicked is not also a door only counted");

        for door in pressed.iter().chain(counted.iter()) {
            assert!(!door.contains("set_"), "a lever that writes a setter is not a lever that clicks");
            assert!(!door.contains("SkipGrandLive") && !door.contains("HighSpeedType"), "the saved skip and high speed settings are not this lever's doors");
        }

        // The doors that would reach the same completion the wrong way: writing the cut control's own playing flag,
        // driving its BGM by hand, or editing the saved Grand Live skip setting. None of them is armed here.
        let never = [
            "SingleModeScenarioLiveCutControl::set_IsPlayingBGM()",
            "SingleModeScenarioLiveCutControl::PlayLiveBGM()",
            "SingleModeScenarioLiveCutControl::StopLiveBGM()",
            "ApplicationSettingSaveLoader::set_IsEnableScenarioLiveSkipGrandLive()",
        ];

        for refused in never {
            assert!(!counted.contains(&refused), "{refused} is not a door this lever arms");
        }

        for field in fields {
            assert!(!field.starts_with("set_"), "the gate reads slots and never names a setter");
            assert!(!field.contains("IsEnable"), "the client's own saved skip setting is not read here, let alone written");
        }
    }

    #[test]
    fn a_visit_that_pressed_nothing_names_the_half_of_the_gate_that_closed_it() {
        // The three halves this module counts, so a run can read which one held the gate and the census can count how
        // many frames each held it for. Run 70 is the first half: 1,751 frames of four phases with no cut-in open
        // anywhere (`player-run70.log:2628`). Run 71 is the second half reporting as the first: 54,648 frames of a live
        // whose cut control had just stopped a cut while its cut-in was open on the screen (`player-run71.log:3542`).
        assert_eq!(press_gate(true, false, false, 1621, 1621), Some(GateHold::NoCutOpen), "no cut-in this module watched is open");
        assert_eq!(press_gate(true, true, true, 0, 1621), Some(GateHold::CutStopped), "the watched shape names the stop guard it honoured");
        assert_eq!(
            press_gate(false, true, true, 0, 1621),
            Some(GateHold::NoCutOpen),
            "with the probes on a stop is not a reason on its own: only a cut-in that is not open is"
        );
        assert_ne!(GateHold::CutStopped, GateHold::NoHelper, "a control that just stopped a cut and a control holding no helper have different fixes");
    }
}
