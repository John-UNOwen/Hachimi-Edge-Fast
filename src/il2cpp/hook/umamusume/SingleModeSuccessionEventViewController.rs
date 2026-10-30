// The Inspiration event screen (`Gallop.SingleModeSuccessionEventViewController`, view 1501) has no skip handler of
// its own: the widened dump names no skip on it, which is why the skip lever has no game button to press there
// (ledger item 111). What it does have is the player's own way forward, `OnClickSuccessionButton()`, which the game
// wires up in `SetupSuccessionButton()`. With `skip_inspiration_cut` on, this module lets the game set the button up
// exactly as it does now and then makes the click the player would have made, through the game's own handler, on the
// instance that handler belongs to, a beat after the button appeared.
//
// The three doors are from the live client dump (`run log/introspect-run62.log:24343` to `:24369`):
// `SetupSuccessionButton/0 -> void()`, `OnClickSuccessionButton/0 -> void()` and `UpdateView/0 -> void()`, all
// instance methods. They are armed in every build, not only under `debug_mode`, because the click has to land for a
// player who runs with no probes. The player session of 2026-10-28 (`run log/hachimi (2).log`) crossed `OnEndCutt`
// and `OnCompleteFadeOut` on six Inspiration visits and crossed neither on two that ended in a soft reset; the stay
// on the screen ran 2 to 4 s when the cut closed and 7 to 10 s when it did not. No log this fork owns has ever
// recorded a click here, because no build armed either of those two doors before this file.
//
// The press waits `PRESS_SETTLE_MS` of wall clock over `PRESS_SETTLE_FRAMES` of the view's own `UpdateView` frames
// after the game set the button up. C64 is the reason for waiting at all: asking in the frame a thing was just set up
// is the shape that left a completion closure reading a null on the training cut. The press goes to the instance
// `UpdateView` hands us rather than to a pointer this file saved earlier, which is the rule every other lever here
// follows: no pointer is called through after the frame that handed it.
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

use crate::il2cpp::{
    hook::umamusume::{
        AnimationSpeed, InheritFlowProbe,
        TrainingCuttProbe::{PROBE_DETAIL_LIMIT, class_for_label, gate_clock_us},
    },
    types::*,
};

const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;

// The game's own click handler, resolved on the class at init. `def_method_wrapper_fn!` keeps the press inert when it
// never arrived, the same case the arming log reports as a door that did not install (A4).
def_method_wrapper_fn!(press_succession_button, ON_CLICK_ADDR, (), this: *mut Il2CppObject);

// The one succession screen that can be up at a time, and its arm: which instance the game set the button up on,
// when, how many of that view's frames have passed since, and whether this build has already clicked for it.
static PENDING_INSTANCE: AtomicUsize = AtomicUsize::new(0);
static PENDING_ARMED_MS: AtomicI64 = AtomicI64::new(-1);
static PENDING_FRAMES: AtomicI64 = AtomicI64::new(0);
static PRESSED: AtomicBool = AtomicBool::new(false);
static MISS_REPORTED: AtomicBool = AtomicBool::new(false);

// Held across the press call. `OnClickSuccessionButton` is hooked too, so this is how the module tells its own press
// from a click the player made, without a second copy of the handler.
static PRESS_INSIDE: AtomicBool = AtomicBool::new(false);

static SETUP_CALLS: AtomicUsize = AtomicUsize::new(0);
static GAME_CLICKS: AtomicUsize = AtomicUsize::new(0);
static OUR_PRESSES: AtomicUsize = AtomicUsize::new(0);
// Our press seen from the door side. Counted apart from `OUR_PRESSES`, which numbers the press lines, so one press
// is one log line and one crossing of the game's handler.
static OUR_PRESS_CROSSINGS: AtomicUsize = AtomicUsize::new(0);
static PLAYER_BEAT_US: AtomicUsize = AtomicUsize::new(0);
static PRESS_DROPPED: AtomicUsize = AtomicUsize::new(0);

