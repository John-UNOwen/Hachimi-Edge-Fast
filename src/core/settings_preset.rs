//! Named settings, so that measuring this fork's speed work stops being a manual chore.
//!
//! Item 55 wants the fork compared against plain upstream Hachimi on the same content. No run in
//! the ledger can answer it because every run has the speed factors raised, and hand editing twelve
//! sliders between runs is how a comparison ends up comparing the wrong pairs. A preset is the
//! whole `Config` under a name, so switching arms is one click and a run says which arm it was on.
//!
//! Three arms ship built in and are derived from the settings the game is live on, never stored, so
//! picking one cannot reset translation or HUD work that has nothing to do with speed:
//!
//! - `Neutral`: every timing field inert, which is the arm where this fork writes nothing on a
//!   timing path.
//! - `All levers`: every timing field at the ceiling its own code honours.
//! - `Hachimi fast`: the options upstream Hachimi itself offers, at the values the user names as
//!   upstream's fastest, with this fork's own group factors left at their shipped 1.0. That is the
//!   arm closest to plain upstream this build can reach without building upstream.
//!
//! A preset a player saves is a full snapshot, so restoring one restores graphics and translation
//! too. Presets are never pre-clamped: every value reaches `AnimationSpeed::normalize`,
//! `normalize_time_scale` and `normalize_ui_animation_scale` on the way into the game, which is the
//! same road a slider value takes, and is what keeps C5 from coming back through a hand edited
//! `config.json`.
//!
//! A switch also opens a measurement window. Run 29 settled that a game session cannot be held
//! constant, because which cut the game plays and how long the server takes are random, so the fork
//! stops trying to compare whole sessions and instead records which arm every sample ran under: the
//! launch names window 1, every switch opens the next one, and a hook reads nothing but an atomic
//! number. A report can then say what one arm did to the cuts it actually saw, split by what kind of
//! cut each one was.

use std::sync::atomic::{self, AtomicUsize};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::il2cpp::hook::umamusume::AnimationSpeed;
use super::hachimi::{recover_lock, Config};

pub const PRESET_NEUTRAL: &str = "Neutral";
pub const PRESET_ALL_LEVERS: &str = "All levers";
pub const PRESET_HACHIMI_FAST: &str = "Hachimi fast";

/// How many arm windows one session may name. A player clicking the picker back and forth cannot
/// grow a table without bound: past the last named window every later switch folds into one window
/// reserved for it.
pub const WINDOW_NAME_LIMIT: usize = 24;

/// The window every switch past `WINDOW_NAME_LIMIT - 2` lands in. It is reserved from the first
/// switch so a window that already has a name is never relabelled by a later click.
pub const WINDOW_FOLDED_NAME: &str = "arms past the named limit";

/// The name a sample gets when it was taken before the run had named the arm it started on.
pub const WINDOW_ANON_NAME: &str = "arm unnamed";

/// The window book as plain data, so the folding rule is testable without a game, a config or a
/// shared static. The live copy below keeps the window number in an atomic, which is the only thing
/// a hook is allowed to read.
#[derive(Default)]
pub struct ArmWindows {
    epoch: usize,
    names: Vec<String>,
}

impl ArmWindows {
    /// Opens the window for the arm the game is now on and returns its number.
    pub fn open(&mut self, name: &str) -> usize {
        let last_named = WINDOW_NAME_LIMIT - 2;

        if self.epoch < last_named {
            self.epoch += 1;

            while self.names.len() <= self.epoch {
                self.names.push(String::new());
            }

            self.names[self.epoch] = name.to_string();
        }
        else {
            self.epoch = WINDOW_NAME_LIMIT - 1;
        }

        self.epoch
    }

    pub fn epoch(&self) -> usize {
        self.epoch
    }

    pub fn name(&self, epoch: usize) -> &str {
        if epoch == 0 {
            return WINDOW_ANON_NAME;
        }

        if epoch == WINDOW_NAME_LIMIT - 1 {
            return WINDOW_FOLDED_NAME;
        }

        match self.names.get(epoch) {
            Some(name) if !name.is_empty() => name.as_str(),
            _ => WINDOW_ANON_NAME,
        }
    }
}

