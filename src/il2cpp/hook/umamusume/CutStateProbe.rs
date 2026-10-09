use std::ffi::CStr;
use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicPtr, AtomicU64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::AnimationSpeed,
        symbols::{get_field_from_name, get_field_ptr},
        types::*,
    },
};

use super::TrainingCuttProbe::{class_for_label, peak_seconds, report_due, CutProbe, PROBE_DETAIL_LIMIT, REPORT_INTERVAL_SECS};

// Eyes on the coroutine a training turn waits on.
//
// Runs 16 and 17 both left the same hole unmoved: a training cut that took 7 to 8 s with the fork's own
// duration doors already reached, and run 16's open cut stretched to 90 s. What neither run could say is
// *what the game was waiting for*. The durations this fork hands the game (2.4 to 0.120000005 at
// `PlayIn`) are only half of a training cut, and the other half is a number the game computed itself and
// has never been read (C58).
//
// The state machine the C# compiler builds for
// `SingleModeMainTrainingCuttController::PlayTrainingCut` holds that number, and this client's dump names
// it together with the locals it captured:
//
//   <PlayTrainingCut>d__70::field <timeScale>5__7 [float]
//   <PlayTrainingCut>d__70::field <allTextWaitTime>5__12 [float]
//   <PlayTrainingCut>d__70::field <isHighSpeedOnStart>5__9 [bool]
//   <PlayTrainingCut>d__70::field <waitForFixedUpdate>5__10 [class<UnityEngine.WaitForFixedUpdate>]
//
// So a probe standing on that class reads the game's own timing instead of inferring it from wall clock
// (A29), and a door on its `MoveNext` says which branch of the coroutine the game is sitting in, how long
// it sat there, and whether the driver kept calling it at all. Those three answers are what decides
// whether the hole is a wait the fork can shorten or a round trip it must not touch (C3, C31).
//
// Everything here is observe only. The wrappers hand the original its arguments and its return value back
// and write nothing, so a measurement made on a value cannot change it. A field handle is only used after
// it was resolved by name against the class this probe resolved, and the object it is read from is checked
// against that class first.
//
// Installed only when debug_mode is on, like TrainingCuttProbe, and after the scaling modules so the class
// lookup is the one they resolved.

// The class exactly as this client names it. `get_class` wants the bare name for a type with no namespace,
// which is how a compiler generated state machine is stored.
const MACHINE_NAME: &str = "<PlayTrainingCut>d__70";
const TIME_SCALE_NAME: &CStr = c"<timeScale>5__7";
const ALL_TEXT_WAIT_NAME: &CStr = c"<allTextWaitTime>5__12";
const HIGH_SPEED_NAME: &CStr = c"<isHighSpeedOnStart>5__9";
// The branch marker every C# state machine carries. It is private, so no dump filter has ever printed it,
// and the probe reports a miss rather than guessing an offset.
const STATE_NAME: &CStr = c"__state";

const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];

// A state machine's branch marker starts at -1 before its first move and ends at -2 or lower when it is
// finished, so no real state is this value and it is safe to keep as "nothing sampled yet".
pub(crate) const NO_STATE: i32 = i32::MIN;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Held {
    // The branch that was current until this call.
    pub state: i32,
    // How long it had been current when this call arrived.
    pub held_ms: i64,
    // Whether this call left that branch.
    pub changed: bool,
}

// What one `MoveNext` says about the coroutine: the branch it found the coroutine in, how long that branch
// had been held, and whether the branch just changed. The first sample of a machine has nothing behind it,
// so it reports nothing, and a clock that went backwards is refused rather than turned into a hold.
pub(crate) fn hold_of(previous_state: i32, held_since_ms: i64, now_ms: i64, state: i32) -> Option<Held> {
    if previous_state == NO_STATE || held_since_ms < 0 || now_ms < held_since_ms {
        return None;
    }

    Some(Held { state: previous_state, held_ms: now_ms - held_since_ms, changed: previous_state != state })
}

// One bit per branch. A training cut walks a handful of branches, and a door the engine calls every frame
// it is playing one cannot afford a set. The markers a machine reports before its first move (-1) and once
// it is done (-2) are not branches a cut walked, so they get the top bits of the word and cannot be
// confused with a real branch number. A branch number past the 61 bits left for them shares the last one;
// the count of distinct branches a cut walked is a coarse number either way, and the branch numbers a run
// reports are the exact ones.
pub(crate) fn state_bit(state: i32) -> u64 {
    match state {
        -1 => 1u64 << 63,
        -2 => 1u64 << 62,
        state if state < 0 => 1u64 << 61,
        state => 1u64 << state.min(60) as u32,
    }
}