// When the game last set the button up, on the clock the Inspiration pump keeps. The pump uses it to stop asking a
// cut whose screen has already reached the step after the cut.
static BUTTON_ARMED_MS: AtomicI64 = AtomicI64::new(-1);

/// How long the auto click waits after the game set the button up before it presses. 250 ms is chosen, not measured:
/// it is the same order as the 200 ms this build already waits before asking a cut-in timeline for its end, which is
/// itself longer than the 176 ms the client waited in run 60a for the reason C64 gives.
pub(crate) const PRESS_SETTLE_MS: i64 = 250;

/// How many of the view's own `UpdateView` frames must pass over that window. Two doors, one window: a wall clock
/// alone cannot tell a frame that ran from a frame the view never got.
pub(crate) const PRESS_SETTLE_FRAMES: i64 = 3;

/// How long after the arm a click that has still not fired is reported as missed. Longer than any settle the view
/// needs and shorter than the 7 to 10 s stays that end in a soft reset, so a run that says nothing here is a run
/// where the click landed.
pub(crate) const PRESS_MISS_MS: i64 = 8_000;

/// Whether the armed screen is due its click. `armed_ms` is when the game set the button up, `-1` when it has not,
/// `frames` counts this view's `UpdateView` visits since then, `now_ms` is the current one, and `pressed` says this
/// build already clicked for this arm. The switch itself is the caller's fast exit.
pub(crate) fn succession_press_due(armed_ms: i64, frames: i64, now_ms: i64, pressed: bool) -> bool {
    !pressed && armed_ms >= 0 && frames >= PRESS_SETTLE_FRAMES && now_ms - armed_ms >= PRESS_SETTLE_MS
}

/// When the game last set the succession button up, on the pump's clock, or `-1` if it has not since the build
/// started.
pub(crate) fn button_armed_ms() -> i64 {
    BUTTON_ARMED_MS.load(Ordering::Relaxed)
}

/// Called from the Inspiration pump, whose door runs every frame of an Inspiration visit whether or not this view's
/// own `UpdateView` runs. Reports an arm that never fired, once per arm, so a run can tell a click that landed from
/// an option that reached nothing.
pub(crate) fn note_press_window(now_ms: i64) {
    let armed = PENDING_ARMED_MS.load(Ordering::Relaxed);

    if armed < 0 || PRESSED.load(Ordering::Relaxed) || MISS_REPORTED.load(Ordering::Relaxed) || now_ms - armed < PRESS_MISS_MS {
        return;
    }

    MISS_REPORTED.store(true, Ordering::Release);

    warn!(
        "Inspiration auto click never fired: the button was set up on {:#x} {} ms ago and this view's UpdateView did not reach its settle",
        PENDING_INSTANCE.load(Ordering::Relaxed),
        now_ms - armed,
    );
}

fn now_ms() -> i64 {
    gate_clock_us() / 1000
}

// The game set the button up: arm this instance for a click it has not had yet, on the count of crossings the door
// already took.
fn arm_press(this: *mut Il2CppObject, armed_number: usize) {
    let now = now_ms();

    BUTTON_ARMED_MS.store(now, Ordering::Relaxed);
    PENDING_INSTANCE.store(this as usize, Ordering::Relaxed);
    PENDING_ARMED_MS.store(now, Ordering::Relaxed);
    PENDING_FRAMES.store(0, Ordering::Relaxed);
    PRESSED.store(false, Ordering::Release);
    MISS_REPORTED.store(false, Ordering::Release);

    if armed_number <= PROBE_DETAIL_LIMIT {
        info!("Inspiration auto click armed {armed_number}: the game set the succession button up on {this:p}, waiting {PRESS_SETTLE_MS} ms and {PRESS_SETTLE_FRAMES} frames before the press");
    }
}

