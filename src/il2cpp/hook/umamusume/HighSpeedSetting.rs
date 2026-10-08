use crate::{
    core::Hachimi,
    il2cpp::{hook::umamusume::SaveDataManager, symbols::get_method_addr, types::*}
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
static mut GET_MAX_HIGH_SPEED_TYPE_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetMaxHighSpeedType, GET_MAX_HIGH_SPEED_TYPE_ADDR, i32,);

static mut GET_SAVED_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetSavedHighSpeedSetting, GET_SAVED_HIGH_SPEED_ADDR, i32,);

static mut SAVE_HIGH_SPEED_TYPE_ADDR: usize = 0;
impl_addr_wrapper_fn!(SaveHighSpeedType, SAVE_HIGH_SPEED_TYPE_ADDR, (), value: i32);

static mut GET_STORY_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_StoryHighSpeedType, GET_STORY_HIGH_SPEED_ADDR, i32, this: *mut Il2CppObject);

static mut SET_STORY_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(set_StoryHighSpeedType, SET_STORY_HIGH_SPEED_ADDR, (), this: *mut Il2CppObject, value: i32);

static mut GET_TRAINING_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_TrainingHighSpeedType, GET_TRAINING_HIGH_SPEED_ADDR, i32, this: *mut Il2CppObject);

static mut SET_TRAINING_HIGH_SPEED_ADDR: usize = 0;
impl_addr_wrapper_fn!(set_TrainingHighSpeedType, SET_TRAINING_HIGH_SPEED_ADDR, (), this: *mut Il2CppObject, value: i32);

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryManager);
    get_class_or_return!(umamusume, Gallop, ApplicationSettingSaveLoader);

    unsafe {
        GET_MAX_HIGH_SPEED_TYPE_ADDR = get_method_addr(StoryManager, c"GetMaxHighSpeedType", 0);
        GET_SAVED_HIGH_SPEED_ADDR = get_method_addr(StoryManager, c"GetSavedHighSpeedSetting", 0);
        SAVE_HIGH_SPEED_TYPE_ADDR = get_method_addr(StoryManager, c"SaveHighSpeedType", 1);

        GET_STORY_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"get_StoryHighSpeedType", 0);
        SET_STORY_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"set_StoryHighSpeedType", 1);
        GET_TRAINING_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"get_TrainingHighSpeedType", 0);
        SET_TRAINING_HIGH_SPEED_ADDR = get_method_addr(ApplicationSettingSaveLoader, c"set_TrainingHighSpeedType", 1);
    }
}

// Runs on the game main thread wherever the speed groups are applied, so a config change taken
// from the Config Editor lands at the next scene change. The writes are idempotent: the current
// value is read first and only a lower value is raised.
pub fn apply() {
    if !Hachimi::instance().config.load().high_speed_settings {
        return;
    }

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

    if story != max {
        SaveHighSpeedType(max);

        let after = get_StoryHighSpeedType(loader);
        info!("HighSpeedSetting: story high speed {} -> {} via StoryManager::SaveHighSpeedType, read back {}", story, max, after);
    }

    if training < max {
        set_TrainingHighSpeedType(loader, max);

        let after = get_TrainingHighSpeedType(loader);
        info!("HighSpeedSetting: training high speed {} -> {}, read back {}", training, max, after);
    }
}
