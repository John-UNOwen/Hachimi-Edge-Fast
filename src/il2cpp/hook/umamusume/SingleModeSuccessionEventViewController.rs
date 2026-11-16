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
// The press waits two things, and the second one is what the first run of this module corrected. It waits
// `PRESS_SETTLE_MS` of wall clock over `PRESS_SETTLE_FRAMES` of the view's own `UpdateView` frames, because C64 is
// the reason for waiting at all: asking in the frame a thing was just set up is the shape that left a completion
// closure reading a null on the training cut. And it waits for the game to reach its own `OnEndCutt` for this
// screen's cut, because the build that pressed measured what the arm does not mean: on the Inspiration screen at
// 06:35:38 of the session of 2026-10-30 the client crossed `SetupSuccessionButton` at 38.504 and only then
// `PrepareCutIn` at 38.534 and `PlayCutIn` at 38.546, so a button that exists can sit in front of 3.2 s of cut, and
// the click that build made 392 ms after the arm landed inside the cut it was waiting for. `OnEndCutt` is the door
// the game itself uses to say the cut is over, so the press waits for that, and the arm alone no longer starts a
// countdown to a click. The press goes to the instance `UpdateView` hands us rather than to a pointer this file
// saved earlier, which is the rule every other lever here follows: no pointer is called through after the frame that
// handed it.
//
// Where the cut end is watched from depends on the build, and only one door ever sits on that address. With
// `debug_mode` on, `InheritFlowProbe` hooks `OnEndCutt` and this module reads the cut runs it closes. With it off
// that probe arms nothing, so this module hooks `OnEndCutt` itself, as a door that only counts, and the press reads
// its own count.
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed, InheritFlowProbe,
            TrainingCuttProbe::{PROBE_DETAIL_LIMIT, class_for_label, gate_clock_us, CutProbe},
        },
        types::*,
    },
};

const NO_PARAMS: &[Il2CppTypeEnum] = &[];
// `OnEndCutt/1 -> void(class<Gallop.CutIn.Cutt.CutInTimelineController>)`, the cut end handing the timeline that
// ended (`run log/introspect-run62.log:24351`).
const ONE_INFO: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_CLASS];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;

// The game's own click handler, resolved on the class at init. `def_method_wrapper_fn!` keeps the press inert when it
// never arrived, the same case the arming log reports as a door that did not install (A4).
def_method_wrapper_fn!(press_succession_button, ON_CLICK_ADDR, (), this: *mut Il2CppObject);

// The cut end, watched here instead of through `InheritFlowProbe` in the builds where that probe arms no doors. It
// counts the crossing and changes nothing else. Traced rather than counted because this door is crossed once per
// Succession visit and the run that needs its lines is the visit that never crossed it: run 73 played 11 visits and
// the shared limit printed 8 (C85).
const END_WATCH_TRACE_LIMIT: usize = 40;
type EndCuttFn = extern "C" fn(this: *mut Il2CppObject, controller: *mut Il2CppObject);
static END_WATCH: CutProbe = CutProbe::traced("SingleModeSuccessionEventViewController::OnEndCutt watched by the auto click", END_WATCH_TRACE_LIMIT);

def_detour! {
    SuccessionEvent_OnEndCuttWatch(this: *mut Il2CppObject, controller: *mut Il2CppObject) {
            END_WATCH.count();

        get_orig_fn!(SuccessionEvent_OnEndCuttWatch, EndCuttFn)(this, controller);
    }
    bail {
                get_orig_fn!(SuccessionEvent_OnEndCuttWatch, EndCuttFn)(this, controller)
    }
}

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

// How many cut ends had been counted when the game last set the button up, and the gate-clock moment of the newest
// cut end since. The first is what the press compares against; the second is what the press line reports the click
// against. The arm's own moment lives in `PENDING_ARMED_MS`, which the press and the waiting line both read.
static ARM_END_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_END_COUNT: AtomicUsize = AtomicUsize::new(0);
static LAST_END_MS: AtomicI64 = AtomicI64::new(-1);

/// How long the auto click waits after the game set the button up before it presses. 250 ms is chosen, not measured:
/// it is the same order as the 200 ms this build already waits before asking a cut-in timeline for its end, which is
/// itself longer than the 176 ms the client waited in run 60a for the reason C64 gives.
pub(crate) const PRESS_SETTLE_MS: i64 = 250;

/// How many of the view's own `UpdateView` frames must pass over that window. Two doors, one window: a wall clock
/// alone cannot tell a frame that ran from a frame the view never got.
pub(crate) const PRESS_SETTLE_FRAMES: i64 = 3;