pub(crate) fn states_seen(bits: u64) -> usize {
    bits.count_ones() as usize
}

// The gap between two `MoveNext` calls. A coroutine that Unity is driving advances every frame, so a gap
// of whole seconds says the driver stopped calling it, which is a different problem from a coroutine that
// is running and re-entering the same wait every frame.
pub(crate) fn step_gap(previous_ms: i64, now_ms: i64) -> Option<i64> {
    if previous_ms < 0 || now_ms < previous_ms {
        return None;
    }

    Some(now_ms - previous_ms)
}

static START: OnceLock<Instant> = OnceLock::new();

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

static MOVENEXT: CutProbe = CutProbe::counted("<PlayTrainingCut>d__70::MoveNext()");
static TIME_SCALE: CutProbe = CutProbe::peaked("PlayTrainingCut timeScale");
static ALL_TEXT_WAIT: CutProbe = CutProbe::peaked("PlayTrainingCut allTextWaitTime");

// The largest value the game itself put into the cut's own timing fields this run.
pub(crate) fn time_scale_peak() -> f32 {
    peak_seconds(TIME_SCALE.peak_bits())
}

pub(crate) fn all_text_wait_peak() -> f32 {
    peak_seconds(ALL_TEXT_WAIT.peak_bits())
}

static STATE_CHANGES: AtomicUsize = AtomicUsize::new(0);
static STATES_SEEN: AtomicU64 = AtomicU64::new(0);
static HOLDS: AtomicUsize = AtomicUsize::new(0);
static HOLD_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static HOLD_WORST_MS: AtomicI64 = AtomicI64::new(0);
static HOLD_WORST_STATE: AtomicI32 = AtomicI32::new(NO_STATE);
static LAST_STATE: AtomicI32 = AtomicI32::new(NO_STATE);
static LAST_CHANGE_MS: AtomicI64 = AtomicI64::new(-1);
static LAST_STEP_MS: AtomicI64 = AtomicI64::new(-1);
static STEP_GAP_WORST_MS: AtomicI64 = AtomicI64::new(0);
static STEP_GAP_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static STEP_GAP_RUNS: AtomicUsize = AtomicUsize::new(0);
static HIGH_SPEED_START_COUNT: AtomicUsize = AtomicUsize::new(0);
static FIELD_READS: AtomicUsize = AtomicUsize::new(0);
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);
// The machine class this probe resolved, and whether it has already reported a surprise. The snapshot is
// taken from a door that hands back a coroutine object, and a door that hands back a different class than
// the one these field handles belong to must say so instead of reading slots it has no claim on.
static MACHINE_CLASS: AtomicPtr<Il2CppClass> = AtomicPtr::new(std::ptr::null_mut());
static CLASS_MISMATCH_REPORTED: AtomicUsize = AtomicUsize::new(0);

static mut TIME_SCALE_FIELD: *mut FieldInfo = std::ptr::null_mut();
static mut ALL_TEXT_WAIT_FIELD: *mut FieldInfo = std::ptr::null_mut();
static mut HIGH_SPEED_FIELD: *mut FieldInfo = std::ptr::null_mut();
static mut STATE_FIELD: *mut FieldInfo = std::ptr::null_mut();

// A field handle this probe resolved by name on the class `obj` was made from is that object's own slot,
// so the read is one load and nothing has to be interpreted. Only float and bool instance fields are read
// this way; a struct field would need a layout this file does not have.
unsafe fn read_f32(obj: *mut Il2CppObject, field: *mut FieldInfo) -> Option<f32> {
    if obj.is_null() || field.is_null() {
        return None;
    }

    Some(unsafe { *get_field_ptr::<f32>(obj, field) })
}

unsafe fn read_bool(obj: *mut Il2CppObject, field: *mut FieldInfo) -> Option<bool> {
    if obj.is_null() || field.is_null() {
        return None;
    }

    Some(unsafe { *get_field_ptr::<bool>(obj, field) })
}

unsafe fn read_state(obj: *mut Il2CppObject) -> i32 {
    if obj.is_null() || STATE_FIELD.is_null() {
        return NO_STATE;
    }

    unsafe { *get_field_ptr::<i32>(obj, STATE_FIELD) }
}