static ARM_WINDOWS: Lazy<Mutex<ArmWindows>> = Lazy::new(|| Mutex::new(ArmWindows::default()));
static ARM_WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Opens a window for the arm the game is now running on: once when a run names the arm it launched
/// with, and once every time the picker changes it.
pub fn open_arm_window(name: &str) {
    let mut book = recover_lock(&ARM_WINDOWS);
    let epoch = book.open(name);

    drop(book);
    ARM_WINDOW.store(epoch, atomic::Ordering::Relaxed);
}

/// The window a measurement belongs to. This is the only arm call a hook may make: a relaxed load,
/// no lock and no allocation.
pub fn arm_window() -> usize {
    ARM_WINDOW.load(atomic::Ordering::Relaxed)
}

/// Resolves a window number to its name. Cold path only: a report line, never a door.
pub fn arm_window_name(epoch: usize) -> String {
    recover_lock(&ARM_WINDOWS).name(epoch).to_string()
}

/// The whole `Config` under a name. `settings_presets` is cleared in the snapshot this struct holds,
/// so saving a preset cannot nest a copy of the preset list inside itself.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsPreset {
    pub name: String,
    pub config: Config,
}

/// The timing fields an arm is allowed to set. Everything outside this list is carried over from the
/// settings the game is live on, because a preset that also moved translation or HUD would be
/// changing two variables at once.
fn with_timing(mut config: Config, timing: impl FnOnce(&mut Config)) -> Config {
    timing(&mut config);
    config
}

/// Sets the fields whose whole job is timing. `target_fps` and `physics_update_mode` are left alone
/// on purpose: a frame cap and a physics mode are second variables, and an arm that moved them would
/// not be measuring the levers.
fn set_timing(
    config: &mut Config,
    transition: f32,
    result: f32,
    story: f32,
    ui_animation: f32,
    time_scale: f32,
    tcps: f32,
    choice_delay: f32,
    cyspring_uncap: bool,
    auto_skip: bool,
    high_speed: bool,
    story_high_speed: bool,
) {
    config.transition_speed = transition;
    config.result_screen_speed = result;
    config.story_speed = story;
    config.ui_animation_scale = ui_animation;
    config.time_scale = time_scale;
    config.story_tcps_multiplier = tcps;
    config.story_choice_auto_select_delay = choice_delay;
    config.cyspring_mono_uncap_frame_scale = cyspring_uncap;
    config.auto_skip_result_screens = auto_skip;
    config.high_speed_settings = high_speed;
    config.story_high_speed_mode = story_high_speed;
}

impl Default for SettingsPreset {
    fn default() -> Self {
        Self::neutral(&Config::default())
    }
}

impl SettingsPreset {
    /// Every timing field inert. Most of these are the shipped defaults already; two shipped
    /// defaults are raised levers, so this arm names the inert value instead: the story text speed
    /// multiplier ships at 3.0, and a shipped choice delay of 1.2 hands the story auto select
    /// accumulator `CHOICE_AUTO_SELECT_TRIGGER_TIME / 1.2` = 0.625, a *slower* wait than the game's
    /// own. Item 55 wants an arm the fork does nothing on, so both are named inert here.
    pub fn neutral(live: &Config) -> Self {
        Self {
            name: PRESET_NEUTRAL.to_string(),
            config: with_timing(live.clone(), |config| {
                set_timing(
                    config,
                    1.0,
                    1.0,
                    1.0,
                    1.0,
                    1.0,
                    1.0,
                    AnimationSpeed::CHOICE_AUTO_SELECT_TRIGGER_TIME,
                    false,
                    false,
                    false,
                    false,
                )
            }),
        }
    }

    /// Every timing field at the ceiling the code honours: `MAX_FACTOR` for the transition and
    /// result groups, 10.0 for the story group where its slider ends, `MAX_UI_ANIMATION_SCALE`,
    /// `MAX_TIME_SCALE`, the choice delay floor, and the story text multiplier at the 1000.0 its
    /// slider offers, which is deliberately unclamped (StoryTimelineData.rs multiplies
    /// TypewriteCountPerSecond by it).
    pub fn all_levers(live: &Config) -> Self {
        Self {
            name: PRESET_ALL_LEVERS.to_string(),
            config: with_timing(live.clone(), |config| {
                set_timing(
                    config,
                    AnimationSpeed::MAX_FACTOR,
                    AnimationSpeed::MAX_FACTOR,
                    10.0,
                    AnimationSpeed::MAX_UI_ANIMATION_SCALE,
                    AnimationSpeed::MAX_TIME_SCALE,
                    1000.0,
                    AnimationSpeed::MIN_STORY_CHOICE_AUTO_SELECT_DELAY,
                    true,
                    true,
                    true,
                    true,
                )
            }),
        }
    }