/// How long after the arm a click that has still not fired is reported. Longer than any settle the view needs and
/// shorter than the 7 to 10 s stays that end in a soft reset, so a run that says nothing here is a run where the click
/// landed. A screen whose cut never closes reports here too, and says that as the reason.
pub(crate) const PRESS_MISS_MS: i64 = 8_000;

/// How many cut ends this build has counted on this screen. `InheritFlowProbe` answers for a build that armed its
/// doors; a build that armed none counts the same door through this module's own watch.
fn cut_end_count() -> usize {
    InheritFlowProbe::cut_runs_closed().unwrap_or_else(|| END_WATCH.calls())
}

/// Whether the screen's own cut is over. `ends_at_arm` is how many times the game had reached its cut end when it
/// wired the button up and `ends_now` how many it has reached since. The build that first pressed answers what a click
/// in front of that is worth: it landed 392 ms after the arm, 2.9 s inside a 3.240 s cut, and the door that sends the
/// inheritance request never ran.
pub(crate) fn succession_cut_over(ends_at_arm: usize, ends_now: usize) -> bool {
    ends_now > ends_at_arm
}

/// Whether the armed screen is due its click. `armed_ms` is when the game set the button up, `-1` when it has not,
/// `frames` counts this view's `UpdateView` visits since then, `now_ms` is the current one, `pressed` says this build
/// already clicked for this arm, and `cut_over` is `succession_cut_over` on this screen's cut-end count. The switch
/// itself is the caller's fast exit.
pub(crate) fn succession_press_due(armed_ms: i64, frames: i64, now_ms: i64, pressed: bool, cut_over: bool) -> bool {
    !pressed && armed_ms >= 0 && cut_over && frames >= PRESS_SETTLE_FRAMES && now_ms - armed_ms >= PRESS_SETTLE_MS
}

/// Called from the Inspiration pump, whose door runs every frame of an Inspiration visit whether or not this view's
/// own `UpdateView` runs. Reports an arm that has not fired, once per arm, and names which of the two waits it is
/// still in: a cut the game never ended, or a view that never reached its settle.
pub(crate) fn note_press_window(now_ms: i64) {
    let armed = PENDING_ARMED_MS.load(Ordering::Relaxed);

    if armed < 0 || PRESSED.load(Ordering::Relaxed) || MISS_REPORTED.load(Ordering::Relaxed) || now_ms - armed < PRESS_MISS_MS {
        return;
    }

    MISS_REPORTED.store(true, Ordering::Release);

    let waiting_on = if succession_cut_over(ARM_END_COUNT.load(Ordering::Relaxed), cut_end_count()) {
        "this view's UpdateView never reached its settle"
    }
    else {
        "the game never reached its own cut end, so the click stayed the player's to make"
    };

    warn!(
        "Inspiration auto click still waiting: the button was set up on {:#x} {} ms ago and {waiting_on}",
        PENDING_INSTANCE.load(Ordering::Relaxed),
        now_ms - armed,
    );
}

/// The succession screen the game last set its button up on, or null when no arm is pending. The Inspiration watch
/// reads the cut through this so a parked visit can be described from the screen's own side and not only from the
/// timeline it plays. `SetupSuccessionButton` is armed in every build, so a visit that reached its button has an
/// instance here; one that never got that far has none, and the watch says so.
pub(crate) fn pending_view() -> *mut Il2CppObject {
    let instance = PENDING_INSTANCE.load(Ordering::Relaxed);

    if instance == 0 {
        ::std::ptr::null_mut()
    }
    else {
        instance as *mut Il2CppObject
    }
}

/// How many cut ends this build has counted on this screen, the same number the press rule waits on. The Inspiration
/// watch prints it so a line about a parked cut says whether the game's own cut end ran.
pub(crate) fn cut_ends_counted() -> usize {
    cut_end_count()
}

fn now_ms() -> i64 {
    gate_clock_us() / 1000
}