// The values the game computed for this cut, read every frame the cut advances. A peak is enough: the
// question is how large the game's own number got, not what it was at one instant.
unsafe fn note_values(this: *mut Il2CppObject) {
    if let Some(value) = read_f32(this, TIME_SCALE_FIELD) {
        TIME_SCALE.sample(value);
    }

    if let Some(value) = read_f32(this, ALL_TEXT_WAIT_FIELD) {
        ALL_TEXT_WAIT.sample(value);
    }

    if let Some(flag) = read_bool(this, HIGH_SPEED_FIELD) {
        FIELD_READS.fetch_add(1, atomic::Ordering::Relaxed);

        if flag {
            HIGH_SPEED_START_COUNT.fetch_add(1, atomic::Ordering::Relaxed);
        }
    }
}

fn observe_step(this: *mut Il2CppObject) {
    MOVENEXT.count();

    let now = elapsed_ms();
    let state = unsafe { read_state(this) };
    let previous = LAST_STATE.load(atomic::Ordering::Relaxed);

    match hold_of(previous, LAST_CHANGE_MS.load(atomic::Ordering::Relaxed), now, state) {
        Some(held) => {
            HOLDS.fetch_add(1, atomic::Ordering::Relaxed);
            HOLD_MS_TOTAL.fetch_add(held.held_ms, atomic::Ordering::Relaxed);

            // A branch that never changes is the parked case, and its hold grows on every frame the
            // engine drives the coroutine, so the worst hold is kept without a second pass.
            if held.held_ms > HOLD_WORST_MS.load(atomic::Ordering::Relaxed) {
                HOLD_WORST_MS.store(held.held_ms, atomic::Ordering::Relaxed);
                HOLD_WORST_STATE.store(held.state, atomic::Ordering::Relaxed);
            }

            if held.changed {
                let changes = STATE_CHANGES.fetch_add(1, atomic::Ordering::Relaxed) + 1;
                LAST_CHANGE_MS.store(now, atomic::Ordering::Relaxed);

                if changes <= PROBE_DETAIL_LIMIT {
                    debug!(
                        "Cut state probe change {}: state {} held {} ms then {}, states seen {}",
                        changes,
                        held.state,
                        held.held_ms,
                        state,
                        states_seen(STATES_SEEN.load(atomic::Ordering::Relaxed) | state_bit(state))
                    );
                }
            }
        },
        None => LAST_CHANGE_MS.store(now, atomic::Ordering::Relaxed),
    }

    LAST_STATE.store(state, atomic::Ordering::Relaxed);
    STATES_SEEN.fetch_or(state_bit(state), atomic::Ordering::Relaxed);

    if let Some(gap) = step_gap(LAST_STEP_MS.load(atomic::Ordering::Relaxed), now) {
        STEP_GAP_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        STEP_GAP_MS_TOTAL.fetch_add(gap, atomic::Ordering::Relaxed);

        if gap > STEP_GAP_WORST_MS.load(atomic::Ordering::Relaxed) {
            STEP_GAP_WORST_MS.store(gap, atomic::Ordering::Relaxed);
        }
    }

    LAST_STEP_MS.store(now, atomic::Ordering::Relaxed);
}

type MoveNextFn = extern "C" fn(this: *mut Il2CppObject) -> bool;

// Dumped as `MoveNext/0 -> bool()` on `<PlayTrainingCut>d__70`. Unity drives a live coroutine by calling
// `MoveNext` on it, so this is the door that says whether the training cut's coroutine is advancing,
// where it is sitting, and what the game's own timing values are while it sits there.
extern "C" fn PlayTrainingCutStateMachine_MoveNext(this: *mut Il2CppObject) -> bool {
    observe_step(this);
    unsafe { note_values(this) };

    get_orig_fn!(PlayTrainingCutStateMachine_MoveNext, MoveNextFn)(this)
}

// Whether `obj` is the machine these field handles belong to. A door that hands back a coroutine from a
// different method reports the class it actually saw once, because that class is the next name this probe
// would need, and reading these offsets against it would be reading memory this probe has no claim on.
fn machine_is_the_one_resolved(obj: *mut Il2CppObject) -> bool {
    let machine = MACHINE_CLASS.load(atomic::Ordering::Relaxed);

    if obj.is_null() || machine.is_null() {
        return false;
    }

    let klass = unsafe { *(*obj).__bindgen_anon_1.klass.as_ref() };

    if klass == machine {
        return true;
    }

    if CLASS_MISMATCH_REPORTED.swap(1, atomic::Ordering::Relaxed) == 0 {
        let name = unsafe { std::ffi::CStr::from_ptr((*klass).name) }.to_string_lossy().into_owned();
        info!("Cut state probe: PlayTrainingCut handed back a {name} object, not {MACHINE_NAME}");
    }

    false
}