    /// Upstream Hachimi's own speed options at the fastest values the user names: UI animation scale
    /// 10, story auto select delay 0.1, story text speed multiplier 10, CySpring mono frame scale
    /// uncapped. This fork's group factors, its `Time.timeScale` lever, auto skip and the two story
    /// high speed flags stay at their shipped values, so this arm is what plain upstream looks like
    /// rather than what this fork can be pushed to.
    pub fn hachimi_fast(live: &Config) -> Self {
        Self {
            name: PRESET_HACHIMI_FAST.to_string(),
            config: with_timing(live.clone(), |config| {
                set_timing(
                    config,
                    1.0,
                    1.0,
                    1.0,
                    10.0,
                    1.0,
                    10.0,
                    0.1,
                    true,
                    false,
                    false,
                    false,
                )
            }),
        }
    }

    pub fn built_ins(live: &Config) -> [Self; 3] {
        [Self::neutral(live), Self::all_levers(live), Self::hachimi_fast(live)]
    }

    pub fn is_builtin(name: &str) -> bool {
        name == PRESET_NEUTRAL || name == PRESET_ALL_LEVERS || name == PRESET_HACHIMI_FAST
    }

    /// A snapshot of what the game is actually running. The stored copy holds no preset list, so a
    /// preset cannot grow the config every time it is saved.
    pub fn capture(name: &str, live: &Config) -> Self {
        let mut config = live.clone();
        config.settings_presets.clear();
        config.settings_preset_name = name.to_string();

        Self { name: name.to_string(), config }
    }

    /// Replace the live config with the stored one, keeping the preset list the player has saved so
    /// switching arms never costs them their own presets. The switch also opens a measurement window,
    /// because a sample the probes take after this point belongs to the arm this switch put in force.
    pub fn apply_to(&self, target: &mut Config) {
        let saved = std::mem::take(&mut target.settings_presets);
        *target = self.config.clone();
        target.settings_presets = saved;
        target.settings_preset_name = self.name.clone();
        open_arm_window(&self.name);
    }

    /// The built-in arms, then the player's own presets. A saved name wins a clash with a built-in.
    pub fn all_presets(live: &Config) -> Vec<Self> {
        let mut presets: Vec<Self> = Self::built_ins(live).into_iter().collect();

        for preset in live.settings_presets.iter() {
            if let Some(existing) = presets.iter().position(|built| built.name == preset.name) {
                presets.remove(existing);
            }
            presets.push(preset.clone());
        }

        presets
    }

    pub fn save(config: &mut Config, preset: &Self) {
        if let Some(existing) = config.settings_presets.iter().position(|saved| saved.name == preset.name) {
            config.settings_presets.remove(existing);
        }
        config.settings_presets.push(preset.clone());
    }

    /// Removes a saved preset. The built-in arms are derived from the live config rather than
    /// stored, so deleting a saved arm that borrowed a built-in name hands the built-in back instead
    /// of costing the player an arm.
    pub fn remove(config: &mut Config, name: &str) -> bool {
        let before = config.settings_presets.len();
        config.settings_presets.retain(|saved| saved.name != name);
        config.settings_presets.len() != before
    }

