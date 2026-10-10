use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use crate::il2cpp::{api::il2cpp_resolve_icall, hook::umamusume::AnimationSpeed, types::*};

/*** Time.timeScale ***/

/// Set while we are the ones writing `Time.timeScale`, so the hook below passes our
/// own value through instead of multiplying it a second time.
static APPLYING: AtomicBool = AtomicBool::new(false);

/// Raised when the config changes. The write itself is deferred to the game thread,
/// because Unity's native setters are not meant to be called from the overlay thread.
static DIRTY: AtomicBool = AtomicBool::new(false);

// The lever value already pushed into the game, NAN until the first pass has run. It is
// the marker that makes `apply()` one write per config change instead of one per view
// change: a call that finds its own lever here has nothing left to do, so a pause or a
// game fast forward the game chose after that pass keeps the value the game chose.
static APPLIED_LEVER: AtomicU32 = AtomicU32::new(f32::NAN.to_bits());

/// The last value the game itself asked for, exactly as it came through the hook below.
/// NAN until the game writes one. This is the baseline `apply()` scales from: reading
/// `Time.timeScale` back reads our own scaled write, and multiplying that again is the
/// compounding C35 describes.
static GAME_REQUESTED: AtomicU32 = AtomicU32::new(f32::NAN.to_bits());

/// The last number *this layer* handed to the game's setter - a scaled write from the hook
/// below, or the one write `apply()` makes - NAN until the first. It is what makes the layer
/// provably unable to multiply its own write twice: a game write that arrives as exactly this
/// number is the game passing our product back (the read-modify-write loop C22/C24/C35 refuse,
/// AGENTS section 5 "never multiply the current value"), and the lever is already inside it. It
/// is also what makes the layer provably unable to lower a game value: `apply()` may write below
/// `Time.timeScale`'s current only when the current is one of these numbers, so the only value
/// ever lowered is one this layer itself put there, down to the game's own last request.
/// `StoryTimelineController`'s `HIGH_SPEED_LAST_WRITTEN` is the same device for the static it
/// rewrites.
static PRODUCED: AtomicU32 = AtomicU32::new(f32::NAN.to_bits());

/// Addresses of the icall implementations. 0 when this build does not expose them, in
/// which case every entry point here stays inert instead of calling through a null pointer.
static mut SET_TIME_SCALE_ADDR: usize = 0;
static mut GET_TIME_SCALE_ADDR: usize = 0;

type SetTimeScaleFn = extern "C" fn(value: f32);

/// The trampoline of the original icall, resolved once and kept here. This hook does not reach
/// the original through `get_orig_fn!` - it asks `get_trampoline_addr` for the icall by name -
/// and that is a registry read per call: a lock over the hook map, on a hook the game writes
/// through whenever it touches `Time.timeScale`. Cached, the neutral path is one atomic load and
/// a jump.
///
/// `get_orig_fn!` (C33) keeps the same address per detour and re-reads only when the registry's
/// install generation moves; this copy cannot go stale for the same reason a detour's cannot,
/// which is stronger than a generation: a detour is only reachable through its own trampoline, so
/// the moment the hook is removed nothing routes into the code that would read the cached address
/// any more. It stays 0 until the first armed call has resolved it, which also keeps `init` out of
/// the hook map while `begin_batch`/`finish_batch` are arming.
static TRAMPOLINE: AtomicUsize = AtomicUsize::new(0);
static NO_TRAMPOLINE_WARNED: AtomicBool = AtomicBool::new(false);

/// Hand `value` to the original implementation. An unresolved trampoline is never called
/// through address 0.
fn call_original(value: f32) {
    let mut addr = TRAMPOLINE.load(Ordering::Acquire);

    if addr == 0 {
        addr = crate::core::Hachimi::instance().interceptor.get_trampoline_addr(set_timeScale as *const () as usize);

        if addr == 0 {
            if !NO_TRAMPOLINE_WARNED.swap(true, Ordering::AcqRel) {
                error!("Time::set_timeScale has no trampoline, the hook passes the game's value on");
            }

            return;
        }

        TRAMPOLINE.store(addr, Ordering::Release);
    }

    unsafe {
        let set_time_scale: SetTimeScaleFn = std::mem::transmute(addr);
        set_time_scale(value);
    }
}