// Taken from `TrainingCuttProbe`'s own `PlayTrainingCut` door, on the coroutine object the game created.
// This is the value as the game set it up, before a frame of anything has run, which is the half the
// `MoveNext` peak cannot show when the machine only advanced once or twice.
pub(crate) fn note_play_training_cut(obj: *mut Il2CppObject) {
    if !machine_is_the_one_resolved(obj) {
        return;
    }

    unsafe {
        if let Some(value) = read_f32(obj, TIME_SCALE_FIELD) {
            debug!("Cut state probe cut opening timeScale {value:.4}");
        }

        if let Some(value) = read_f32(obj, ALL_TEXT_WAIT_FIELD) {
            debug!("Cut state probe cut opening allTextWaitTime {value:.4}");
        }

        if let Some(flag) = read_bool(obj, HIGH_SPEED_FIELD) {
            debug!("Cut state probe cut opening isHighSpeedOnStart {flag}");
        }
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());

    let class = match class_for_label(umamusume, MACHINE_NAME) {
        Some(class) => class,
        None => {
            info!("Cut state probe: {MACHINE_NAME} is not in this client, the training cut coroutine is not measured");
            return;
        }
    };

    MACHINE_CLASS.store(class, atomic::Ordering::Relaxed);

    unsafe {
        TIME_SCALE_FIELD = get_field_from_name(class, TIME_SCALE_NAME);
        ALL_TEXT_WAIT_FIELD = get_field_from_name(class, ALL_TEXT_WAIT_NAME);
        HIGH_SPEED_FIELD = get_field_from_name(class, HIGH_SPEED_NAME);
        STATE_FIELD = get_field_from_name(class, STATE_NAME);
    }

    let addr = unsafe { AnimationSpeed::resolve_method(class, "MoveNext", NO_PARAMS, BOOL) };

    if addr == 0 {
        info!("Cut state probe: {MACHINE_NAME} has no MoveNext with the dumped signature, only the opening values can be read");
        return;
    }

    new_hook!(addr, PlayTrainingCutStateMachine_MoveNext);

    let named = |field: *mut FieldInfo| if field.is_null() { "not found" } else { "resolved" };

    info!(
        "Cut state probe: standing on {MACHINE_NAME}::MoveNext, timeScale {}, allTextWaitTime {}, isHighSpeedOnStart {}, branch marker {}",
        unsafe { named(TIME_SCALE_FIELD) },
        unsafe { named(ALL_TEXT_WAIT_FIELD) },
        unsafe { named(HIGH_SPEED_FIELD) },
        unsafe { named(STATE_FIELD) }
    );
}

