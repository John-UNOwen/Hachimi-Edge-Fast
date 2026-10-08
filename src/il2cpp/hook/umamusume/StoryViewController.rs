use crate::il2cpp::{hook::umamusume::AnimationSpeed, symbols::get_method_addr, types::*};

use super::StoryChoiceController;

type GetTimeScaleByHighSpeedTypeFn = extern "C" fn() -> f32;
extern "C" fn GetTimeScaleByHighSpeedType() -> f32 {
    let res = get_orig_fn!(GetTimeScaleByHighSpeedType, GetTimeScaleByHighSpeedTypeFn)();
    if !StoryChoiceController::is_checking_choice_auto_tap() {
        return res;
    }

    // The getter half of the story choice setting. `CheckChoiceAutoTap` scales a wait time
    // accumulator by its ceiling, and what this returns is the scale the story timeline steps its
    // clips by, so it goes through AnimationSpeed, which caps the product at MAX_TIME_SCALE and
    // leaves a scale of 1.0 for a delay at or above the game's own 0.75, a pause and a slow motion
    // (C24). One atomic read, no config load on a story path getter.
    let scaled = AnimationSpeed::story_choice_time_scale(res);
    AnimationSpeed::hit(17, "StoryViewController::GetTimeScaleByHighSpeedType", res, scaled);

    scaled
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryViewController);

    let GetTimeScaleByHighSpeedType_addr = get_method_addr(StoryViewController, c"GetTimeScaleByHighSpeedType", 0);

    new_hook!(GetTimeScaleByHighSpeedType_addr, GetTimeScaleByHighSpeedType);
}
