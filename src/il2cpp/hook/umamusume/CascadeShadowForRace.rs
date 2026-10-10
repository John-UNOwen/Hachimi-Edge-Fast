use crate::{
    core::{game::Region, Hachimi},
    il2cpp::{symbols::get_method_addr, types::*}
};
use super::CascadeShadow;

type CascadeShadowForRace_GetShadowResolutionFn = extern "C" fn(this: *mut Il2CppObject, default_shadow_map_width: i32) -> i32;
def_detour! {
    CascadeShadowForRace_GetShadowResolution(this: *mut Il2CppObject, default_shadow_map_width: i32) answer -> i32 {
            let orig = get_orig_fn!(CascadeShadowForRace_GetShadowResolution, CascadeShadowForRace_GetShadowResolutionFn)(this, default_shadow_map_width);
        // Published before the config read, for the same reason as `CascadeShadow`: 0 is not a
        // shadow map width, and the game's own answer is available before the half that can trip.
        answer.publish(orig);
        CascadeShadow::shadow_resolution(orig)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if Hachimi::instance().game.region != Region::Japan {
        return;
    }

    get_class_or_return!(umamusume, "Gallop.RenderPipeline", CascadeShadowForRace);

    let CascadeShadowForRace_GetShadowResolution_addr = get_method_addr(CascadeShadowForRace, c"GetShadowResolution", 1);
    new_hook!(CascadeShadowForRace_GetShadowResolution_addr, CascadeShadowForRace_GetShadowResolution);
}