// Called from the GameSystem update tick beside the other probe reports. A cut that never opened prints
// nothing, which is the rule TrainingCuttProbe runs on.
pub fn report_if_due() {
    if START.get().is_none() {
        return;
    }

    let totals = MOVENEXT.calls();
    let now_sec = elapsed_ms() / 1000;

    if !report_due(now_sec, LAST_REPORT_SEC.load(atomic::Ordering::Relaxed), totals, LAST_TOTALS.load(atomic::Ordering::Relaxed), REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_TOTALS.store(totals, atomic::Ordering::Relaxed);

    let holds = HOLDS.load(atomic::Ordering::Relaxed);
    let hold_ms = HOLD_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let gaps = STEP_GAP_RUNS.load(atomic::Ordering::Relaxed);
    let mean = |total: i64, count: usize| match count {
        0 => 0.0,
        n => total as f64 / n as f64,
    };
    let hold_mean = mean(hold_ms, holds);
    let gap_mean = mean(STEP_GAP_MS_TOTAL.load(atomic::Ordering::Relaxed), gaps);
    let distinct = states_seen(STATES_SEEN.load(atomic::Ordering::Relaxed));
    let changes = STATE_CHANGES.load(atomic::Ordering::Relaxed);
    let worst_hold = HOLD_WORST_MS.load(atomic::Ordering::Relaxed);
    let worst_branch = HOLD_WORST_STATE.load(atomic::Ordering::Relaxed);
    let last_branch = LAST_STATE.load(atomic::Ordering::Relaxed);
    let worst_gap = STEP_GAP_WORST_MS.load(atomic::Ordering::Relaxed);
    let high_speed_reads = FIELD_READS.load(atomic::Ordering::Relaxed);
    let high_speed_on = HIGH_SPEED_START_COUNT.load(atomic::Ordering::Relaxed);

    // The hold that is still open: the engine has not moved the coroutine since the last time its branch
    // changed, and that stretch is not in the completed holds yet.
    let since_change = now_sec * 1000 - LAST_CHANGE_MS.load(atomic::Ordering::Relaxed);
    let open_hold = if since_change < 0 { 0 } else { since_change };

    info!(
        "Cut state probe training cut coroutine at {now_sec} s: MoveNext {totals} steps over {distinct} distinct branches, branch changes {changes}, branch holds {holds} mean {hold_mean:.1} ms worst {worst_hold} ms at branch {worst_branch}, still in branch {last_branch} for {open_hold} ms, gaps between steps {gaps}"
    );
    info!(
        "Cut state probe training cut values: timeScale peak {:.4}, allTextWaitTime peak {:.4}, isHighSpeedOnStart true in {high_speed_on} of {high_speed_reads} reads, worst gap between steps {worst_gap} ms mean {gap_mean:.1} ms",
        time_scale_peak(),
        all_text_wait_peak()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sample_of_a_machine_holds_nothing() {
        assert_eq!(hold_of(NO_STATE, -1, 1_000, 0), None);
        assert_eq!(hold_of(3, -1, 1_000, 3), None);
    }

    #[test]
    fn a_branch_that_stays_the_same_grows_its_hold() {
        let first = hold_of(0, 1_000, 2_500, 0).expect("a sample behind the first");

        assert_eq!(first.state, 0);
        assert_eq!(first.held_ms, 1_500);
        assert!(!first.changed);

        let grown = hold_of(0, 1_000, 8_266, 0).expect("a sample behind the first");

        assert_eq!(grown.held_ms, 7_266);
        assert!(!grown.changed);
    }

    #[test]
    fn a_branch_that_changes_charges_the_hold_to_the_branch_left_behind() {
        let moved = hold_of(4, 5_000, 8_400, 7).expect("a sample behind the first");

        assert!(moved.changed);
        assert_eq!(moved.state, 4);
        assert_eq!(moved.held_ms, 3_400);
    }

    #[test]
    fn a_clock_that_went_backwards_is_refused() {
        assert_eq!(hold_of(1, 9_000, 8_000, 1), None);
    }

    #[test]
    fn finished_and_unstarted_branch_markers_stay_in_the_word() {
        // A state machine reports -1 before its first move and -2 once it is done. A bare shift of those
        // values is undefined behaviour in C and a panic in a debug Rust build, so the markers take the top
        // bits of the word, where no branch a cut really walked can land.
        assert_eq!(state_bit(-1), 1u64 << 63);
        assert_eq!(state_bit(-2), 1u64 << 62);
        assert_eq!(state_bit(0), 1);
        assert_ne!(state_bit(-1), state_bit(63));
        assert_eq!(states_seen(state_bit(-1) | state_bit(-2)), 2);
        assert_eq!(states_seen(state_bit(0) | state_bit(3) | state_bit(-1)), 3);

        // A branch number past the room the word has shares its last bit rather than shifting past the end.
        assert_eq!(state_bit(61), state_bit(60));
    }

    #[test]
    fn a_gap_between_steps_is_measured_and_the_first_one_is_not() {
        assert_eq!(step_gap(-1, 4_000), None);
        assert_eq!(step_gap(4_000, 4_000), Some(0));
        assert_eq!(step_gap(1_000, 8_266), Some(7_266));
        assert_eq!(step_gap(8_266, 1_000), None);
    }

    #[test]
    fn the_report_waits_for_the_motion_and_for_the_interval() {
        assert!(!report_due(30, -1, 0, 0, REPORT_INTERVAL_SECS));
        assert!(report_due(30, -1, 12, 0, REPORT_INTERVAL_SECS));
        assert!(!report_due(35, 30, 18, 12, REPORT_INTERVAL_SECS));
        assert!(report_due(50, 30, 18, 12, REPORT_INTERVAL_SECS));
    }
}