// The game set the button up: arm this instance for a click it has not had yet, on the count of crossings the door
// already took, and remember how many cut ends had gone by, which is what the press waits for.
fn arm_press(this: *mut Il2CppObject, armed_number: usize) {
    let now = now_ms();
    let ends = cut_end_count();

    PENDING_INSTANCE.store(this as usize, Ordering::Relaxed);
    PENDING_ARMED_MS.store(now, Ordering::Relaxed);
    PENDING_FRAMES.store(0, Ordering::Relaxed);
    PRESSED.store(false, Ordering::Release);
    MISS_REPORTED.store(false, Ordering::Release);
    ARM_END_COUNT.store(ends, Ordering::Relaxed);
    LAST_END_COUNT.store(ends, Ordering::Relaxed);
    LAST_END_MS.store(now, Ordering::Relaxed);

    if armed_number <= PROBE_DETAIL_LIMIT {
        info!("Inspiration auto click armed {armed_number}: the game set the succession button up on {this:p} with {ends} cut ends already counted, waiting for its own cut end and then {PRESS_SETTLE_MS} ms and {PRESS_SETTLE_FRAMES} frames");
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

    // The cut end, watched on the count rather than on a door of this module's own in a probe build. A rise since the
    // arm is stamped with this frame's time, which is what the press line reports the click against.
    let ends = cut_end_count();

    if ends > LAST_END_COUNT.swap(ends, Ordering::Relaxed) {
        LAST_END_MS.store(now, Ordering::Relaxed);
    }

    let armed_ends = ARM_END_COUNT.load(Ordering::Relaxed);

    if !succession_press_due(armed, frames, now, false, succession_cut_over(armed_ends, ends)) {
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
    let since_end_ms = now - LAST_END_MS.load(Ordering::Relaxed);

    press_succession_button(this);
    PRESS_INSIDE.store(false, Ordering::Release);

    if presses <= PROBE_DETAIL_LIMIT {
        info!("Inspiration auto click {presses}: pressed OnClickSuccessionButton on {this:p} {} ms after the game set the button up and {since_end_ms} ms after its own cut end, on frame {frames} of the settle", now - armed);
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

    // The cut end, watched from here in the build where `InheritFlowProbe` arms no doors at all. Which side watches it
    // is decided by the same setting that probe keys its own arming on, so one address ever carries one hook.
    let watch_end_here = !Hachimi::instance().config.load().debug_mode;
    let end_addr = if watch_end_here {
        unsafe { AnimationSpeed::resolve_method(class, "OnEndCutt", ONE_INFO, VOID) }
    }
    else {
        0
    };

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

    if end_addr != 0 {
        new_hook!(end_addr, SuccessionEvent_OnEndCuttWatch);
        installed += 1;
    }
    else if watch_end_here {
        missing.push("OnEndCutt() watched here");
    }

    let ends_read_from = match (watch_end_here, end_addr != 0) {
        (true, true) => "counting the cut end through this module's own door",
        (true, false) => "with no cut-end door resolved, so the press never presses and the click stays the player's",
        (false, _) => "counting the cut end through the inheritance probe",
    };
    // The fourth door is only armed in the build where the probe leaves that address free, so the number the line
    // counts up to is the number this module had doors to arm.
    let door_count = 3 + usize::from(watch_end_here);

    info!(
        "Inspiration auto click: {installed} of {door_count} doors armed on SingleModeSuccessionEventViewController in every build, waiting for the game's own cut end and then {} ms and {} frames on the view's own frames, {ends_read_from}",
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
        assert!(!succession_press_due(-1, 90, 90_000, false, true), "the game has not set the button up yet");
        assert!(!succession_press_due(1_000, 90, 90_000, true, true), "this build already clicked for this arm");
    }

    #[test]
    fn the_press_waits_out_both_the_wall_clock_and_the_view_frames() {
        assert!(!succession_press_due(1_000, 2, 1_250, false, true), "one frame short of the settle");
        assert!(!succession_press_due(1_000, 3, 1_249, false, true), "1 ms short of the settle");
        assert!(succession_press_due(1_000, 3, 1_250, false, true), "both windows are due at the settle");
        assert!(succession_press_due(1_000, 9, 12_000, false, true), "a view that ticked slowly is still due its click");
    }

    // The run line this rule replaces. At 06:35:38 of the session of 2026-10-30 this build pressed 392 ms and 3 frames
    // after `SetupSuccessionButton`, with the cut end at zero crossings, `PrepareCutIn` 30 ms after the arm and
    // `PlayCutIn` 40 ms after it, and that visit sat on the screen until a soft reset 176 s later. A settle alone is
    // not a permission to click.
    #[test]
    fn a_button_in_front_of_a_live_cut_is_not_clicked() {
        assert!(!succession_press_due(1_000, 9, 30_000, false, false), "the settle ran out and the game still has its cut");
        assert!(!succession_cut_over(0, 0), "no cut end has gone by since the button was set up");
        assert!(succession_cut_over(0, 1), "the game reached its own cut end after arming the button");
        assert!(succession_cut_over(3, 4), "and a screen ending a later cut counts the same way");
        assert!(!succession_cut_over(4, 4), "an end that went by before the arm belongs to an earlier cut");
    }

    #[test]
    fn the_window_is_wider_than_a_slow_player_and_narrower_than_a_stuck_run() {
        assert!(PRESS_SETTLE_MS >= 200, "the settle does not drop below the window this build already uses for a cut-in ask");
        assert!(PRESS_MISS_MS > 6_000, "the waiting line has to sit past a stay the player would have clicked through");
    }
}
