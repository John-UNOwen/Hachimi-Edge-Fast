use crate::il2cpp::{hook::umamusume::AnimationSpeed, symbols::get_method_addr, types::*};

type UpdateFn = extern "C" fn(update_type: i32, delta_time: f32, independent_time: f32);
extern "C" fn Update(update_type: i32, mut delta_time: f32, mut independent_time: f32) {
    // The clamped mirror, not the config: this detour runs on every tween tick, and the
    // ceiling is what bounds the 0.1..=1000.0 both sliders offer (C5). Left unbounded, the
    // wizard's maximum setting handed DOTween 16 s of elapsed time per 60 fps tick.
    let scale = AnimationSpeed::ui_animation_scale();
    if scale != 1.0 {
        delta_time *= scale;
        independent_time *= scale;
    }
    get_orig_fn!(Update, UpdateFn)(update_type, delta_time, independent_time);
}

pub fn init(DOTween: *const Il2CppImage) {
    get_class_or_return!(DOTween, "DG.Tweening.Core", TweenManager);

    let Update_addr = get_method_addr(TweenManager, c"Update", 3);

    new_hook!(Update_addr, Update);
}