/// Installed is not the same as called (A4), and a scaling log cannot answer that question at
/// all: `AnimationSpeed::hit` prints only when a value changed, so with the lever at its
/// neutral 1.0 this hook printed nothing anywhere and no run could tell it off from a hook the
/// game never reached. Same shape the story stepping counters use - the first few calls in
/// full, then one line every `CALL_CHUNK` calls, so a path that runs every frame reports a
/// total instead of flooding the log.
const CALL_DETAIL_LIMIT: usize = 6;
const CALL_CHUNK: usize = 4096;
static CALLS: AtomicUsize = AtomicUsize::new(0);

fn log_call(value: f32, scaled: f32, lever: f32, echoed: bool) {
    let calls = CALLS.fetch_add(1, Ordering::Relaxed) + 1;
    let tail = if echoed { ", echoed" } else { "" };

    if calls <= CALL_DETAIL_LIMIT {
        debug!("Time::set_timeScale call {calls}: {value} -> {scaled} (lever x{lever}{tail})");
    }
    else if calls % CALL_CHUNK == 0 {
        debug!("Time::set_timeScale {calls} calls, most recent {value} -> {scaled} (lever x{lever}{tail})");
    }
}

/// What the hook does with one value the game wrote: record it as the game's own request, and
/// hand back the value to pass on. Held apart from the detour and from the jump to the original
/// so the neutral fast exit and the scaling branch are the shipped code the tests run, instead of
/// a test module repeating them. `set_timeScale` reads the mirror and hands the lever in, so this
/// half needs no config and reaches no game pointer.
fn scale_game_write(value: f32, lever: f32) -> f32 {
    if value == f32::from_bits(PRODUCED.load(Ordering::Acquire)) {
        // The game is handing back the exact number this layer last put in `Time.timeScale`.
        // The lever is already inside that number; multiplying it a second time is the
        // read-modify-write compounding C22/C24/C35 exist to refuse - the ceiling bounds that
        // compounding, but a bounded multiply-twice is not what "never multiply the current
        // value" asks for. Pass it through, and leave `GAME_REQUESTED` on the raw request the
        // produced number was derived from: recording the scaled echo there is what would
        // compound the baseline a later `apply()` scales from.
        log_call(value, value, lever, true);
        return value;
    }

    // Recorded on both paths below, the neutral one included. `apply()` scales and restores
    // from the game's own last request, so a record written only on the scaling path goes
    // stale the moment the game writes while the lever is neutral, and the next config change
    // would raise a request the game no longer holds.
    GAME_REQUESTED.store(value.to_bits(), Ordering::Release);

    if lever == 1.0 {
        // Neutral fast exit: at the shipped default this hook has nothing to change, so the
        // value goes to the original as it came, with no scaling, no ceiling arithmetic and no
        // lock.
        log_call(value, value, lever, false);
        return value;
    }

    // Everything the game writes goes through the one shared ceiling in AnimationSpeed:
    // its 0 pauses and its sub-1 slow motion stay as the game asked for, a value above 1.0 is
    // raised only up to MAX_TIME_SCALE even when a story getter already scaled it and this lever
    // scales it again, and a scale the game already holds above that ceiling is left where the
    // game put it - `apply_time_scale` never returns a value below the one it was handed, so
    // this hook cannot lower what the game asked for.
    let scaled = AnimationSpeed::apply_time_scale(value, lever);

    // The number the game now holds *from this layer*, for the echo check above and the
    // never-lower guard in `plan_write`.
    PRODUCED.store(scaled.to_bits(), Ordering::Release);

    log_call(value, scaled, lever, false);

    scaled
}

def_detour! {
    set_timeScale(value: f32) {
            // The lever is the atomic mirror `AnimationSpeed` keeps for hot paths, not a config load:
        // the config is read once per pass by `refresh_time_scale` / `refresh_config_mirrors`.
        let lever = AnimationSpeed::time_scale();

        if APPLYING.load(Ordering::Acquire) {
            // Our own write from `apply()`. It is clamped where the lever is read, so it
            // passes through instead of being multiplied a second time. Not counted: this is the
            // mod's write, and the counter is the answer to "did the game reach this hook".
            call_original(value);
            return;
        }

        call_original(scale_game_write(value, lever));
    }
}

/// The value Unity is holding right now, read through the game's own getter. NAN when
/// this build does not expose the getter.
fn read_time_scale() -> f32 {
    let addr = unsafe { GET_TIME_SCALE_ADDR };
    if addr == 0 {
        return f32::NAN;
    }

    unsafe {
        let get_time_scale: extern "C" fn() -> f32 = std::mem::transmute(addr);
        get_time_scale()
    }
}

