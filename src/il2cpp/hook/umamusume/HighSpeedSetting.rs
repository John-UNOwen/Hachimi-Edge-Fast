use std::sync::atomic::{self, AtomicBool, AtomicI32};

use crate::{
    core::Hachimi,
    il2cpp::{hook::umamusume::{AnimationSpeed, SaveDataManager, StoryEventProbe, TrainingCuttProbe}, symbols::get_method_addr, types::*}
};

// Story and training High Speed are Gallop settings that the Global options screen does not
// expose, but the save loader holds them and StoryManager owns the paths that pick the highest
// valid value and persist it. Both are reached through the game's own API so no enum value is
// invented here.
//
// StoryManager::GetMaxHighSpeedType/0 -> static struct<HighSpeedType:4B>()
// StoryManager::SaveHighSpeedType/1 -> static void(struct<HighSpeedType:4B>)
// StoryManager::GetSavedHighSpeedSetting/0 -> static struct<HighSpeedType:4B>()
// ApplicationSettingSaveLoader::get_StoryHighSpeedType/0 -> int()
// ApplicationSettingSaveLoader::set_StoryHighSpeedType/1 -> void(int)
// ApplicationSettingSaveLoader::get_TrainingHighSpeedType/0 -> int()
// ApplicationSettingSaveLoader::set_TrainingHighSpeedType/1 -> void(int)
//
// A 4 byte enum is returned in EAX and passed in ECX, so a plain i32 wrapper is correct for all
// of them. StoryManager's are static and take no hidden `this`.
//
// The loader's own story setter is listed above but not wrapped: a run showed
// SaveHighSpeedType moving StoryManager's saved setting while the loader getter kept reporting
// the value last written to disk, so StoryManager is the path that actually changes behaviour.
static mut GET_MAX_HIGH_SPEED_TYPE_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetMaxHighSpeedType, GET_MAX_HIGH_SPEED_TYPE_ADDR, i32,);

static mut GET_SAVED_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetSavedHighSpeedSetting, GET_SAVED_HIGH_SPEED_ADDR, i32,);

static mut SAVE_HIGH_SPEED_TYPE_ADDR: usize = 0;
impl_addr_wrapper_fn!(SaveHighSpeedType, SAVE_HIGH_SPEED_TYPE_ADDR, (), value: i32);

static mut GET_STORY_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_StoryHighSpeedType, GET_STORY_HIGH_SPEED_ADDR, i32, this: *mut Il2CppObject);

static mut GET_TRAINING_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_TrainingHighSpeedType, GET_TRAINING_HIGH_SPEED_ADDR, i32, this: *mut Il2CppObject);

static mut SET_TRAINING_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(set_TrainingHighSpeedType, SET_TRAINING_HIGH_SPEED_ADDR, (), this: *mut Il2CppObject, value: i32);

// Both writes land on state the game owns and keeps: StoryManager's saved High Speed setting
// and the save loader's training value. Writing them without remembering what the game had
// means turning the option off leaves both at the number this module wrote, which is the state
// the game then saves. So each write carries a baseline, and WAS_RAISED is the marker that
// makes the pass back to the option being off actually restore them once, in the shape
// AnimationSpeed uses for the duration constants it rewrites (its Entry baselines and
// its APPLIED_FACTORS markers).
//
// No HighSpeedType value read out of the game can be i32::MIN, so it stands for "nothing
// captured yet" without colliding with a real setting.
const UNSET: i32 = i32::MIN;

// The marker that makes the pass back to the option being off restore the settings once,
// instead of re-asserting or restoring them at every scene change.
static WAS_RAISED: AtomicBool = AtomicBool::new(false);
static RESTORE_WAIT_WARNED: AtomicBool = AtomicBool::new(false);
static CUT_HOLD_WARNED: AtomicBool = AtomicBool::new(false);
static STORY_BASELINE: AtomicI32 = AtomicI32::new(UNSET);
static TRAINING_BASELINE: AtomicI32 = AtomicI32::new(UNSET);
static STORY_LAST_WRITTEN: AtomicI32 = AtomicI32::new(UNSET);
static TRAINING_LAST_WRITTEN: AtomicI32 = AtomicI32::new(UNSET);

// The two spellings this client's dump can use for the game's 4 byte HighSpeedType: the
// `struct<HighSpeedType:4B>` StoryManager's three are dumped as, and `int` for the same value
// spelled as an integer. A reference is not on the list, because every wrapper here holds an
// `i32`, and the matcher now has to prove the value travels in a register.
const HIGH_SPEED_VALUE_CANDIDATES: [Il2CppTypeEnum; 2] = [
    Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE,
    Il2CppTypeEnum_IL2CPP_TYPE_I4,
];

// Which position of the dumped signature carries that value: `GetMaxHighSpeedType/0` and
// `GetSavedHighSpeedSetting/0` carry it in the result, `SaveHighSpeedType/1` in its parameter.
enum ValuePosition { Result, Parameter }

// Resolve one StoryManager static through the static matcher the fork wrote for exactly this shape
// instead of name plus argument count (C7). It rejects an instance method - none of these wrappers
// declare a `this` - a generic, a reference parameter, the wrong return type, and a value type too
// big to travel by value. The candidate that bound is reported so the install line can name it.
unsafe fn resolve_story_manager_static(
    story_manager: *mut Il2CppClass,
    name: &str,
    position: ValuePosition,
) -> Option<(usize, Il2CppTypeEnum)> {
    for candidate in HIGH_SPEED_VALUE_CANDIDATES {
        let addr = match position {
            ValuePosition::Result => AnimationSpeed::resolve_static_method(story_manager, name, &[], candidate),
            ValuePosition::Parameter => AnimationSpeed::resolve_static_method(
                story_manager, name, &[candidate], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
            ),
        };

        if addr != 0 {
            return Some((addr, candidate));
        }
    }

    None
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryManager);
    get_class_or_return!(umamusume, Gallop, ApplicationSettingSaveLoader);

    unsafe {
        let max_type = resolve_story_manager_static(StoryManager, "GetMaxHighSpeedType", ValuePosition::Result);
        let saved_type = resolve_story_manager_static(StoryManager, "GetSavedHighSpeedSetting", ValuePosition::Result);
        let save_type = resolve_story_manager_static(StoryManager, "SaveHighSpeedType", ValuePosition::Parameter);

        GET_MAX_HIGH_SPEED_TYPE_ADDR = max_type.map_or(0, |(addr, _)| addr);
        GET_SAVED_HIGH_SPEED_ADDR = saved_type.map_or(0, |(addr, _)| addr);
        SAVE_HIGH_SPEED_TYPE_ADDR = save_type.map_or(0, |(addr, _)| addr);

        GET_STORY_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"get_StoryHighSpeedType", 0);
        GET_TRAINING_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"get_TrainingHighSpeedType", 0);
        SET_TRAINING_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"set_TrainingHighSpeedType", 1);

        // The spelling each target bound to, so a `class` install - which the matcher refuses in
        // front of an `i32` wrapper - is never mistaken for the `struct<HighSpeedType:4B>` the dump
        // spells, and so an option that went inert says so at install instead of after a run.
        debug!(
            "HighSpeedSetting: StoryManager statics max {}, saved {}, setter {}",
            AnimationSpeed::candidate_word(max_type.map(|(_, candidate)| candidate)),
            AnimationSpeed::candidate_word(saved_type.map(|(_, candidate)| candidate)),
            AnimationSpeed::candidate_word(save_type.map(|(_, candidate)| candidate)),
        );
    }
}

