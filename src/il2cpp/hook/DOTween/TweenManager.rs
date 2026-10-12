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
        //
        // `tween_clocks` also asks the training cut whether it is playing, and hands this pair back untouched
        // while it is (ledger item 106): a cut parked on `UnityEngine.WaitForFixedUpdate` while the animation
        // it waits on finished on the multiplied clock is how three sessions stalled a training turn at
        // `ui_animation_scale 20.0`. The hold is one cut wide and its bound is measured on the game thread's
        // tick, in `TrainingCuttProbe::tick_cut_gate`.
        //
        // And it holds the tick itself under `AnimationSpeed::MAX_TWEEN_TICK_ADVANCE_SEC`: this argument is the
        // whole tween time one call to the game's tween manager advances every delta channel tween by, so a
        // tick longer than the animation it is speeding closes that animation - a `Sequence` leg, an inserted
        // callback, the completion an `Action` is waiting on - in one step, before the coroutine that was going
        // to watch for it parks on it. Run 57's own frame clock read a mean of 33.7 ms - `frames 74951 over
        // 2527736 ms mean 33.7 ms` at 2528 s, 33.7 to 33.8 ms in every window through its blocked tail - so that
        // session's 20x clock carried 674 ms at once. Its last training door call is `TrainingParamChangeUI::
        // OnAllTypewriteEnd() call 5` at 20:45:37.757 and the 2,412 s after it prints periodic totals and no door
        // call, the last reading `open now 2417805 ms` on a goal complete dialog whose acknowledge button is
        // inserted 0.3 to 0.5 s into its own entry sequence (C58, ledger item 107). The trim lands on what this
        // fork's lever added and never on the delta the game handed, so a long frame arrives as the game wrote
        // it.
        let (delta_time, independent_time) = AnimationSpeed::tween_clocks(delta_time, independent_time);

        get_orig_fn!(Update, UpdateFn)(update_type, delta_time, independent_time);
    }
}

pub fn init(DOTween: *const Il2CppImage) {
    get_class_or_return!(DOTween, "DG.Tweening.Core", TweenManager);

    let Update_addr = get_method_addr(TweenManager, c"Update", 3);

    new_hook!(Update_addr, Update);
}