/// The decision `apply()` makes, kept free of il2cpp so the sequence is testable.
///
/// `game_value` is what the game holds, `current` is what Unity is holding (NAN when it
/// cannot be read), and `current_is_ours` says whether that current is a number this layer
/// itself wrote (`PRODUCED`) - the only case a value below the game's current may be written.
/// `None` means leave the game alone, and that covers every case the overwrite used to break: a
/// game value at or below 1.0 is its 0 pause, its slow motion or its neutral 1.0, and none of
/// them is ours to replace with a configured number. A value above 1.0 is the game's own fast
/// forward: the lever raises it through the same arithmetic the write hook uses - which never
/// returns below the value it was handed - bounded by MAX_TIME_SCALE, instead of swapping in the
/// configured value - which for a lever below the game's own number would have slowed the fast
/// forward down. A number below Unity's current is only ever the restore, down to the game's own
/// last request, and only when the current is one of this layer's writes: a scale the game
/// reached on its own is left exactly where the game put it.
fn plan_write(game_value: f32, lever: f32, current: f32, current_is_ours: bool) -> Option<f32> {
    if !game_value.is_finite() || !lever.is_finite() || game_value <= 1.0 {
        return None;
    }

    let desired = AnimationSpeed::apply_time_scale(game_value, lever);

    if !desired.is_finite() || desired == current {
        return None;
    }

    if desired < current && !current_is_ours {
        warn!("Time::apply: the game holds {current}, which this layer did not write; not lowering it to {desired}");
        return None;
    }

    Some(desired)
}

/// The applied marker gate `apply()` runs. The first call for a lever value takes it; every later
/// call for the same value - a game tick, a view change, game initialisation - has nothing left to
/// write and leaves whatever the game is holding alone. Held apart from the game reads it gates so
/// the marker rule the game runs is the rule the tests run.
fn claim_lever(lever: f32) -> bool {
    if f32::from_bits(APPLIED_LEVER.load(Ordering::Acquire)) == lever {
        return false;
    }

    APPLIED_LEVER.store(lever.to_bits(), Ordering::Release);

    true
}

/// Push the configured lever into the game, once per config change.
///
/// The lever is a multiplier on what the game itself puts into `Time.timeScale`, and the
/// hook above already applies it to every write the game makes, so this does not re-assert
/// a value: it re-derives the one value the current config calls for, from the number the
/// game last asked for, and only when the game is holding something above its neutral 1.0.
/// Unity keeps `Time.timeScale` across scene loads, so a view change has nothing to fix;
/// what needs one pass is a change of the lever, which is why the marker below gates it.
///
/// The lever comes from `AnimationSpeed`, clamped to MIN_TIME_SCALE..=MAX_TIME_SCALE, and
/// `plan_write` bounds the value handed to the game by the same ceiling. At the neutral
/// lever with nothing previously scaled this writes nothing at all, so the game's own use
/// of timeScale (pauses, slow motion) is left untouched, and dropping the lever back to
/// 1.0 writes the game's own value once. The two `PRODUCED` reads - the echo check the hook
/// runs on every game write, and the current-is-ours gate here - are what make the layer
/// provably unable to multiply its own write twice, and unable to write under a scale the
/// game reached on its own.
pub fn apply() {
    let addr = unsafe { SET_TIME_SCALE_ADDR };
    if addr == 0 {
        return;
    }

    let lever = AnimationSpeed::refresh_time_scale();

    // Applied marker: one pass per config value. Every later call - the game
    // initialisation, a game tick, a call from anywhere - sees the lever it is about to
    // write and leaves the game's current value alone.
    if !claim_lever(lever) {
        return;
    }

    let current = read_time_scale();
    let requested = f32::from_bits(GAME_REQUESTED.load(Ordering::Acquire));
    let game_value = if requested.is_finite() { requested } else { current };

    // Unity's current counts as one of ours only while it matches the number this layer last
    // wrote. That is what lets the restore write the game's own value back over our own scaled
    // one, and what makes every other current - a scale the game reached on its own - a value
    // this layer will not write under.
    let ours = f32::from_bits(PRODUCED.load(Ordering::Acquire)) == current;

    let Some(desired) = plan_write(game_value, lever, current, ours) else {
        return;
    };

    info!("Time::apply: game {game_value} x lever {lever} -> {desired}");

    unsafe {
        // Recorded as ours before the write reaches the game: the detour passes an APPLYING
        // write through without re-scaling it, and the number this write leaves behind is one
        // the echo check may pass back through, and the restore may write under, for exactly
        // that reason.
        PRODUCED.store(desired.to_bits(), Ordering::Release);
        APPLYING.store(true, Ordering::Release);
        let set_time_scale: extern "C" fn(value: f32) = std::mem::transmute(addr);
        set_time_scale(desired);
        APPLYING.store(false, Ordering::Release);
    }
}