// Hidden for now (decision of 2026-10-12, knowledge/decisions/hide-high-speed-settings-2026-10-12.md). This pass
// raises the game's own saved story and training High Speed values to the game maximum, and within one session it
// put them back to the maximum after the save had read 0. The config key stays, so an existing config still loads,
// but the pass reads it as off until this constant is removed.
const HIGH_SPEED_SETTINGS_HIDDEN: bool = true;

// The value the pass acts on, read from the config key. The Config snapshot line reports this value, so a run's log
// says what the run did rather than what the file says.
pub(crate) fn effective_enabled(config_value: bool) -> bool {
    !HIGH_SPEED_SETTINGS_HIDDEN && config_value
}

// Runs on the game main thread wherever the speed groups are applied, so a config change taken
// from the Config Editor lands at the next scene change. The writes are idempotent: the current
// value is read first and only a lower value is raised, and what the game held before the first
// write is put back once when the option is turned off.
pub fn apply() {
    // Charged to the pass that runs this: `AnimationSpeed::apply` reaches this module, this
    // module reads the config, and a pass total that leaves that read out is the half of the
    // cost C36 never measured.
    AnimationSpeed::note_config_read();

    let enabled = effective_enabled(Hachimi::instance().config.load().high_speed_settings);

    // Three inputs, each read here, on the game thread, in the same pass that writes, and each handed to
    // `plan_pass` as an argument the way `AnimationSpeed::tween_clocks` hands the gate to
    // `tween_clocks_holding` (`AnimationSpeed.rs:878`, `:888`). A career screen and a cut-in stamp are what
    // `25ccda1` asked and `529e4f5` deleted; `cut_gate_engaged()` is the boundary `529e4f5` measured. The pass
    // asks all three, because the refusal set has to be the union: the gate is coverage added to the guard, not
    // the guard replaced by a gate.
    let career_screen = TrainingCuttProbe::on_a_career_screen();
    let cut_in_running = StoryEventProbe::cut_in_engine_driving();
    let cut_open = TrainingCuttProbe::cut_gate_engaged();

    match plan_pass(enabled, WAS_RAISED.load(atomic::Ordering::Acquire), career_screen, cut_in_running, cut_open) {
        Pass::Idle => return,
        Pass::Hold => {
            warn_cut_hold();
            return;
        }
        Pass::Raise => apply_raise(),
        Pass::Restore => apply_restore_with_loader(),
    }
}

// Its own slot, so holding a write back does not spend the line that says the save loader is not
// ready yet. It speaks once per session because the pass runs at every view change. It names all three
// reasons because the pass refuses on any one of them, and run 57's refusal at 20:45:29.864 was the
// career screen half while its cut census six seconds earlier read `open 0 ms`
// (`run log/hachimi-run57.log:1854`, `:1827`).
fn warn_cut_hold() {
    if !CUT_HOLD_WARNED.swap(true, atomic::Ordering::AcqRel) {
        warn!("HighSpeedSetting: left the story and training settings alone, a training cut was open, a career screen was up, or a cut in runtime was still being stepped");
    }
}

// The guard's own refusal prints its line, above. The other thing a run needs to know - whether the
// doors the guard stands on armed at all, so that `false` from the gate means "no cut is open" rather
// than "nothing can ever report a cut" - is not knowable from this module: it is a fact about
// `TrainingCuttProbe`'s arming, and `TrainingCuttProbe` prints it at install (`cut_gate_armed_line` /
// `cut_gate_missing_line`, at warn level, so it survives the `Info` file filter of a played build:
// `core/log.rs:3-9`). This pass adds no line of its own about that half, and reads nothing from that
// module beyond the shipped `cut_gate_engaged()` and `on_a_career_screen()`.

