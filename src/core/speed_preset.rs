// Named sets of the timing settings, so a paired measurement is one click between turns instead of
// twelve sliders edited twice.
//
// A preset carries only the fields that reach a duration or a game speed setting. An apply never
// touches translation, HUD, graphics or the Config Editor's own state, so switching arms mid session
// moves nothing that is not under test.
//
// The values travel as named. What clamps a slider value on its way into the game clamps a preset
// value the same way (`AnimationSpeed::normalize`, `normalize_time_scale` and
// `normalize_ui_animation_scale` all run on the write), so a preset hand written into config.json
// still cannot reach the game past `MAX_FACTOR` or `MAX_TIME_SCALE`. The story text multiplier is
// deliberately unclamped and travels here like any other field.

use serde::{Deserialize, Serialize};

use crate::il2cpp::hook::umamusume::{AnimationSpeed, CySpringController::SpringUpdateMode};

use super::hachimi::Config;

/// The arm where this fork writes nothing on a timing path. Every field is the value `Config` ships
/// with, which is what "the option does nothing at its default" means for a player who never opened
/// the Config Editor.
pub const PRESET_NEUTRAL: &str = "Neutral";

/// The other arm: every lever this fork offers, each at the ceiling its own slider offers. The
/// story group stops at 10.0 in the Config Editor while the transition and result groups go to
/// `MAX_FACTOR`, so the two numbers are not the same on purpose.
pub const PRESET_ALL_LEVERS: &str = "All levers";

/// One arm of a timing measurement. `target_fps` is in the set because a frame cap changes what a
/// session costs, and both built-in arms leave it at the shipped value so a pair compares levers
/// rather than frame rates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpeedPreset {
    pub name: String,
    pub transition_speed: f32,
    pub result_screen_speed: f32,
    pub story_speed: f32,
    pub ui_animation_scale: f32,
    pub time_scale: f32,
    pub story_tcps_multiplier: f32,
    pub story_choice_auto_select_delay: f32,
    pub target_fps: Option<i32>,
    pub auto_skip_result_screens: bool,
    pub high_speed_settings: bool,
    pub story_high_speed_mode: bool,
    pub physics_update_mode: Option<SpringUpdateMode>,
}

impl SpeedPreset {
    /// The arm where this fork writes nothing on a timing path. Most fields are the shipped defaults
    /// because those defaults are already inert. Two are not: the story text speed multiplier ships at
    /// 3.0, and a shipped choice delay of 1.2 hands the story auto select accumulator
    /// `CHOICE_AUTO_SELECT_TRIGGER_TIME / 1.2` = 0.625, which is a *slower* wait than the game's own.
    /// This arm names 1.0 and the trigger time itself, the one delay where the multiplier is 1.0.
    pub fn neutral() -> Self {
        let shipped = Config::default();

        Self {
            name: PRESET_NEUTRAL.to_string(),
            transition_speed: shipped.transition_speed,
            result_screen_speed: shipped.result_screen_speed,
            story_speed: shipped.story_speed,
            ui_animation_scale: shipped.ui_animation_scale,
            time_scale: shipped.time_scale,
            story_tcps_multiplier: 1.0,
            story_choice_auto_select_delay: AnimationSpeed::CHOICE_AUTO_SELECT_TRIGGER_TIME,
            target_fps: shipped.target_fps,
            auto_skip_result_screens: shipped.auto_skip_result_screens,
            high_speed_settings: shipped.high_speed_settings,
            story_high_speed_mode: shipped.story_high_speed_mode,
            physics_update_mode: shipped.physics_update_mode,
        }
    }

    /// The arm with every lever this fork offers raised to the ceiling the code honours, with the
    /// game's own physics update mode and frame cap left alone so those stay out of the pair.
    pub fn all_levers() -> Self {
        Self {
            name: PRESET_ALL_LEVERS.to_string(),
            transition_speed: AnimationSpeed::MAX_FACTOR,
            result_screen_speed: AnimationSpeed::MAX_FACTOR,
            // The story group's slider ends at 10.0 and `story_choice_auto_select_delay` at its floor,
            // which is where the choice sites multiply by MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER.
            story_speed: 10.0,
            ui_animation_scale: AnimationSpeed::MAX_UI_ANIMATION_SCALE,
            time_scale: AnimationSpeed::MAX_TIME_SCALE,
            // The top of the story text speed slider, which is deliberately unclamped.
            story_tcps_multiplier: 1000.0,
            story_choice_auto_select_delay: AnimationSpeed::MIN_STORY_CHOICE_AUTO_SELECT_DELAY,
            target_fps: Config::default().target_fps,
            auto_skip_result_screens: true,
            high_speed_settings: true,
            story_high_speed_mode: true,
            physics_update_mode: None,
        }
    }

    /// The two arms this fork ships. A saved preset of the same name replaces a built-in in
    /// `all_presets`, so a player can keep the name and change what it means.
    pub fn built_ins() -> [Self; 2] {
        [Self::neutral(), Self::all_levers()]
    }

