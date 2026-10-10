use std::sync::atomic::{self, AtomicBool};

use crate::{
    core::{Hachimi, game::Region},
    il2cpp::{
        hook::umamusume::AnimationSpeed,
        symbols::{get_field_from_name, get_method_addr},
        types::*
    }
};

def_field_value_accessors!(get__choiceAutoSelectWaitTime, set__choiceAutoSelectWaitTime, _CHOICEAUTOSELECTWAITTIME_FIELD, f32);

static IS_CHECKING_CHOICE_AUTO_TAP: AtomicBool = AtomicBool::new(false);
pub fn is_checking_choice_auto_tap() -> bool {
    IS_CHECKING_CHOICE_AUTO_TAP.swap(false, atomic::Ordering::Relaxed)
}

type CheckChoiceAutoTapFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    CheckChoiceAutoTap(this: *mut Il2CppObject) {
            IS_CHECKING_CHOICE_AUTO_TAP.store(true, atomic::Ordering::Relaxed);

        // Global has a different way of handling choice auto select delay in stories
        let is_global = Hachimi::instance().game.region == Region::Global;
        // The floor on the delay and the ceiling on the multiplier are in the mirror this reads, not in
        // the Config Editor's slider range: egui clamps to the range and then snaps from `range.start`,
        // so the left end of the slider is a real value, and config.json is read unbounded. C24 measured
        // `0.75 / 0.0001` = 7500 reaching the game's own accumulator through this line. This half scales
        // seconds, so AnimationSpeed caps it at MAX_STORY_CHOICE_AUTO_SELECT_MULTIPLIER; the story time
        // scale half is capped at MAX_TIME_SCALE in `StoryViewController.rs`. NAN is the inert setting.
        let mult = if is_global { AnimationSpeed::story_choice_wait_time_multiplier() } else { 1.0 };
        let needs_scaling = mult.is_finite() && mult != 1.0;
        let before = if needs_scaling {
            get__choiceAutoSelectWaitTime(this)
        } else {
            0.0
        };

        get_orig_fn!(CheckChoiceAutoTap, CheckChoiceAutoTapFn)(this);

        if needs_scaling {
            let after = get__choiceAutoSelectWaitTime(this);
            let increment = after - before;
            if increment > 0.0 {
                // _choiceAutoSelectWaitTime accumulates elapsed time upward from 0
                // Auto select triggers when it reaches SINGLE_CHOICE_AUTO_SELECT_DURATION (0.75)
                // Scale the increment so it takes "delay" seconds instead of 0.75
                set__choiceAutoSelectWaitTime(this, before + increment * mult);
                AnimationSpeed::hit(18, "StoryChoiceController::CheckChoiceAutoTap", increment, increment * mult);
            }
        }

        IS_CHECKING_CHOICE_AUTO_TAP.store(false, atomic::Ordering::Relaxed);
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryChoiceController);

    unsafe {
        _CHOICEAUTOSELECTWAITTIME_FIELD = get_field_from_name(StoryChoiceController, c"_choiceAutoSelectWaitTime");
    }

    let CheckChoiceAutoTap_addr = get_method_addr(StoryChoiceController, c"CheckChoiceAutoTap", 0);
    new_hook!(CheckChoiceAutoTap_addr, CheckChoiceAutoTap);
}