// Reached from the view's own per frame door, so `this` is an instance the game is holding right now.
fn pump_press(this: *mut Il2CppObject) {
    let instance = this as usize;

    if PENDING_INSTANCE.load(Ordering::Relaxed) != instance || PRESSED.load(Ordering::Relaxed) {
        return;
    }

    let armed = PENDING_ARMED_MS.load(Ordering::Relaxed);
    let now = now_ms();
    let frames = PENDING_FRAMES.fetch_add(1, Ordering::Relaxed) + 1;

    if !succession_press_due(armed, frames, now, false) {
        return;
    }

    // Nothing resolved behind the door: say it once through the wrapper and leave the screen its own click.
    if unsafe { ON_CLICK_ADDR } == 0 {
        PRESSED.store(true, Ordering::Release);
        press_succession_button(this);

        return;
    }

    if PRESS_INSIDE.swap(true, Ordering::AcqRel) {
        let dropped = PRESS_DROPPED.fetch_add(1, Ordering::Relaxed) + 1;

        if dropped <= PROBE_DETAIL_LIMIT {
            warn!("Inspiration auto click {dropped} dropped: a press is already inside the door for {this:p}");
        }

        return;
    }

    PRESSED.store(true, Ordering::Release);
    let presses = OUR_PRESSES.fetch_add(1, Ordering::Relaxed) + 1;

    press_succession_button(this);
    PRESS_INSIDE.store(false, Ordering::Release);

    if presses <= PROBE_DETAIL_LIMIT {
        info!("Inspiration auto click {presses}: pressed OnClickSuccessionButton on {this:p} {} ms and {frames} frames after the game set it up", now - armed);
    }
}

type SuccessionVoidFn = extern "C" fn(this: *mut Il2CppObject);

