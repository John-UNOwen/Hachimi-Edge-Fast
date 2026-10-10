use crate::il2cpp::{hook::umamusume::AnimationSpeed, symbols::get_method_addr, types::*};

type UpdateFn = extern "C" fn(update_type: i32, delta_time: f32, independent_time: f32);
def_detour! {
    Update(update_type: i32, delta_time: f32, independent_time: f32) {
            // The clamped mirror, not the config: this detour runs on every tween tick, and the
        // ceiling is what bounds the 0.1..=1000.0 both sliders offer (C5). Left unbounded, the
        // wizard's maximum setting handed DOTween 16 s of elapsed time per 60 fps tick.
        //
        // Only the delta channel is scaled; `independent_time` is handed on exactly as the caller
        // computed it. DOTween reads the two per tween - `DG.Tweening.Core.TweenManager.Update(tween,
        // deltaTime, independentTime, ..)` advances a tween by
        // `float tDeltaTime = (t.isIndependentUpdate ? independentTime : deltaTime) * t.timeScale;` -
        // and `DOTweenComponent` computes `independentTime` from `Time.unscaledDeltaTime`. The second
        // argument is therefore the clock of the tweens the game marked time scale independent, the
        // channel DOTween keeps outside every time lever so UI keeps animating while the game is
        // paused. Scaling it - this line's other half, C5 - took wall clock out of a tween the game
        // deliberately took out of the game's own clock: 1 s of real time UI animation in 50 ms at
        // `MAX_UI_ANIMATION_SCALE`, pause included. The first argument is the game's `Time.deltaTime`, so it
        // already carries `Time.timeScale`, which this fork's write layer fills: `AnimationSpeed::ui_clock_of`
        // caps this multiply by what `MAX_TWEEN_SPEED_PRODUCT` leaves once the whole scale that write left in
        // the game is counted, so a 20x lever under a Unity holding 5.0 multiplies this argument by 4 here and
        // the completion the two add up to is 20x, not 100x (C58, ledger item 62). The arithmetic the hook
        // performs lives in `AnimationSpeed::tween_clocks`, which the `cargo test --lib` case
        // `the_tween_clock_layer_scales_the_delta_channel_and_not_the_independent_one` drives; the wrapper
        // itself cannot run in a test, because `get_orig_fn!` is 0 outside `init` (C1).
        let (delta_time, independent_time) = AnimationSpeed::tween_clocks(delta_time, independent_time);

        get_orig_fn!(Update, UpdateFn)(update_type, delta_time, independent_time);
    }
}

pub fn init(DOTween: *const Il2CppImage) {
    get_class_or_return!(DOTween, "DG.Tweening.Core", TweenManager);

    let Update_addr = get_method_addr(TweenManager, c"Update", 3);

    new_hook!(Update_addr, Update);
}
