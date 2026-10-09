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

// Runs on the game main thread wherever the speed groups are applied, so a config change taken
// from the Config Editor lands at the next scene change. The writes are idempotent: the current
// value is read first and only a lower value is raised, and what the game held before the first
// write is put back once when the option is turned off.
pub fn apply() {
    // Charged to the pass that runs this: `AnimationSpeed::apply` reaches this module, this
    // module reads the config, and a pass total that leaves that read out is the half of the
    // cost C36 never measured.
    AnimationSpeed::note_config_read();

    let enabled = Hachimi::instance().config.load().high_speed_settings;

    match plan_pass(enabled, WAS_RAISED.load(atomic::Ordering::Acquire)) {
        Pass::Idle => return,
        // Run 16 is why this pass waits. Its raise landed at 09:00:22 and its restore at 09:03:24,
        // both while a career screen was up, and the training cut runtime in that run was still
        // being stepped every frame when the log ended, with this module's own numbers changing
        // across that window. The settings live on the save loader and in StoryManager's saved
        // setting, so a write taken at a menu or a story screen is the one the next turn reads, and
        // a write taken inside a turn is the one run 16 measured. The pass runs again on the next
        // game tick, so waiting for the screen to move costs a later write rather than losing one.
        Pass::Raise | Pass::Restore if TrainingCuttProbe::on_a_career_screen() || StoryEventProbe::cut_in_engine_driving() => {
            warn_cut_hold();
            return;
        }
        Pass::Raise => apply_raise(),
        Pass::Restore => apply_restore_with_loader(),
    }
}

// Its own slot, so holding a write back does not spend the line that says the save loader is not
// ready yet. It speaks once per session because the pass runs every few seconds.
fn warn_cut_hold() {
    if !CUT_HOLD_WARNED.swap(true, atomic::Ordering::AcqRel) {
        warn!("HighSpeedSetting: left the story and training settings alone, a career screen was up or a cut in runtime was still being stepped");
    }
}

// Which pass a call is, kept free of il2cpp so the sequence is testable the way Time.rs's
// plan_write is. Off with nothing ever written touches nothing; off with something written is
// the restore; on is the raise.
#[derive(Clone, Copy, PartialEq)]
enum Pass {
    Idle,
    Raise,
    Restore,
}

fn plan_pass(enabled: bool, raised: bool) -> Pass {
    if enabled {
        Pass::Raise
    } else if raised {
        Pass::Restore
    } else {
        Pass::Idle
    }
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
// (AnimationSpeed.rs :136-141): the last value the game itself left in that setting,
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
                story_writes: Vec::new(),
                training_writes: Vec::new(),
            }
        }

        fn apply(&mut self, enabled: bool, max: i32, getters_resolved: bool) {
            match plan_pass(enabled, self.raised) {
                Pass::Idle => {}
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
}