def_detour! {
    SuccessionEvent_SetupSuccessionButton(this: *mut Il2CppObject) {
            SETUP_CALLS.fetch_add(1, Ordering::Relaxed);
        // The inheritance probe keeps the leg that starts here. It counts the crossing whether or not this lever is
        // on, and does nothing at all when its own doors were never armed.
        InheritFlowProbe::note_succession_button_setup();

        get_orig_fn!(SuccessionEvent_SetupSuccessionButton, SuccessionVoidFn)(this);

        // Armed after the game's own wiring ran, so the listeners the game added are in place before anything is
        // asked of them.
        if AnimationSpeed::skip_inspiration_cut() && !this.is_null() {
            arm_press(this, SETUP_CALLS.load(Ordering::Relaxed));
        }
    }
    bail {
                get_orig_fn!(SuccessionEvent_SetupSuccessionButton, SuccessionVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_OnClickSuccessionButton(this: *mut Il2CppObject) {
            // The click closes the probe's press leg and opens the one that runs to the teardown, for a player's
        // click and for this build's press alike; the lines this module prints name which was which.
        InheritFlowProbe::note_succession_button_click();

        // The press this build made comes back through here, and the original is the handler that press is for,
        // so both paths call it and only the bookkeeping differs.
        if PRESS_INSIDE.load(Ordering::Acquire) {
            OUR_PRESS_CROSSINGS.fetch_add(1, Ordering::Relaxed);
        }
        else {
            let clicks = GAME_CLICKS.fetch_add(1, Ordering::Relaxed) + 1;

            if !this.is_null() && PENDING_INSTANCE.load(Ordering::Relaxed) == this as usize && !PRESSED.load(Ordering::Relaxed) {
                let beaten = PLAYER_BEAT_US.fetch_add(1, Ordering::Relaxed) + 1;
                let waited = now_ms() - PENDING_ARMED_MS.load(Ordering::Relaxed);

                PENDING_INSTANCE.store(0, Ordering::Relaxed);
                PENDING_ARMED_MS.store(-1, Ordering::Relaxed);

                if beaten <= PROBE_DETAIL_LIMIT {
                    info!("Inspiration auto click {beaten} stood down: the player clicked {this:p} {waited} ms after the game set the button up");
                }
            }

            if clicks > PROBE_DETAIL_LIMIT && clicks % PROBE_DETAIL_LIMIT == 0 {
                info!("Inspiration click door: {clicks} clicks on the succession button this run");
            }
        }

        get_orig_fn!(SuccessionEvent_OnClickSuccessionButton, SuccessionVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_OnClickSuccessionButton, SuccessionVoidFn)(this)
    }
}

def_detour! {
    SuccessionEvent_UpdateView(this: *mut Il2CppObject) {
            // One atomic load on the fast path: with the option off this door does nothing but the original call.
            if AnimationSpeed::skip_inspiration_cut() && !this.is_null() {
            pump_press(this);
        }

        get_orig_fn!(SuccessionEvent_UpdateView, SuccessionVoidFn)(this);
    }
    bail {
                get_orig_fn!(SuccessionEvent_UpdateView, SuccessionVoidFn)(this)
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    // Armed in every build: a player who never turns `debug_mode` on still gets the click.
    let Some(class) = class_for_label(umamusume, "Gallop.SingleModeSuccessionEventViewController") else {
        warn!("Inspiration auto click: no SingleModeSuccessionEventViewController in this client, the screen keeps its own click");
        return;
    };

    let on_click_addr = unsafe { AnimationSpeed::resolve_method(class, "OnClickSuccessionButton", NO_PARAMS, VOID) };
    let setup_addr = unsafe { AnimationSpeed::resolve_method(class, "SetupSuccessionButton", NO_PARAMS, VOID) };
    let update_addr = unsafe { AnimationSpeed::resolve_method(class, "UpdateView", NO_PARAMS, VOID) };

    unsafe { ON_CLICK_ADDR = on_click_addr };

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;

    if on_click_addr != 0 {
        new_hook!(on_click_addr, SuccessionEvent_OnClickSuccessionButton);
        installed += 1;
    }
    else {
        missing.push("OnClickSuccessionButton()");
    }

    if setup_addr != 0 {
        new_hook!(setup_addr, SuccessionEvent_SetupSuccessionButton);
        installed += 1;
    }
    else {
        missing.push("SetupSuccessionButton()");
    }

    if update_addr != 0 {
        new_hook!(update_addr, SuccessionEvent_UpdateView);
        installed += 1;
    }
    else {
        missing.push("UpdateView()");
    }

    info!(
        "Inspiration auto click: {installed} of 3 doors armed on SingleModeSuccessionEventViewController in every build, waiting {} ms and {} frames after the game sets the button up",
        PRESS_SETTLE_MS,
        PRESS_SETTLE_FRAMES,
    );

    if !missing.is_empty() {
        warn!("Inspiration auto click: not armed on {}", missing.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unarmed_screen_is_never_clicked() {
        assert!(!succession_press_due(-1, 90, 90_000, false), "the game has not set the button up yet");
        assert!(!succession_press_due(1_000, 90, 90_000, true), "this build already clicked for this arm");
    }

    #[test]
    fn the_press_waits_out_both_the_wall_clock_and_the_view_frames() {
        assert!(!succession_press_due(1_000, 2, 1_250, false), "one frame short of the settle");
        assert!(!succession_press_due(1_000, 3, 1_249, false), "1 ms short of the settle");
        assert!(succession_press_due(1_000, 3, 1_250, false), "both windows are due at the settle");
        assert!(succession_press_due(1_000, 9, 12_000, false), "a view that ticked slowly is still due its click");
    }

    #[test]
    fn the_window_is_wider_than_a_slow_player_and_narrower_than_a_stuck_run() {
        assert!(PRESS_SETTLE_MS >= 200, "the settle does not drop below the window this build already uses for a cut-in ask");
        assert!(PRESS_MISS_MS > 6_000, "the miss line has to sit past a stay the player would have clicked through");
    }
}