// Which pass a call is, kept free of il2cpp so the sequence is testable the way Time.rs's
// plan_write is. Off with nothing ever written touches nothing; off with something written is
// the restore; on is the raise.
//
// Three inputs say whether an owed pass runs:
//
// - `career_screen` is `TrainingCuttProbe::on_a_career_screen()`, and `cut_in_running` is
//   `StoryEventProbe::cut_in_engine_driving()`. Those two are the whole condition `25ccda1` shipped and
//   `529e4f5` (HEAD) still held `Pass::Raise | Pass::Restore` on - a career screen up, or a stamp on
//   `SingleModeTrainingCutInHelper::OnTimelineUpdatePost` inside the last second - and both are still asked.
//   `held_by_the_screen_and_the_engine` below is that condition spelled once, so the claim that this guard
//   refuses at least what HEAD refused is a function a test can compare against rather than a sentence.
// - `cut_open` is `TrainingCuttProbe::cut_gate_engaged()`: the measured boundary of a training cut in the
//   game, open from `PlayTrainingCut` / `PlayScenarioTrainingCut`, closed by `CleanUpCutt`, aged on the game
//   thread's own tick and ended at `CUT_GATE_HOLD_TICKS 900` when the close door never arrives, armed
//   whatever `debug_mode` says (`TrainingCuttProbe.rs`'s `CUT_GATE_HOLD_TICKS` at `:1291` and `arm_cut_gate`
//   at `:1515`, shipped by `529e4f5`).
//
// A pass that would write under any one of them is `Hold`: nothing is touched, and what the pass owed stays
// owed so the next view change or config change runs it. The refusal set is `HEAD's condition ∪ the gate`,
// and it is nothing wider than that.
//
// Run 16 is why the pass asks a boundary at all. Its raise landed at 09:00:22 and its restore at 09:03:24.9,
// and the training cut that never closed opened 3.5 s after that restore, its runtime stepped every frame to
// the end of the log with the value the cut read from these settings changing across the window
// (`GetTimeScaleByHighSpeedType(0) -> 8` at 09:00:28 where the same door answered 1 at 09:00:23). Run 16's own
// log is gone from the tree - `LEDGER.md:2701` names the file it was - so these numbers are quoted from the
// ledger's run 16 note as printed 2026-10-12: `LEDGER.md:2756` for the write, `:2759` for the restore, `:2760`
// for the cut that opened 3.5 s later, `:2758` for the 8 against the 1, and `:2737-2744` for the engine stepped
// every frame and the door at 21,427 calls by the 09:04:49 totals. No door-pair boundary and no screen test
// reaches a write that lands 3.5 s *before* a cut opens, so neither HEAD's guard, nor the gate, nor this union
// closes that half of C57; the union restores the coverage the gate-only build had dropped and adds the
// measured boundary to it.
//
// What the union does and does not do on the run the gate was measured on, stated plainly. Run 57 ran
// `Hachimi v0.32.0-529e4f5` (`run log/hachimi-run57.log:1`), and the gate's own window there is
// `PlayTrainingCut(info) call 1` at 20:44:19.183 to `CleanUpCutt() call 1` at 20:44:28.080 (`:1605`, `:1720`)
// - 8.9 s - inside which this module ran no pass at all: its own lines bracket that window at 20:44:02.982,
// where a pair landed (`:1470-1472`), and at 20:45:18.071 (`:1811-1813`), the option was on and raised through
// it, a raise prints its read line, and the session's first refusal line is the one at 20:45:29.864 (`:1854`),
// so nothing was held in between. What HEAD refused inside the window is the story half, at 20:44:23.512
// (`:1691`), and `529e4f5` - the build that printed it - reached that refusal on the stamp alone; in this tree
// the same refusal is `StoryTimelineController.rs:664-665`, on the stamp union the gate. The stamp was hot
// there: the door's counter is absent from the 20:44:14 totals, which is how this probe prints a zero
// (`StoryEventProbe.rs:800-802`, `:1589`), and it reads 230 at the 20:44:33 totals (`:1730`), so every crossing
// the door made lies inside the 19 s bracket that holds the gate window. On this run the gate half adds no
// refusal of its own, and the story write at 20:44:33.536 (`:1732`) sits 5.5 s past the close door.
//
// What the gate-only shape loses on this run is one logged pass, not a stretch of traffic. It is the pass HEAD
// held at 20:45:29.864 (`:1854`): the gate had closed 61 s earlier (`:1720`) and `PlayTrainingCut` stayed at 1
// to the last totals of the run (`:1872`, `:3707`), while the view the guard reads was 1200 - `SingleModePaddock`
// (`SceneDefine.rs:27`), one of `TrainingCuttProbe::VIEW_TRAINING` (`:657`) - requested at 20:45:18.894
// (`:1820`), named by the two cut-in closes at 20:45:25.846 and 20:45:26.514 (`:1844`, `:1850`), and still
// current 71 ms later when `next_view_id = 1101` was requested (`:1855`). HEAD's refusal there can only be the
// screen half, because the stamp cannot have been hot: the door reads 230 at 20:44:33 (`:1730`), 230 again at
// 20:45:25 (`:1840`) and 230 again at 20:45:45 (`:1884`), so it crossed 0 times across the whole 20:45:18 to
// 20:45:40 window, and the two cut-ins that window did play, 343 ms and 662 ms on view 1200 (`:1844`, `:1850`),
// stepped it no times. The union recovers that one refusal through HEAD's condition and adds none of its own;
// `run_57s_window_from_20_45_18_to_20_45_40_hands_what_head_handed_and_holds_what_head_held` below counts it
// instead of leaving it to this sentence.
//
// The raise that landed at 20:45:18.071 (`:1812-1813`) is refused by none of the three shapes: the screen
// answer was false, the stamp was cold by at least 44 s (the door read 230 at 20:44:33, `:1730`, and 230 again
// at 20:45:25, `:1840`), and the gate was closed - `CleanUpCutt` crossed 55.6 ms *later* (`:1815`), and no gate
// reads a close as evidence of a cut that had already been stepped. C57 records no cause; this is a limit on
// where the writes land, not a proven fix for the stall.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pass {
    Idle,
    // A write was owed and one of the three inputs says wait: refused here, owed on the next pass.
    Hold,
    Raise,
    Restore,
}

/// The condition `25ccda1` shipped and `529e4f5` held: a career screen up, or the cut-in engine stepped
/// inside the last second. Spelled on its own so both directions of the superset are testable - every pass
/// this answers true is held whatever the gate says, and the guard holds nothing this answers false unless
/// the gate says a cut is open.
fn held_by_the_screen_and_the_engine(career_screen: bool, cut_in_running: bool) -> bool {
    career_screen || cut_in_running
}

/// The shipped refusal: HEAD's condition, or an open training cut. Never neither, and never a fourth thing.
fn hold_write_asked(career_screen: bool, cut_in_running: bool, cut_open: bool) -> bool {
    held_by_the_screen_and_the_engine(career_screen, cut_in_running) || cut_open
}

fn plan_pass(enabled: bool, raised: bool, career_screen: bool, cut_in_running: bool, cut_open: bool) -> Pass {
    let owed = if enabled {
        Pass::Raise
    } else if raised {
        Pass::Restore
    } else {
        Pass::Idle
    };

    if owed != Pass::Idle && hold_write_asked(career_screen, cut_in_running, cut_open) { Pass::Hold } else { owed }
}

// Raise only, the direction guard A11 forced: a setting already at or above the ceiling the
// game reported is left alone, because GetMaxHighSpeedType is context dependent and a "differs"
// test let a menu-side ceiling of 1 write back over a story setting already raised to 2.
fn raise_value(current: i32, max: i32) -> Option<i32> {
    (current < max).then_some(max)
}

// Restore only what this module actually changed: a setting that already holds its baseline, or
// one that never had a baseline captured, is left alone.
fn restore_value(current: i32, baseline: i32) -> Option<i32> {
    (baseline != UNSET && current != baseline).then_some(baseline)
}

fn apply_restore_with_loader() {
    // The training setting lives on the save loader, so the restore needs the same handle the
    // write used. It does not exist before the save is loaded; the marker stays set and the
    // next scene change or game tick runs the pass again. That retry runs on a game tick, so
    // the reason it is waiting is told once instead of once per tick.
    let save_data_manager = SaveDataManager::instance();
    if save_data_manager.is_null() {
        warn_once("SaveDataManager has no instance yet");
        return;
    }

    let loader = SaveDataManager::get_SaveLoader(save_data_manager);
    if loader.is_null() {
        warn_once("SaveDataManager::get_SaveLoader returned null");
        return;
    }

    apply_restore(loader);
}