    /// An arm taken from what the game is running right now, which is how a player captures the
    /// settings they actually play with.
    pub fn capture(name: &str, config: &Config) -> Self {
        Self {
            name: name.to_string(),
            transition_speed: config.transition_speed,
            result_screen_speed: config.result_screen_speed,
            story_speed: config.story_speed,
            ui_animation_scale: config.ui_animation_scale,
            time_scale: config.time_scale,
            story_tcps_multiplier: config.story_tcps_multiplier,
            story_choice_auto_select_delay: config.story_choice_auto_select_delay,
            target_fps: config.target_fps,
            auto_skip_result_screens: config.auto_skip_result_screens,
            high_speed_settings: config.high_speed_settings,
            story_high_speed_mode: config.story_high_speed_mode,
            physics_update_mode: config.physics_update_mode,
        }
    }

    /// The fields a preset owns, logged in the order `Config snapshot:` prints them so a run can
    /// read an arm switch off the log next to the settings it produced.
    pub fn log_line(&self) -> String {
        format!(
            "preset {} transition {} result {} story {} ui_animation {} time_scale {} story_tcps {} \
             choice_delay {} target_fps {:?} auto_skip_result {} high_speed_settings {} \
             story_high_speed {} physics {:?}",
            self.name,
            self.transition_speed,
            self.result_screen_speed,
            self.story_speed,
            self.ui_animation_scale,
            self.time_scale,
            self.story_tcps_multiplier,
            self.story_choice_auto_select_delay,
            self.target_fps,
            self.auto_skip_result_screens,
            self.high_speed_settings,
            self.story_high_speed_mode,
            self.physics_update_mode,
        )
    }

    /// Write the arm into a config. The name goes in too, so the next `Config snapshot:` line says
    /// which arm the run was on.
    pub fn apply_to(&self, config: &mut Config) {
        config.speed_preset_name = self.name.clone();
        config.transition_speed = self.transition_speed;
        config.result_screen_speed = self.result_screen_speed;
        config.story_speed = self.story_speed;
        config.ui_animation_scale = self.ui_animation_scale;
        config.time_scale = self.time_scale;
        config.story_tcps_multiplier = self.story_tcps_multiplier;
        config.story_choice_auto_select_delay = self.story_choice_auto_select_delay;
        config.target_fps = self.target_fps;
        config.auto_skip_result_screens = self.auto_skip_result_screens;
        config.high_speed_settings = self.high_speed_settings;
        config.story_high_speed_mode = self.story_high_speed_mode;
        config.physics_update_mode = self.physics_update_mode;
    }

    /// The built-ins first, then the saved ones, with a saved preset winning a name clash so one
    /// picker never shows the same name twice.
    pub fn all_presets(config: &Config) -> Vec<Self> {
        let mut presets = Self::built_ins().to_vec();

        for saved in config.speed_presets.iter() {
            match presets.iter_mut().find(|preset| preset.name == saved.name) {
                Some(slot) => *slot = saved.clone(),
                None => presets.push(saved.clone()),
            }
        }

        presets
    }

    /// Store an arm under its name, replacing one already saved under it.
    pub fn save(config: &mut Config, preset: &Self) {
        match config.speed_presets.iter_mut().find(|saved| saved.name == preset.name) {
            Some(slot) => *slot = preset.clone(),
            None => config.speed_presets.push(preset.clone()),
        }
    }

    /// Drop a saved arm. A built-in name is refused: the picker always shows those two, and a
    /// delete button that silently did nothing to them would be a lie about what it removed.
    pub fn remove(config: &mut Config, name: &str) -> bool {
        if name == PRESET_NEUTRAL || name == PRESET_ALL_LEVERS {
            return false;
        }

        let before = config.speed_presets.len();
        config.speed_presets.retain(|saved| saved.name != name);

        config.speed_presets.len() != before
    }
}