/// What the game's clock reads at the moment someone asks, for a census line that has to state the
/// value rather than a peak. NAN when this build does not expose the getter.
pub fn time_scale_now() -> f32 {
    read_time_scale()
}

/// Called from the overlay when the config is saved; the actual write happens on the
/// next game-thread tick in `GameSystem::GameSystem_Update`.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Release);
}

pub fn apply_if_dirty() {
    if DIRTY.swap(false, Ordering::AcqRel) {
        apply();
    }
}

pub fn init(_UnityEngine_CoreModule: *const Il2CppImage) {
    let set_timeScale_addr = il2cpp_resolve_icall(
        c"UnityEngine.Time::set_timeScale(System.Single)".as_ptr()
    );

    unsafe { GET_TIME_SCALE_ADDR = il2cpp_resolve_icall(
        c"UnityEngine.Time::get_timeScale()".as_ptr()
    ); }

    unsafe { SET_TIME_SCALE_ADDR = set_timeScale_addr; }

    if set_timeScale_addr == 0 {
        error!("Failed to resolve UnityEngine.Time::set_timeScale, time scaling is unavailable on this build");
        return;
    }

    if unsafe { GET_TIME_SCALE_ADDR } == 0 {
        warn!("Failed to resolve UnityEngine.Time::get_timeScale, a config change writes without reading the game's current value");
    }

    new_hook!(set_timeScale_addr, set_timeScale);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Where `Time.timeScale` stands in this process: `unity` is the value the game holds,
    // `writes` the values a pass handed it. Nothing here re-implements a decision: `game_write`
    // calls `scale_game_write`, the function the detour runs for every value the game writes, and
    // `apply` calls `claim_lever` and `plan_write`, the two functions `apply` itself runs, with
    // the same `PRODUCED` marker `apply` reads and writes around its icall. The one part of
    // `apply` a test process cannot run is `read_time_scale` and the icall write it ends with,
    // both of which go through the game's own getter and setter, so `unity` stands in for the
    // read and what a run has to confirm is written in the ledger.
    struct Game {
        // The turn this case is running in, held for as long as the fixture lives.
        _turn: std::sync::MutexGuard<'static, ()>,
        unity: f32,
        writes: Vec<f32>,
    }

    impl Game {
        fn new() -> Self {
            Self { _turn: hook_turn(), unity: 1.0, writes: Vec::new() }
        }

        // The detour half, run as the hook runs it.
        fn game_write(&mut self, value: f32, lever: f32) {
            self.unity = scale_game_write(value, lever);
        }

        // The `apply` half, with the icall read and the icall write left out.
        fn apply(&mut self, lever: f32) {
            if !claim_lever(lever) {
                return;
            }

            let current = self.unity;
            let requested = f32::from_bits(GAME_REQUESTED.load(Ordering::Acquire));
            let game_value = if requested.is_finite() { requested } else { current };
            let ours = f32::from_bits(PRODUCED.load(Ordering::Acquire)) == current;

            if let Some(desired) = plan_write(game_value, lever, current, ours) {
                PRODUCED.store(desired.to_bits(), Ordering::Release);
                self.writes.push(desired);
                self.unity = desired;
            }
        }
    }

    // The hook's state is process wide and `cargo test` runs cases on several threads, so the
    // cases that drive it take turns and each starts from the state a fresh process holds: no
    // game request recorded, no lever applied, counter at zero.
    static HOOK_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn hook_turn() -> std::sync::MutexGuard<'static, ()> {
        let turn = HOOK_TEST_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

        GAME_REQUESTED.store(f32::NAN.to_bits(), Ordering::Release);
        APPLIED_LEVER.store(f32::NAN.to_bits(), Ordering::Release);
        PRODUCED.store(f32::NAN.to_bits(), Ordering::Release);
        CALLS.store(0, Ordering::Relaxed);

        turn
    }

    #[test]
    fn a_game_pause_survives_the_option_being_on() {
        let lever = 2.0;
        let mut game = Game::new();

        game.game_write(1.0, lever);   // the game's neutral write at boot
        game.apply(lever);             // game initialisation
        game.game_write(0.0, lever);   // the game pauses
        for _ in 0..5 {
            game.apply(lever);         // five view changes with the option still on
        }

        assert_eq!(game.writes, Vec::<f32>::new(), "the configured lever was written over the game's pause");
        assert_eq!(game.unity, 0.0, "the pause was replaced by the configured value");
    }

    #[test]
    fn a_game_slow_motion_survives_the_option_being_on() {
        let lever = 3.0;
        let mut game = Game::new();

        game.game_write(0.5, lever);   // slow motion
        game.apply(lever);

        assert_eq!(game.writes, Vec::<f32>::new());
        assert_eq!(game.unity, 0.5);
    }

    #[test]
    fn a_game_fast_forward_is_raised_not_replaced() {
        let mut game = Game::new();

        game.game_write(4.0, 1.0);     // the game's own fast forward, lever still neutral
        assert_eq!(game.unity, 4.0);

        game.apply(2.0);               // the option goes on after the game chose 4.0
        assert_eq!(game.writes, vec![5.0], "expected the game's 4.0 raised to the ceiling");
        assert!(game.unity >= 4.0, "the game's fast forward was slowed to the configured value");
        assert!(game.unity <= AnimationSpeed::MAX_TIME_SCALE);
    }

    #[test]
    fn one_pass_per_config_change_not_one_per_view_change() {
        let lever = 2.0;
        let mut game = Game::new();

        game.game_write(4.0, 1.0);
        game.apply(lever);
        for _ in 0..20 {
            game.apply(lever);         // twenty view changes / ticks, same config
        }

        assert_eq!(game.writes, vec![5.0], "the same config value was written {} times", game.writes.len());
    }

    #[test]
    fn dropping_the_lever_restores_the_games_own_value() {
        let mut game = Game::new();

        game.game_write(4.0, 1.0);
        game.apply(2.0);
        assert_eq!(game.unity, 5.0);

        game.apply(1.0);               // option turned back off
        assert_eq!(game.writes, vec![5.0, 4.0]);
        assert_eq!(game.unity, 4.0, "the scaled value was left behind instead of the game's 4.0");

        game.apply(1.0);               // turning it off twice writes once
        assert_eq!(game.writes, vec![5.0, 4.0]);
    }

    #[test]
    fn a_lower_lever_lowers_the_write_instead_of_compounding_our_own() {
        let mut game = Game::new();

        game.game_write(4.0, 1.2);     // hook scaled the game's write: 4 -> 4.8
        assert_eq!(game.unity, 4.8);

        game.apply(1.05);              // read back and multiplied, this would rise to 5.0
        assert_eq!(game.writes, vec![4.2]);
        assert_eq!(game.unity, 4.2);
    }

    // C40. A lever below 1.0 is not a slow down: `AnimationSpeed` clamps the lever to
    // MIN_TIME_SCALE..=MAX_TIME_SCALE and `apply_time_scale` refuses a factor at or below its
    // neutral 1.0, so neither the hook nor the config write puts anything under where the game
    // put it. Every lever in the tests above sat at or above 1.0; this is the case they missed.
    #[test]
    fn a_lever_below_one_does_not_slow_the_games_fast_forward() {
        let mut game = Game::new();

        game.game_write(4.0, 1.0);      // the game's own fast forward, lever neutral
        assert_eq!(game.unity, 4.0);

        game.game_write(4.0, 0.1);      // the lever dragged to the Config Editor's old left end
        assert_eq!(game.unity, 4.0, "a lever of 0.1 multiplied the game's 4.0 down to 0.4");

        game.apply(0.1);                // and the deferred config write for that same lever
        assert_eq!(game.writes, Vec::<f32>::new(), "apply wrote a slowed value into the game");
        assert_eq!(game.unity, 4.0, "the game's fast forward was replaced by a slow motion");

        assert_eq!(plan_write(4.0, 0.1, 4.0, false), None);
        assert_eq!(plan_write(4.0, 0.5, 4.0, false), None);
        assert_eq!(plan_write(2.0, 0.9, 2.0, false), None);
    }

    #[test]
    fn the_neutral_lever_writes_nothing_at_all() {
        let mut game = Game::new();

        game.apply(1.0);
        for _ in 0..5 {
            game.apply(1.0);
        }

        assert_eq!(game.writes, Vec::<f32>::new());
        assert_eq!(game.unity, 1.0);
    }

    #[test]
    fn the_neutral_fast_exit_still_records_the_games_request() {
        // The neutral branch is a fast exit, but it is still the place the game's own request
        // is recorded. Dropping the record there is the trap this branch has to avoid: `apply()`
        // keeps an older request than the game holds, and the next config change raises a value
        // the game no longer asked for.
        let mut game = Game::new();

        game.game_write(4.0, 2.0);     // lever on: 4 -> 5, request 4 recorded
        game.apply(1.0);               // lever back to neutral: the game's own 4.0 is restored
        assert_eq!(game.writes, vec![4.0]);

        game.game_write(1.0, 1.0);     // a neutral write taken by the fast exit
        assert_eq!(game.unity, 1.0);

        game.apply(2.0);               // lever on again
        assert_eq!(game.writes, vec![4.0], "the lever scaled a request the game no longer held");
        assert_eq!(game.unity, 1.0, "a game sitting at 1.0 was pushed to the configured lever");

        // What the stale record would have written instead, had the fast exit skipped it.
        assert_eq!(plan_write(4.0, 2.0, 1.0, false), Some(5.0));
    }

    // Captures what the counter actually writes to the log. "Installed is not the same as
    // called" (A4) is only closed if a run at the shipped default can see the line at all.
    static CAPTURED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Capture;

    impl log::Log for Capture {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            let line = record.args().to_string();
            CAPTURED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(line);
        }

        fn flush(&self) {}
    }

    static CAPTURE: Capture = Capture;

    #[test]
    fn the_neutral_default_reports_that_the_hook_was_reached() {
        let _turn = hook_turn();

        let _ = log::set_logger(&CAPTURE);
        log::set_max_level(log::LevelFilter::Trace);

        CAPTURED.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clear();

        // Ten neutral calls, run through `scale_game_write`: the function the detour runs for a
        // value the game wrote, at the lever a shipped build holds.
        for index in 0..CALL_DETAIL_LIMIT + 4 {
            assert_eq!(scale_game_write(1.0, 1.0), 1.0, "the neutral fast exit changed the game's value");
            assert_eq!(CALLS.load(Ordering::Relaxed), index + 1, "the counter missed a call");
        }

        let captured = CAPTURED.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let lines: Vec<&String> = captured.iter().filter(|line| line.contains("Time::set_timeScale")).collect();

        assert_eq!(lines.len(), CALL_DETAIL_LIMIT, "expected the first {CALL_DETAIL_LIMIT} calls to be logged, got {}", lines.len());
        assert!(lines.iter().all(|line| line.contains("1 -> 1 (lever x1)")), "the neutral line does not show a value passing through: {lines:?}");
    }

    #[test]
    fn claim_lever_holds_one_pass_per_lever_value() {
        let _turn = hook_turn();

        assert!(claim_lever(2.0), "the first pass for a lever did not take it");

        for _ in 0..20 {
            assert!(!claim_lever(2.0), "a view change re-ran the pass for the same lever");
        }

        // A different lever value is a different config change, including the one that turns
        // the option back off and has to reach the game to put its own value back.
        assert!(claim_lever(1.0), "turning the option off could not run its restore pass");
        assert!(!claim_lever(1.0));
        assert!(claim_lever(3.0), "a new lever value was marked as already applied");
    }

    #[test]
    fn plan_write_leaves_every_game_value_at_or_below_one_alone() {
        assert_eq!(plan_write(0.0, 2.0, 0.0, false), None, "pause");
        assert_eq!(plan_write(0.5, 3.0, 0.5, false), None, "slow motion");
        assert_eq!(plan_write(1.0, 2.0, 1.0, false), None, "the game's neutral 1.0");
        assert_eq!(plan_write(1.0, 2.0, 5.0, false), None, "not ours to reset");
        assert_eq!(plan_write(f32::NAN, 2.0, 4.0, false), None, "nothing known");
        assert_eq!(plan_write(4.0, 2.0, 4.0, false), Some(5.0), "already at the ceiling");
        assert_eq!(plan_write(2.0, 2.0, 2.0, false), Some(4.0));
        assert_eq!(plan_write(4.0, 1.0, 5.0, true), Some(4.0), "restore our own write to the game's own value");
        assert_eq!(plan_write(4.0, 1.0, 5.0, false), None, "the restore lowered a 5.0 the game held itself");
    }

    // The two writers of Time.timeScale share `AnimationSpeed::apply_time_scale`, so both take
    // the never lower floor the read half has: a scale the game already holds above
    // MAX_TIME_SCALE passes through instead of being pulled back to the ceiling.
    #[test]
    fn a_game_scale_above_the_ceiling_is_not_pulled_down() {
        let mut game = Game::new();

        game.game_write(8.0, 2.0);     // the game's own 8.0 with the lever on
        assert_eq!(game.unity, 8.0, "the detour wrote the ceiling over the game's 8.0");

        game.apply(2.0);               // and the deferred config write for that same lever
        assert_eq!(game.writes, Vec::<f32>::new(), "apply wrote 5.0 over the game's 8.0");
        assert_eq!(game.unity, 8.0);

        assert_eq!(plan_write(8.0, 2.0, 8.0, false), None, "the plan lowered the game's own scale");
        assert_eq!(scale_game_write(8.0, 5.0), 8.0);

        // The ceiling still binds every raise the mod itself makes, so nothing compounds.
        assert_eq!(scale_game_write(4.0, 5.0), 5.0, "the ceiling stopped binding the mod's own raise");
        assert_eq!(scale_game_write(5.0, 5.0), 5.0, "a raise at the ceiling compounded");
    }

    // C12's other half: a ceiling makes a multiplied-twice value bounded, but a bounded
    // multiply-twice is still the read-modify-write compounding C22/C24/C35 exist to refuse
    // (AGENTS section 5: remember the original, never multiply the current value). The game
    // reading `Time.timeScale` back hands the hook its own scaled write; the `PRODUCED` marker
    // is what lets the hook see that and pass it through instead of multiplying the lever a
    // second time.
    #[test]
    fn a_write_the_game_hands_back_is_not_multiplied_a_second_time() {
        let lever = 1.2;
        let mut game = Game::new();

        game.game_write(2.0, lever);   // the hook raised the game's 2.0 to 2.4
        assert_eq!(game.unity, 2.4, "the first raise did not happen");

        game.game_write(2.4, lever);   // the game read `Time.timeScale` back and wrote what it saw
        assert_eq!(game.unity, 2.4, "our own 2.4 was multiplied again to {}", game.unity);

        for _ in 0..10 {
            game.game_write(2.4, lever); // ten more read-modify-write rounds
        }

        assert_eq!(game.unity, 2.4, "the lever compounded the layer's own write to {}", game.unity);

        // And the baseline a restore scales from is still the game's raw request, not the echo:
        // recording the echo as the game's request is what would compound the next pass.
        game.apply(1.0);               // turning the option back off
        assert_eq!(game.writes, vec![2.0], "the restore wrote the scaled echo, not the game's 2.0");
        assert_eq!(game.unity, 2.0);
    }

    // Runs 19 to 21 hold `Time.timeScale` at 1.0 while the training cut's own clock peaks at
    // 8.334 in values this layer never wrote. A config change whose restore or raise lands
    // below such a current must leave it alone: the layer may write under `Time.timeScale` only
    // over its own number.
    #[test]
    fn a_restore_never_lowers_a_scale_the_game_reached_on_its_own() {
        let mut game = Game::new();

        game.game_write(4.0, 1.2);     // the layer's own write is 4.8
        assert_eq!(game.unity, 4.8);

        // The game then stands `Time.timeScale` at a number this layer did not write.
        game.unity = 8.0;

        game.apply(1.05);              // a config change for a lever above 1.0
        assert_eq!(game.writes, Vec::<f32>::new(), "apply wrote {:?} over the game's own 8.0", game.writes);
        assert_eq!(game.unity, 8.0, "the layer lowered the game's own scale under its record");

        // The guard is on the decision itself, not only on the sequence above.
        assert_eq!(plan_write(4.0, 1.0, 8.0, false), None, "the restore lowered the game's 8.0 to 4.0");
        assert_eq!(plan_write(4.0, 2.0, 8.0, false), None, "a raise derived from an old record undercut a game 8.0");
        assert_eq!(plan_write(4.0, 1.0, 4.8, true), Some(4.0), "our own 4.8 could not be restored to the game's 4.0");
    }
}