fn apply_raise() {
    let max = GetMaxHighSpeedType();

    if max <= 0 {
        warn!("HighSpeedSetting: GetMaxHighSpeedType gave {}, leaving the settings alone", max);
        return;
    }

    let save_data_manager = SaveDataManager::instance();
    if save_data_manager.is_null() {
        warn!("HighSpeedSetting: SaveDataManager has no instance yet");
        return;
    }

    let loader = SaveDataManager::get_SaveLoader(save_data_manager);
    if loader.is_null() {
        warn!("HighSpeedSetting: SaveDataManager::get_SaveLoader returned null");
        return;
    }

    let saved = GetSavedHighSpeedSetting();
    let story = get_StoryHighSpeedType(loader);
    let training = get_TrainingHighSpeedType(loader);

    debug!("HighSpeedSetting: max {}, saved story {}, loader story {}, training {}", max, saved, story, training);

    // StoryManager::SaveHighSpeedType changes StoryManager's own saved setting. The save
    // loader getter keeps reporting the value last written to disk, so comparing against it
    // re-applies the same write at every scene change.
    let (saved_resolved, training_resolved) = unsafe {
        (GET_SAVED_HIGH_SPEED_ADDR != 0, GET_TRAINING_HIGH_SPEED_ADDR != 0)
    };

    if let Some(value) = raise_value(saved, max) {
        // Read before the write lands: after it the getter reports our own value.
        observe(&STORY_BASELINE, &STORY_LAST_WRITTEN, saved, saved_resolved);

        SaveHighSpeedType(value);
        mark_written(&STORY_LAST_WRITTEN, value);

        let after = GetSavedHighSpeedSetting();
        info!("HighSpeedSetting: story high speed {} -> {} via StoryManager::SaveHighSpeedType, saved now {}", saved, value, after);
    }

    if let Some(value) = raise_value(training, max) {
        observe(&TRAINING_BASELINE, &TRAINING_LAST_WRITTEN, training, training_resolved);

        set_TrainingHighSpeedType(loader, value);
        mark_written(&TRAINING_LAST_WRITTEN, value);

        let after = get_TrainingHighSpeedType(loader);
        info!("HighSpeedSetting: training high speed {} -> {}, read back {}", training, value, after);
    }
}

// The baseline each write restores to, in the shape AnimationSpeed's entries use
// (AnimationSpeed.rs:236-241 and the re-observation at `:3869-3881`): the last value the game itself left in
// that setting,
// re-observed whenever the current value differs from what this module last wrote, so a value
// the game changed on its own becomes the new baseline instead of a stale snapshot. A value is
// only captured from a getter whose address resolved: a wrapper on an unresolved target returns
// zeroed memory, and writing that back would invent a setting the game never held.
fn observe(baseline: &AtomicI32, last_written: &AtomicI32, current: i32, resolved: bool) {
    if !resolved {
        return;
    }

    let written = last_written.load(atomic::Ordering::Relaxed);

    if written == UNSET || current != written {
        baseline.store(current, atomic::Ordering::Relaxed);
    }
}

fn mark_written(last_written: &AtomicI32, value: i32) {
    last_written.store(value, atomic::Ordering::Relaxed);
    WAS_RAISED.store(true, atomic::Ordering::Release);
}

// apply() runs on a game tick while the marker is set, so the reason a restore is still
// waiting is told once per session instead of once per tick.
fn warn_once(message: &str) {
    if !RESTORE_WAIT_WARNED.swap(true, atomic::Ordering::AcqRel) {
        warn!("HighSpeedSetting: {message}");
    }
}