    /// The fields a run is read against, printed in the same order `Config snapshot:` prints them.
    /// The rest of the preset is carried silently, because the snapshot line already prints the
    /// whole config it was applied from.
    pub fn log_line(&self) -> String {
        let config = &self.config;

        format!(
            "preset {} transition {} result {} story {} ui_animation {} time_scale {} story_tcps {} choice_delay {} cyspring_uncap {} auto_skip {} high_speed {} story_high_speed {}",
            self.name,
            config.transition_speed,
            config.result_screen_speed,
            config.story_speed,
            config.ui_animation_scale,
            config.time_scale,
            config.story_tcps_multiplier,
            config.story_choice_auto_select_delay,
            config.cyspring_mono_uncap_frame_scale,
            config.auto_skip_result_screens,
            config.high_speed_settings,
            config.story_high_speed_mode,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_neutral_arm_is_inert_on_every_timing_field() {
        let live = Config::default();
        let preset = SettingsPreset::neutral(&live);
        let config = &preset.config;

        assert_eq!(preset.name, PRESET_NEUTRAL);
        assert_eq!(config.transition_speed, 1.0);
        assert_eq!(config.result_screen_speed, 1.0);
        assert_eq!(config.story_speed, 1.0);
        assert_eq!(config.ui_animation_scale, 1.0);
        assert_eq!(config.time_scale, 1.0);
        assert!(!config.auto_skip_result_screens);
        assert!(!config.high_speed_settings);
        assert!(!config.story_high_speed_mode);
        assert!(!config.cyspring_mono_uncap_frame_scale);

        assert_eq!(config.story_tcps_multiplier, 1.0, "the shipped text multiplier is 3.0, a raised lever that belongs to another arm");
        assert_eq!(config.story_choice_auto_select_delay, AnimationSpeed::CHOICE_AUTO_SELECT_TRIGGER_TIME);
        assert_eq!(
            AnimationSpeed::story_choice_auto_select_multiplier(live.story_choice_auto_select_delay),
            Some(0.625),
            "the shipped 1.2 delay hands the story auto select a multiplier under 1.0, so copying it would not be inert"
        );
    }

    #[test]
    fn the_all_levers_arm_names_no_more_than_the_code_honours() {
        let config = SettingsPreset::all_levers(&Config::default()).config;

        assert_eq!(config.transition_speed, AnimationSpeed::MAX_FACTOR);
        assert_eq!(config.result_screen_speed, AnimationSpeed::MAX_FACTOR);
        assert_eq!(config.ui_animation_scale, AnimationSpeed::MAX_UI_ANIMATION_SCALE);
        assert_eq!(config.time_scale, AnimationSpeed::MAX_TIME_SCALE);
        assert_eq!(config.story_choice_auto_select_delay, AnimationSpeed::MIN_STORY_CHOICE_AUTO_SELECT_DELAY);
        assert!(config.auto_skip_result_screens);
        assert!(config.high_speed_settings);
        assert!(config.story_high_speed_mode);
        assert!(config.cyspring_mono_uncap_frame_scale);
        assert!(config.story_speed <= AnimationSpeed::MAX_FACTOR, "the story slider ends at 10.0; the arm must not offer past it");
    }

    #[test]
    fn the_hachimi_fast_arm_raises_only_what_upstream_offers() {
        let config = SettingsPreset::hachimi_fast(&Config::default()).config;

        assert_eq!(config.ui_animation_scale, 10.0);
        assert_eq!(config.story_choice_auto_select_delay, 0.1);
        assert_eq!(config.story_tcps_multiplier, 10.0);
        assert!(config.cyspring_mono_uncap_frame_scale);

        assert_eq!(config.transition_speed, 1.0, "this fork's transition group is not an upstream option");
        assert_eq!(config.result_screen_speed, 1.0);
        assert_eq!(config.story_speed, 1.0);
        assert_eq!(config.time_scale, 1.0);
        assert!(!config.auto_skip_result_screens);
        assert!(!config.high_speed_settings);
        assert!(!config.story_high_speed_mode);
    }

    #[test]
    fn a_builtin_arm_moves_no_field_outside_the_timing_set() {
        let live = Config::default();

        for preset in SettingsPreset::built_ins(&live) {
            // Picking an arm must not touch a setting that has nothing to do with timing.
            assert_eq!(preset.config.ipv4_only, live.ipv4_only);
            assert_eq!(preset.config.custom_font_file, live.custom_font_file);
            assert_eq!(preset.config.hide_now_loading, live.hide_now_loading);
            assert_eq!(preset.config.champions_live_year, live.champions_live_year);
            assert_eq!(preset.config.ui_accent_color, live.ui_accent_color);
            assert_eq!(preset.config.target_fps, live.target_fps, "a frame cap is a second variable");
            assert_eq!(preset.config.physics_update_mode, live.physics_update_mode, "a physics mode is a second variable");
        }
    }

    #[test]
    fn a_saved_preset_stores_the_whole_config_without_nesting_the_preset_list() {
        let mut live = Config::default();
        live.settings_preset_name = "My run".to_string();
        let snapshot = SettingsPreset::capture("My run", &live);
        SettingsPreset::save(&mut live, &snapshot);

        let stored = &live.settings_presets[0];
        assert_eq!(stored.name, "My run");
        assert!(stored.config.settings_presets.is_empty(), "a snapshot holding the preset list would grow the config every save");
        assert_eq!(stored.config.settings_preset_name, "My run");
    }

    #[test]
    fn applying_a_preset_restores_the_whole_config_and_keeps_the_saved_presets() {
        let mut live = Config::default();
        live.ipv4_only = true;
        let snapshot = SettingsPreset::capture("With IPv4", &live);
        SettingsPreset::save(&mut live, &snapshot);

        let mut target = Config::default();
        let other = SettingsPreset::capture("Other", &target);
        SettingsPreset::save(&mut target, &other);
        SettingsPreset::all_presets(&live).iter().find(|preset| preset.name == "With IPv4").unwrap().apply_to(&mut target);

        assert!(target.ipv4_only, "a settings preset is expected to restore graphics and network choices too");
        assert_eq!(target.settings_presets.len(), 1);
        assert_eq!(target.settings_presets[0].name, "Other");
        assert_eq!(target.settings_preset_name, "With IPv4");
    }

    #[test]
    fn a_saved_preset_replaces_the_builtin_of_its_name_and_deleting_it_hands_the_builtin_back() {
        let mut live = Config::default();
        live.transition_speed = 7.0;
        let snapshot = SettingsPreset::capture(PRESET_NEUTRAL, &live);
        SettingsPreset::save(&mut live, &snapshot);

        let presets = SettingsPreset::all_presets(&live);
        assert_eq!(presets.len(), 3);
        assert_eq!(presets.iter().filter(|preset| preset.name == PRESET_NEUTRAL).count(), 1);
        assert_eq!(presets.iter().find(|preset| preset.name == PRESET_NEUTRAL).unwrap().config.transition_speed, 7.0,
            "the saved snapshot wins the name, so the picker cannot offer two arms called Neutral");

        assert!(SettingsPreset::remove(&mut live, PRESET_NEUTRAL));
        let presets = SettingsPreset::all_presets(&live);
        assert_eq!(presets.len(), 3, "the built-in arm is derived from the live config, so no arm was lost");
        assert_eq!(presets[0].config.transition_speed, 1.0);
    }

    #[test]
    fn an_arm_switch_opens_the_measurement_window_a_probe_reads() {
        let mut book = ArmWindows::default();

        assert_eq!(book.epoch(), 0);
        assert_eq!(book.name(0), WINDOW_ANON_NAME, "a sample before the launch naming has no arm to blame");

        assert_eq!(book.open(PRESET_NEUTRAL), 1);
        assert_eq!(book.name(1), PRESET_NEUTRAL);
        assert_eq!(book.open(PRESET_ALL_LEVERS), 2);
        assert_eq!(book.name(2), PRESET_ALL_LEVERS);
        assert_eq!(book.epoch(), 2, "the number a door reads is the window the last switch opened");
    }

    #[test]
    fn arm_switches_past_the_named_limit_fold_into_one_reserved_window() {
        let mut book = ArmWindows::default();

        for index in 0..(WINDOW_NAME_LIMIT + 30) {
            book.open(&format!("arm {index}"));
        }

        assert_eq!(book.epoch(), WINDOW_NAME_LIMIT - 1);
        assert_eq!(book.name(WINDOW_NAME_LIMIT - 1), WINDOW_FOLDED_NAME);
        assert_eq!(book.name(1), "arm 0", "a window that already has samples must not be relabelled");
        assert_eq!(book.names.len(), WINDOW_NAME_LIMIT - 1, "the table stops growing at the last named window");
    }

    #[test]
    fn applying_an_arm_moves_the_window_a_probe_can_read() {
        // The window number is one process wide static and cargo runs tests on several threads, so
        // this can only assert that it moved forward, never by how much.
        let before = arm_window();
        let arm = SettingsPreset::all_presets(&Config::default()).into_iter().find(|preset| preset.name == PRESET_ALL_LEVERS).unwrap();

        arm.apply_to(&mut Config::default());

        assert!(arm_window() > before, "a switch that left the window number where it was would file its samples under the arm before it");
    }
}