impl Default for SpeedPreset {
    fn default() -> Self {
        Self::neutral()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_neutral_arm_is_the_shipped_defaults_except_where_a_shipped_default_is_already_raised() {
        let shipped = Config::default();
        let preset = SpeedPreset::neutral();

        assert_eq!(preset.name, PRESET_NEUTRAL);
        assert_eq!(preset.transition_speed, shipped.transition_speed);
        assert_eq!(preset.result_screen_speed, shipped.result_screen_speed);
        assert_eq!(preset.story_speed, shipped.story_speed);
        assert_eq!(preset.ui_animation_scale, shipped.ui_animation_scale);
        assert_eq!(preset.time_scale, shipped.time_scale);
        assert_eq!(preset.target_fps, shipped.target_fps);
        assert_eq!(preset.auto_skip_result_screens, shipped.auto_skip_result_screens);
        assert_eq!(preset.high_speed_settings, shipped.high_speed_settings);
        assert_eq!(preset.story_high_speed_mode, shipped.story_high_speed_mode);
        assert_eq!(preset.physics_update_mode, shipped.physics_update_mode);

        assert_eq!(preset.story_tcps_multiplier, 1.0, "the shipped text multiplier is 3.0, which is a raised lever and belongs to the other arm");
        assert_eq!(preset.story_choice_auto_select_delay, AnimationSpeed::CHOICE_AUTO_SELECT_TRIGGER_TIME);
        assert_eq!(AnimationSpeed::story_choice_auto_select_multiplier(shipped.story_choice_auto_select_delay), Some(0.625),
            "the shipped 1.2 delay hands the story auto select a multiplier under 1.0, which is why the neutral arm cannot simply copy it");
    }

    #[test]
    fn applying_the_neutral_preset_leaves_every_lever_where_the_code_is_inert() {
        let mut config = Config::default();
        config.transition_speed = 20.0;
        config.result_screen_speed = 20.0;
        config.story_speed = 10.0;
        config.ui_animation_scale = 20.0;
        config.time_scale = 5.0;
        config.story_choice_auto_select_delay = 0.1;
        config.auto_skip_result_screens = true;
        config.high_speed_settings = true;
        config.story_high_speed_mode = true;

        SpeedPreset::neutral().apply_to(&mut config);

        assert_eq!(config.transition_speed, 1.0);
        assert_eq!(config.result_screen_speed, 1.0);
        assert_eq!(config.story_speed, 1.0);
        assert_eq!(config.ui_animation_scale, 1.0);
        assert_eq!(config.time_scale, 1.0);
        assert_eq!(config.story_tcps_multiplier, 1.0, "the text multiplier multiplies TypewriteCountPerSecond, so only 1.0 leaves it alone");
        assert_eq!(config.story_choice_auto_select_delay, 0.75);
        assert_eq!(AnimationSpeed::story_choice_auto_select_multiplier(config.story_choice_auto_select_delay), Some(1.0),
            "both story choice sites read this multiplier, so 1.0 is the setting where neither writes");
        assert!(!config.auto_skip_result_screens);
        assert!(!config.high_speed_settings);
        assert!(!config.story_high_speed_mode);
    }

    #[test]
    fn the_all_levers_preset_names_no_more_than_the_code_honours() {
        let preset = SpeedPreset::all_levers();

        assert!(preset.transition_speed <= AnimationSpeed::MAX_FACTOR);
        assert!(preset.result_screen_speed <= AnimationSpeed::MAX_FACTOR);
        assert!(preset.ui_animation_scale <= AnimationSpeed::MAX_UI_ANIMATION_SCALE);
        assert!(preset.time_scale <= AnimationSpeed::MAX_TIME_SCALE);
        assert!(preset.story_speed <= 10.0, "the story group slider ends at 10.0 and the preset does too");
        assert!(preset.story_choice_auto_select_delay >= AnimationSpeed::MIN_STORY_CHOICE_AUTO_SELECT_DELAY);
        assert_eq!(preset.target_fps, None, "a frame cap is a second variable, so neither built-in arm sets one");
        assert_eq!(preset.physics_update_mode, None, "the game's own physics update mode stays out of the pair");
    }

    #[test]
    fn a_preset_apply_touches_no_field_outside_the_timing_set() {
        let mut config = Config::default();
        config.debug_mode = true;
        config.enable_file_logging = true;
        config.auto_skip_result_screens = true;

        SpeedPreset::all_levers().apply_to(&mut config);

        assert!(config.debug_mode, "a speed arm is not a debug arm");
        assert!(config.enable_file_logging, "a speed arm is not a logging arm");
        assert_eq!(config.speed_preset_name, PRESET_ALL_LEVERS);
    }

    #[test]
    fn a_saved_preset_replaces_the_built_in_of_its_name_and_a_delete_cannot_lose_a_built_in() {
        let mut config = Config::default();

        SpeedPreset::save(&mut config, &SpeedPreset { name: PRESET_NEUTRAL.to_string(), story_speed: 4.0, ..SpeedPreset::neutral() });

        let presets = SpeedPreset::all_presets(&config);
        assert_eq!(presets.len(), 2, "one picker, one entry per name");
        assert_eq!(presets.iter().find(|p| p.name == PRESET_NEUTRAL).unwrap().story_speed, 4.0);

        assert!(!SpeedPreset::remove(&mut config, PRESET_NEUTRAL), "a built-in name is not a saved preset");
        assert_eq!(config.speed_presets.len(), 1);
    }

    #[test]
    fn a_capture_records_what_the_game_is_actually_running() {
        let mut config = Config::default();
        config.transition_speed = 7.5;
        config.target_fps = Some(60);
        config.physics_update_mode = SpringUpdateMode::Mode60FPS.into();

        let preset = SpeedPreset::capture("My run", &config);

        assert_eq!(preset.name, "My run");
        assert_eq!(preset.transition_speed, 7.5);
        assert_eq!(preset.target_fps, Some(60));
        assert_eq!(preset.physics_update_mode, config.physics_update_mode);
    }
}