// The pass that puts back what the game had, gated by the same kind of marker
// `AnimationSpeed`'s `APPLIED_FACTORS` is for the duration constants: turning the option off
// runs it exactly once instead of re-asserting two settings at every scene change. Both values go
// back through the game's own API, the same paths the raise used, and a setting that already
// holds its baseline is left alone.
fn apply_restore(loader: *mut Il2CppObject) {
    let mut restored = 0;

    let current = GetSavedHighSpeedSetting();

    if let Some(value) = restore_value(current, STORY_BASELINE.load(atomic::Ordering::Relaxed)) {
        SaveHighSpeedType(value);
        STORY_LAST_WRITTEN.store(value, atomic::Ordering::Relaxed);
        restored += 1;

        info!("HighSpeedSetting: story high speed {} -> {} restored to the value StoryManager held before this option wrote it", current, value);
    }

    let current = get_TrainingHighSpeedType(loader);

    if let Some(value) = restore_value(current, TRAINING_BASELINE.load(atomic::Ordering::Relaxed)) {
        set_TrainingHighSpeedType(loader, value);
        TRAINING_LAST_WRITTEN.store(value, atomic::Ordering::Relaxed);
        restored += 1;

        info!("HighSpeedSetting: training high speed {} -> {} restored to the value the save loader held before this option wrote it", current, value);
    }

    // One pass, whatever it found. Leaving the marker set would retry the same restore at
    // every scene change for the rest of the session.
    WAS_RAISED.store(false, atomic::Ordering::Release);

    if restored == 0 {
        debug!("HighSpeedSetting: option off, nothing this module wrote is out of place");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The module's state machine without il2cpp: `saved` is StoryManager's setting, `training`
    // the save loader's, and the two write lists are the values handed to the game's own setters.
    // Every decision here is the production one - `plan_pass`, `raise_value`, `restore_value` and
    // `observe` are the same functions `apply`, `apply_raise` and `apply_restore` call.
    struct Sim {
        saved: i32,
        training: i32,
        story_baseline: AtomicI32,
        training_baseline: AtomicI32,
        story_last: AtomicI32,
        training_last: AtomicI32,
        raised: bool,
        // What the shipped guard reads - `TrainingCuttProbe::on_a_career_screen()`,
        // `StoryEventProbe::cut_in_engine_driving()` and `TrainingCuttProbe::cut_gate_engaged()` - stood in
        // as the three bools the shipped readers get, so the shipped `plan_pass` makes this sim's decisions
        // too.
        career_screen: bool,
        cut_in_running: bool,
        cut_open: bool,
        story_writes: Vec<i32>,
        training_writes: Vec<i32>,
    }

    impl Sim {
        fn new(saved: i32, training: i32) -> Self {
            Self {
                saved,
                training,
                story_baseline: AtomicI32::new(UNSET),
                training_baseline: AtomicI32::new(UNSET),
                story_last: AtomicI32::new(UNSET),
                training_last: AtomicI32::new(UNSET),
                raised: false,
                career_screen: false,
                cut_in_running: false,
                cut_open: false,
                story_writes: Vec::new(),
                training_writes: Vec::new(),
            }
        }

        fn apply(&mut self, enabled: bool, max: i32, getters_resolved: bool) {
            match plan_pass(enabled, self.raised, self.career_screen, self.cut_in_running, self.cut_open) {
                Pass::Idle | Pass::Hold => {}
                Pass::Raise => self.raise(max, getters_resolved),
                Pass::Restore => self.restore(),
            }
        }

        fn raise(&mut self, max: i32, resolved: bool) {
            if let Some(value) = raise_value(self.saved, max) {
                observe(&self.story_baseline, &self.story_last, self.saved, resolved);

                self.saved = value;
                self.story_last.store(value, atomic::Ordering::Relaxed);
                self.raised = true;
                self.story_writes.push(value);
            }

            if let Some(value) = raise_value(self.training, max) {
                observe(&self.training_baseline, &self.training_last, self.training, resolved);

                self.training = value;
                self.training_last.store(value, atomic::Ordering::Relaxed);
                self.raised = true;
                self.training_writes.push(value);
            }
        }

        fn restore(&mut self) {
            if let Some(value) = restore_value(self.saved, self.story_baseline.load(atomic::Ordering::Relaxed)) {
                self.saved = value;
                self.story_last.store(value, atomic::Ordering::Relaxed);
                self.story_writes.push(value);
            }

            if let Some(value) = restore_value(self.training, self.training_baseline.load(atomic::Ordering::Relaxed)) {
                self.training = value;
                self.training_last.store(value, atomic::Ordering::Relaxed);
                self.training_writes.push(value);
            }

            self.raised = false;
        }
    }

    #[test]
    fn turning_the_option_off_restores_what_the_game_had() {
        // The values the ledger runs reported: saved story 0, loader story 1, max 2.
        let mut sim = Sim::new(0, 1);

        sim.apply(true, 2, true);
        assert_eq!(sim.story_writes, vec![2]);
        assert_eq!(sim.training_writes, vec![2]);

        for _ in 0..20 {
            sim.apply(true, 2, true);   // twenty scene changes with the option still on
        }

        assert_eq!(sim.story_writes, vec![2], "the raise re-applied itself {}", sim.story_writes.len() - 1);
        assert_eq!(sim.training_writes, vec![2]);

        sim.apply(false, 2, true);      // the option is turned off
        assert_eq!(sim.story_writes, vec![2, 0]);
        assert_eq!(sim.training_writes, vec![2, 1]);
        assert_eq!(sim.saved, 0, "StoryManager kept the value this module wrote");
        assert_eq!(sim.training, 1, "the save loader kept the value this module wrote");
    }

    #[test]
    fn the_restore_runs_once_not_once_per_scene_change() {
        let mut sim = Sim::new(1, 1);

        sim.apply(true, 2, true);
        sim.apply(false, 2, true);

        for _ in 0..20 {
            sim.apply(false, 2, true);
        }

        assert_eq!(sim.story_writes, vec![2, 1]);
        assert_eq!(sim.training_writes, vec![2, 1]);
    }

    #[test]
    fn a_value_the_game_changed_itself_is_the_baseline() {
        let mut sim = Sim::new(0, 1);

        sim.apply(true, 2, true);
        sim.saved = 1;                  // the game's own options path wrote the setting back
        sim.apply(true, 2, true);      // re-raised, and the game's 1 becomes the baseline
        sim.apply(false, 2, true);

        assert_eq!(sim.saved, 1, "the restore went back past the value the game last chose");
        assert_eq!(sim.training, 1);
    }

    #[test]
    fn an_unresolved_getter_invents_no_setting_to_restore() {
        let mut sim = Sim::new(0, 1);

        sim.apply(true, 2, false);     // the writes still happen
        sim.apply(false, 2, false);

        assert_eq!(sim.story_writes, vec![2]);
        assert_eq!(sim.training_writes, vec![2]);
        assert_eq!(sim.saved, 2);
        assert_eq!(sim.training, 2);
    }

    #[test]
    fn an_option_that_was_never_on_writes_nothing() {
        let mut sim = Sim::new(0, 1);

        for _ in 0..5 {
            sim.apply(false, 2, true);
        }

        assert!(sim.story_writes.is_empty());
        assert!(sim.training_writes.is_empty());
        assert_eq!(sim.saved, 0);
        assert_eq!(sim.training, 1);
    }

    // HEAD (`529e4f5`) held `Pass::Raise | Pass::Restore` on
    // `TrainingCuttProbe::on_a_career_screen() || StoryEventProbe::cut_in_engine_driving()`. Kept as a
    // test-only replica of that condition so the superset is a checked claim: the guard in this file must
    // refuse every pass this refuses, and must refuse nothing neither this nor the gate refuses.
    fn head_held(career_screen: bool, cut_in_running: bool) -> bool {
        career_screen || cut_in_running
    }

    // Every pass that owes a write: the raise (option on), the restore (option off, a write owed), and the
    // raise the option wins even though a write is owed.
    const OWED_PASSES: [(bool, bool); 3] = [(true, false), (false, true), (true, true)];

    #[test]
    fn every_pass_head_held_is_still_held_whatever_the_cut_gate_answers() {
        // The superset direction, over all eight pairs of HEAD's two inputs and both gate answers.
        for (enabled, raised) in OWED_PASSES {
            for career_screen in [false, true] {
                for cut_in_running in [false, true] {
                    for cut_open in [false, true] {
                        if !head_held(career_screen, cut_in_running) {
                            continue;
                        }

                        assert_eq!(
                            plan_pass(enabled, raised, career_screen, cut_in_running, cut_open),
                            Pass::Hold,
                            "the gate let through a pass HEAD held: career_screen {career_screen}, cut_in_running {cut_in_running}, cut_open {cut_open}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_guard_holds_nothing_beyond_head_held_or_an_open_cut() {
        // The other direction: the refusal set is exactly HEAD's condition union the gate, and the union
        // added no fourth reason. Over all 24 owed input shapes, the answer is HEAD's condition or the gate,
        // and it is `Hold` for at least as many shapes as HEAD held.
        let mut head_holds = 0;
        let mut shipped_holds = 0;

        for (enabled, raised) in OWED_PASSES {
            for career_screen in [false, true] {
                for cut_in_running in [false, true] {
                    for cut_open in [false, true] {
                        let head = head_held(career_screen, cut_in_running);
                        let shipped = plan_pass(enabled, raised, career_screen, cut_in_running, cut_open);

                        head_holds += usize::from(head);
                        shipped_holds += usize::from(shipped == Pass::Hold);

                        assert_eq!(
                            shipped,
                            if head || cut_open { Pass::Hold } else { if enabled { Pass::Raise } else { Pass::Restore } },
                            "the refusal is not HEAD's condition union the gate: career_screen {career_screen}, cut_in_running {cut_in_running}, cut_open {cut_open}",
                        );
                    }
                }
            }
        }

        // 3 owed shapes x 2 x 2 x 2 = 24 input shapes: HEAD holds 18 of them, the union 21.
        assert_eq!(head_holds, 3 * 3 * 2, "HEAD's own refusal set is not what its condition says");
        assert_eq!(shipped_holds, 21, "the union did not add the gate's refusal half");
    }

    #[test]
    fn the_cut_gate_adds_a_refusal_head_did_not_make() {
        // The half that is genuinely new: a training cut open while no career screen is up and the cut-in
        // door is quiet - a cut played off the career screens this probe names. HEAD hands this pass, and the
        // shipped guard holds it.
        assert!(!head_held(false, false));
        assert_eq!(plan_pass(true, false, false, false, true), Pass::Hold, "an open cut did not add a refusal");
        assert_eq!(plan_pass(false, true, false, false, true), Pass::Hold, "an open cut did not hold the restore");
    }

    #[test]
    fn a_write_asked_while_a_training_cut_is_open_is_held_not_run() {
        // The window the gate measured on the run that armed it: `PlayTrainingCut(info) call 1` and
        // `Cutt gate: training cut 1 playing` at 20:44:19.183, `CleanUpCutt() call 1` at 20:44:28.080
        // (`run log/hachimi-run57.log:1604-1605`, `:1720`), and the story half writing 5.46 s after that close
        // door at 20:44:33.536 (`:1732`). A raise asked for inside the first window writes nothing.
        let mut sim = Sim::new(0, 1);
        sim.cut_open = true;

        sim.apply(true, 2, true);

        assert!(sim.story_writes.is_empty(), "the raise ran through an open cut: {:?}", sim.story_writes);
        assert!(sim.training_writes.is_empty(), "the raise ran through an open cut: {:?}", sim.training_writes);
        assert_eq!(sim.saved, 0, "StoryManager's setting changed while a training cut was open");
        assert_eq!(sim.training, 1, "the save loader's training setting changed while a training cut was open");
        assert!(!sim.raised, "a pass that wrote nothing recorded a write it did not make");
    }

    #[test]
    fn a_held_raise_lands_when_the_cut_closes() {
        let mut sim = Sim::new(0, 1);
        sim.cut_open = true;

        for _ in 0..20 {
            sim.apply(true, 2, true);   // twenty view changes while the cut sits open
        }

        assert!(sim.story_writes.is_empty());

        sim.cut_open = false;          // `CleanUpCutt`, or the gate's 900 tick bound
        sim.apply(true, 2, true);
        assert_eq!(sim.story_writes, vec![2], "the deferred raise never landed");
        assert_eq!(sim.training_writes, vec![2]);

        sim.apply(false, 2, true);
        assert_eq!(sim.story_writes, vec![2, 0]);
        assert_eq!(sim.training_writes, vec![2, 1]);
    }

    #[test]
    fn a_restore_held_by_an_open_cut_does_not_consume_the_owed_write() {
        // A restore asked for while the game's own cut is open is the other direction of the write run 16
        // landed inside its open window: its `story high speed 1 -> 2` and `training high speed 1 -> 2` pair at
        // 09:04:51, after its last cut opened 09:03:28.4 and while the ladder line still read `open 90492 ms`
        // (`LEDGER.md:2761`, `:2760`, `:2733` as printed 2026-10-12 - run 16's log is not in the tree,
        // `LEDGER.md:2701`). Consuming `WAS_RAISED` on a held pass would leave the value this module wrote on
        // the game's settings for the rest of the session, so the marker is what carries the write to the next
        // pass.
        let mut sim = Sim::new(0, 1);
        sim.apply(true, 2, true);
        sim.cut_open = true;

        for _ in 0..20 {
            sim.apply(false, 2, true);   // the option off while the cut stays open
        }

        assert_eq!(sim.story_writes, vec![2], "the restore ran {} time(s) through an open cut", sim.story_writes.len() - 1);
        assert!(sim.raised, "a held restore consumed the marker that makes the write owed");
        assert_eq!(sim.saved, 2, "the value this module wrote was left on StoryManager");

        sim.cut_open = false;
        sim.apply(false, 2, true);
        assert_eq!(sim.story_writes, vec![2, 0]);
        assert_eq!(sim.training_writes, vec![2, 1]);
        assert_eq!(sim.saved, 0);
        assert_eq!(sim.training, 1);
        assert!(!sim.raised, "the restore that landed left the marker set");
    }

    #[test]
    fn a_career_screen_with_no_cut_open_holds_the_write_the_gate_alone_left_closed() {
        // The coverage `529e4f5` dropped and this guard holds again. Run 57 held its pass at 20:45:29.864 on
        // HEAD's career screen half - view 1200, `SingleModePaddock`, one of `VIEW_TRAINING` - while its cut
        // census read `cut runs 1 wall 8898 ms timeline 0 ms open 0 ms` six seconds earlier
        // (`run log/hachimi-run57.log:1854`, `:1827`) and its cut gate had closed at `CleanUpCutt`
        // 20:44:28.080 (`:1720`). A guard standing on the gate alone answers closed there and writes.
        //
        // The price is the one item 58 named, paid on purpose: a career screen with nothing playing is a
        // reason to wait, so a raise made at a menu is read by the next turn rather than this one. Whether
        // that costs wall clock on this client is still the run item 58 asks for.
        let mut sim = Sim::new(0, 1);
        sim.career_screen = true;

        for _ in 0..20 {
            sim.apply(true, 2, true);   // twenty view changes on career screens with no cut open
        }

        assert!(sim.story_writes.is_empty(), "a career screen did not hold the write");

        sim.career_screen = false;
        sim.apply(true, 2, true);
        assert_eq!(sim.story_writes, vec![2], "the deferred raise never landed");
    }

    #[test]
    fn a_cut_in_stepped_inside_the_last_second_holds_the_write_the_gate_alone_left_closed() {
        // The other half `529e4f5` deleted: a stamp on `SingleModeTrainingCutInHelper::OnTimelineUpdatePost`
        // inside the last second. Run 16's shape - its last cut teardown at 09:02:41.384 while the door kept
        // advancing to 21,427 calls at the 09:04:49 totals, `ContextExtension::SetTimeAll` pinned at the cut's
        // own 4.000 s (`LEDGER.md:2726`, `:2743-2744`, `:2739`, as printed 2026-10-12) - is a runtime being
        // stepped with no open cut run to read it from, so it is the one input of the three that sees it. The
        // gate answers closed and this one does not.
        let mut sim = Sim::new(0, 1);
        sim.cut_in_running = true;

        for _ in 0..20 {
            sim.apply(true, 2, true);
        }

        assert!(sim.story_writes.is_empty(), "a stepped cut-in runtime did not hold the write");

        sim.cut_in_running = false;   // the door stopped advancing
        sim.apply(true, 2, true);
        assert_eq!(sim.story_writes, vec![2], "the write stayed held after the runtime stopped");
    }

    #[test]
    fn plan_pass_answers_the_three_inputs_as_its_own_kind_of_pass() {
        assert_eq!(plan_pass(true, false, true, true, true), Pass::Hold);
        assert_eq!(plan_pass(false, true, true, true, true), Pass::Hold);
        assert_eq!(plan_pass(false, false, true, true, true), Pass::Idle, "nothing owed is not a refusal");
        assert_eq!(plan_pass(true, false, false, false, false), Pass::Raise);
        assert_eq!(plan_pass(false, true, false, false, false), Pass::Restore);
        assert_eq!(plan_pass(false, false, false, false, false), Pass::Idle);
    }

    #[test]
    fn a_gate_that_can_never_engage_does_not_switch_the_option_off() {
        // The 0-of-3 shape C57 is about: `arm_cut_gate` armed no door that can open a hold, so the gate
        // answers closed for the whole session and its refusal half is unreachable. The two proxies are
        // quiet too here - no career screen, and `StoryEventProbe` installs nothing without `debug_mode` -
        // so this is a build with no reason to hold anything, and the writes run rather than the option
        // being switched off in silence. The build says what the gate lost at install, in
        // `TrainingCuttProbe::cut_gate_missing_line`, at warn level.
        let mut sim = Sim::new(0, 1);

        for _ in 0..20 {
            sim.apply(true, 2, true);   // the option on through twenty view changes with no gate armed
        }

        assert_eq!(sim.story_writes, vec![2], "a gate that cannot engage stopped the option working at all");
        assert_eq!(sim.training_writes, vec![2]);

        sim.apply(false, 2, true);
        assert_eq!(sim.story_writes, vec![2, 0], "the owed restore was held behind a gate that does not exist");
        assert_eq!(sim.training_writes, vec![2, 1]);
    }

    /// One logged pass of the setting half inside run 57's 20:45:18 to 20:45:40 window: the stamp, what the
    /// build that ran the pass did, and the three inputs the guard reads. `wrote_in_the_run` is read off
    /// `run log/hachimi-run57.log`, not derived, so the test checks a decision against an observation.
    struct Run57Pass {
        at: &'static str,
        /// What the run printed, and the line to read it at.
        printed: &'static str,
        career_screen: bool,
        cut_in_running: bool,
        cut_open: bool,
        wrote_in_the_run: bool,
    }

    const RUN_57_WINDOW: [Run57Pass; 3] = [
        Run57Pass {
            at: "20:45:18.071",
            printed: "`story high speed 0 -> 2` and `training high speed 0 -> 2` landed (:1812-1813), 55.6 ms \
                      before `CleanUpCutt() call 2` (:1815)",
            // HEAD's build wrote, which is the evidence its condition was false here. The cut-in answer is
            // independent of that: the door read 230 at the totals at 20:44:33 (:1730) and 230 again at 20:45:25
            // (:1840), so its last crossing was at least 44 s before this pass and nothing stepped a cut-in
            // while it ran. The gate had closed at 20:44:28.080 (:1720) and `PlayTrainingCut` stayed at 1 to the
            // last totals of the run (:1872, :3707).
            career_screen: false,
            cut_in_running: false,
            cut_open: false,
            wrote_in_the_run: true,
        },
        Run57Pass {
            at: "20:45:18.812",
            printed: "the pass read `max 2, saved story 2, loader story 1, training 2` (:1819) - a raise that \
                      touched nothing, already at the ceiling",
            // The read line is printed by `apply_raise`, below the refusal, so printing it is the evidence
            // the pass was not held. `next_view_id = 1200` was requested 82.9 ms later (:1820), and
            // `CleanUpCutt() call 2` had crossed 684.8 ms earlier (:1815) with the gate closed.
            career_screen: false,
            cut_in_running: false,
            cut_open: false,
            wrote_in_the_run: true,
        },
        Run57Pass {
            at: "20:45:29.864",
            printed: "`HighSpeedSetting: left the story and training settings alone, a career screen was up or \
                      a cut in runtime was still being stepped` (:1854)",
            // The cut-in half cannot be what refused here: the door read 230 at the totals at 20:44:33 (:1730),
            // 230 again at 20:45:25 (:1840) and 230 again at 20:45:45 (:1884), so its last crossing was 56.6 s
            // before this pass and the stamp was cold. What was up is the view the guard reads: 1200,
            // `SingleModePaddock` (`SceneDefine.rs:27`, one of `TrainingCuttProbe::VIEW_TRAINING` at `:657`),
            // requested at 20:45:18.894 (:1820), named by the two cut-in closes at 20:45:25.846 and 20:45:26.514
            // (:1844, :1850), and still current 71 ms later when `next_view_id = 1101` was requested (:1855).
            // The gate had closed 61 s earlier (:1720) and no `PlayTrainingCut` opened it since (:1872).
            career_screen: true,
            cut_in_running: false,
            cut_open: false,
            wrote_in_the_run: false,
        },
    ];

    #[test]
    fn run_57s_window_from_20_45_18_to_20_45_40_hands_what_head_handed_and_holds_what_head_held() {
        for pass in RUN_57_WINDOW.iter() {
            // First: HEAD's condition, spelled in `head_held`, reproduces what the build that ran this pass
            // actually did. A row that fails this is a row whose inputs were read wrong.
            assert_eq!(
                head_held(pass.career_screen, pass.cut_in_running),
                !pass.wrote_in_the_run,
                "HEAD's condition does not reproduce {} ({}): head_held {}, the run wrote {}",
                pass.at, pass.printed, head_held(pass.career_screen, pass.cut_in_running), pass.wrote_in_the_run,
            );

            // Then the superset: whatever HEAD held, the shipped decision holds too, whatever the gate says.
            if head_held(pass.career_screen, pass.cut_in_running) {
                assert_eq!(
                    plan_pass(true, true, pass.career_screen, pass.cut_in_running, pass.cut_open),
                    Pass::Hold,
                    "the shipped guard handed {} ({}), which HEAD held", pass.at, pass.printed,
                );
            }
            else {
                assert_ne!(
                    plan_pass(true, true, pass.career_screen, pass.cut_in_running, pass.cut_open),
                    Pass::Hold,
                    "the shipped guard holds {} ({}), which HEAD handed - the union added a fourth reason", pass.at, pass.printed,
                );
            }
        }

        // Which passes in the window the gate alone reaches: none. Its one open stretch in the whole run is
        // 20:44:19.183 to 20:44:28.080 (`:1604-1605`, `:1720`), 8.9 s, and this window starts 49.99 s after that
        // close with `PlayTrainingCut` still at 1 (`:1872`, `:3707`). So the pass HEAD held at 20:45:29.864 is
        // exactly the pass the boundary-only guard hands, and the career screen half is what closes it again.
        // Counted rather than stated: over the passes this run logged in the window the union's `Hold` set is
        // HEAD's `Hold` set, so the gate half adds no refusal on run 57's data, which is the honest companion of
        // `the_guard_holds_nothing_beyond_head_held_or_an_open_cut` adding three refusals to HEAD's 18 over the
        // 24 input shapes in general.
        assert!(RUN_57_WINDOW.iter().all(|pass| !pass.cut_open), "run 57's window is not gate-closed throughout");

        let head_holds = RUN_57_WINDOW.iter().filter(|pass| head_held(pass.career_screen, pass.cut_in_running)).count();
        let union_holds = RUN_57_WINDOW.iter()
            .filter(|pass| plan_pass(true, true, pass.career_screen, pass.cut_in_running, pass.cut_open) == Pass::Hold)
            .count();

        assert_eq!(head_holds, 1, "run 57's window has {} pass(es) HEAD held, and `:1854` names one", head_holds);
        assert_eq!(union_holds, head_holds, "the gate refused {} pass(es) run 57 never showed", union_holds - head_holds);

        let refused = &RUN_57_WINDOW[2];
        assert!(!refused.cut_open, "the 20:45:29.864 refusal is inside an open cut gate");
        assert!(head_held(refused.career_screen, refused.cut_in_running), "the 20:45:29.864 pass is not one HEAD held");
        assert_eq!(plan_pass(true, true, refused.career_screen, false, false), Pass::Hold);

        // And the reproduction the change set left standing, kept as its own decision so the suite carries
        // the bug it is fixing: `529e4f5` plus the change set refused on the gate alone. Over this window
        // that shape holds none of the three passes, including the one `:1854` says the run held.
        let gate_only_holds = |cut_open: bool| cut_open;

        assert!(!gate_only_holds(refused.cut_open), "the boundary-only shape is not the one that hands the 20:45:29.864 pass");
        assert!(!RUN_57_WINDOW.iter().any(|pass| gate_only_holds(pass.cut_open)), "the boundary-only shape held no pass in run 57's window");

        // And the honest half: the raise at 20:45:18.071 is handed by HEAD, by the boundary-only build, and
        // by the union. It lands 55.6 ms before a `CleanUpCutt` (`:1815`) and 49.99 s after the last cut this
        // gate ever opened (`:1720`), and no door pair reads a close as evidence of a cut that was being
        // stepped. Run 16's raise at 09:00:22 and its restore at 09:03:24.9 sit the same way, 3.5 s before the
        // cut that never closed (`LEDGER.md:2756`, `:2759`, `:2760`). C57 is not closed by this.
        let landed = &RUN_57_WINDOW[0];
        assert_eq!(plan_pass(true, false, landed.career_screen, landed.cut_in_running, landed.cut_open), Pass::Raise);
        assert_eq!(plan_pass(false, true, landed.career_screen, landed.cut_in_running, landed.cut_open), Pass::Restore);
    }

    #[test]
    fn run_57s_window_drives_the_shipped_writes_the_same_way_the_log_did() {
        // The same three passes through the sim the shipped `plan_pass` decides, so a refusal is a write that
        // did not happen and not only an enum. The values are the ones the run printed.
        //
        // 20:45:18.071 read `saved story 0, loader story 1, training 0` (`:1811`): the game had put its own
        // settings back down, a raise was owed, no input held it, and both writes landed (`:1812-1813`).
        let mut landed = Sim::new(0, 1);
        landed.apply(true, 2, true);
        assert_eq!(landed.story_writes, vec![2]);
        assert_eq!(landed.training_writes, vec![2]);

        // 20:45:18.812 read `saved story 2, loader story 1, training 2` (`:1819`): the same pass, already at
        // the ceiling, touching nothing.
        let mut at_ceiling = Sim::new(2, 2);
        at_ceiling.apply(true, 2, true);
        assert!(at_ceiling.story_writes.is_empty(), "the ceiling was not where the run read it: {:?}", at_ceiling.story_writes);
        assert!(at_ceiling.training_writes.is_empty());

        // 20:45:29.864 (`:1854`): a write owed, view 1200 up, the gate closed since 20:44:28.080. The pass is
        // held, the owed write stays owed, and it lands when the screen moves.
        let mut held = Sim::new(1, 1);
        held.apply(true, 2, true);   // an earlier raise on a quiet screen, the baseline on record
        held.career_screen = true;

        for _ in 0..20 {
            held.apply(false, 2, true);   // the option off while the career screen stays up
        }

        assert_eq!(held.story_writes, vec![2], "the restore ran through the career screen the run refused it on");
        assert!(held.raised, "a held restore spent the owed write");

        held.career_screen = false;
        held.apply(false, 2, true);
        assert_eq!(held.story_writes, vec![2, 1], "the deferred restore never landed");
        assert_eq!(held.saved, 1);
        assert_eq!(held.training, 1);
    }

    #[test]
    fn a_hidden_high_speed_settings_option_plans_nothing_whatever_the_config_says() {
        // Hidden for now: the saved story and training values stay where the game has them, and no pass
        // raises or restores them, with the config key on or off.
        for config_value in [true, false] {
            let enabled = effective_enabled(config_value);

            assert!(!enabled, "a hidden option read as on for config value {config_value}");
            assert_eq!(plan_pass(enabled, false, false, false, false), Pass::Idle);
            assert_eq!(plan_pass(enabled, false, true, true, true), Pass::Idle, "a hidden option held a write it was never owed");
        }
    }
}
