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

/// Addresses of the icall implementations. 0 when this build does not expose them, in
/// which case every entry point here stays inert instead of calling through a null pointer.
static mut SET_TIME_SCALE_ADDR: usize = 0;
static mut GET_TIME_SCALE_ADDR: usize = 0;

type SetTimeScaleFn = extern "C" fn(value: f32);

/// The trampoline of the original icall, resolved once and kept here. `get_orig_fn!` is a
/// `Mutex<FnvHashMap>` lookup with an `unwrap()` (C33), so calling the original that way
/// puts a lock - and a poisoned lock waiting to panic across an `extern "C"` frame (C2) - on
/// every value the game writes into `Time.timeScale`. Cached, the neutral path is one atomic
/// load and a jump.
///
/// The cache cannot go stale underneath us: a detour is only reachable through its own
/// trampoline, so the moment the hook is removed nothing routes into the code that would read
/// the cached address any more. It stays 0 until the first armed call has resolved it, which
/// also keeps `init` out of the hook map while `begin_batch`/`finish_batch` are arming.
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

fn log_call(value: f32, scaled: f32, lever: f32) {
    let calls = CALLS.fetch_add(1, Ordering::Relaxed) + 1;

    if calls <= CALL_DETAIL_LIMIT {
        debug!("Time::set_timeScale call {calls}: {value} -> {scaled} (lever x{lever})");
    }
    else if calls % CALL_CHUNK == 0 {
        debug!("Time::set_timeScale {calls} calls, most recent {value} -> {scaled} (lever x{lever})");
    }
}

/// What the hook does with one value the game wrote: record it as the game's own request, and
/// hand back the value to pass on. Held apart from the detour and from the jump to the original
/// so the neutral fast exit and the scaling branch are the shipped code the tests run, instead of
/// a test module repeating them. `set_timeScale` reads the mirror and hands the lever in, so this
/// half needs no config and reaches no game pointer.
fn scale_game_write(value: f32, lever: f32) -> f32 {
    // Recorded on both paths below, the neutral one included. `apply()` scales and restores
    // from the game's own last request, so a record written only on the scaling path goes
    // stale the moment the game writes while the lever is neutral, and the next config change
    // would raise a request the game no longer holds.
    GAME_REQUESTED.store(value.to_bits(), Ordering::Release);

    if lever == 1.0 {
        // Neutral fast exit: at the shipped default this hook has nothing to change, so the
        // value goes to the original as it came, with no scaling, no ceiling arithmetic and no
        // lock.
        log_call(value, value, lever);
        return value;
    }

    // Everything the game writes goes through the one shared ceiling in AnimationSpeed:
    // its 0 pauses and its sub-1 slow motion stay as the game asked for, a value above 1.0 is
    // raised only up to MAX_TIME_SCALE even when a story getter already scaled it and this lever
    // scales it again, and a scale the game already holds above that ceiling is left where the
    // game put it.
    let scaled = AnimationSpeed::apply_time_scale(value, lever);
    log_call(value, scaled, lever);

    scaled
}

extern "C" fn set_timeScale(value: f32) {
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
/// cannot be read). `None` means leave the game alone, and that covers every case the
/// overwrite used to break: a game value at or below 1.0 is its 0 pause, its slow motion
/// or its neutral 1.0, and none of them is ours to replace with a configured number. A
/// value above 1.0 is the game's own fast forward: the lever raises it through the same
/// arithmetic the write hook uses, bounded by MAX_TIME_SCALE, instead of swapping in the
/// configured value - which for a lever below the game's own number would have slowed
/// the fast forward down.
fn plan_write(game_value: f32, lever: f32, current: f32) -> Option<f32> {
    if !game_value.is_finite() || !lever.is_finite() || game_value <= 1.0 {
        return None;
    }

    let desired = AnimationSpeed::apply_time_scale(game_value, lever);

    if !desired.is_finite() || desired == current {
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
/// 1.0 writes the game's own value once.
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

    let Some(desired) = plan_write(game_value, lever, current) else {
        return;
    };

    info!("Time::apply: game {game_value} x lever {lever} -> {desired}");

    unsafe {
        APPLYING.store(true, Ordering::Release);
        let set_time_scale: extern "C" fn(value: f32) = std::mem::transmute(addr);
        set_time_scale(desired);
        APPLYING.store(false, Ordering::Release);
    }
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
    // `apply` calls `claim_lever` and `plan_write`, the two functions `apply` itself runs. The one
    // part of `apply` a test process cannot run is `read_time_scale` and the icall write it ends
    // with, both of which go through the game's own getter and setter, so `unity` stands in for
    // the read and what a run has to confirm is written in the ledger.
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

            if let Some(desired) = plan_write(game_value, lever, current) {
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

        assert_eq!(plan_write(4.0, 0.1, 4.0), None);
        assert_eq!(plan_write(4.0, 0.5, 4.0), None);
        assert_eq!(plan_write(2.0, 0.9, 2.0), None);
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
        assert_eq!(plan_write(4.0, 2.0, 1.0), Some(5.0));
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
        assert_eq!(plan_write(0.0, 2.0, 0.0), None, "pause");
        assert_eq!(plan_write(0.5, 3.0, 0.5), None, "slow motion");
        assert_eq!(plan_write(1.0, 2.0, 1.0), None, "the game's neutral 1.0");
        assert_eq!(plan_write(1.0, 2.0, 5.0), None, "not ours to reset");
        assert_eq!(plan_write(f32::NAN, 2.0, 4.0), None, "nothing known");
        assert_eq!(plan_write(4.0, 2.0, 4.0), Some(5.0), "already at the ceiling");
        assert_eq!(plan_write(2.0, 2.0, 2.0), Some(4.0));
        assert_eq!(plan_write(4.0, 1.0, 5.0), Some(4.0), "restore the game's own value");
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

        assert_eq!(plan_write(8.0, 2.0, 8.0), None, "the plan lowered the game's own scale");
        assert_eq!(scale_game_write(8.0, 5.0), 8.0);

        // The ceiling still binds every raise the mod itself makes, so nothing compounds.
        assert_eq!(scale_game_write(4.0, 5.0), 5.0, "the ceiling stopped binding the mod's own raise");
        assert_eq!(scale_game_write(5.0, 5.0), 5.0, "a raise at the ceiling compounded");
    }
}
