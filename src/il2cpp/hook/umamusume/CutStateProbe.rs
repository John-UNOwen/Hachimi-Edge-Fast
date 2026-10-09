use std::ffi::CStr;
use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::AnimationSpeed,
        symbols::{find_nested_class_by_prefix, get_field_from_name, get_field_ptr},
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
// `SingleModeMainTrainingCuttController::PlayTrainingCut` holds that number, and this client's dump prints
// it whole once its name is on the allowlist, branch marker and captured locals included:
//
//   === <PlayTrainingCut>d__70 ===
//     MoveNext/0 -> bool()
//     field <>1__state [int]
//     field <timeScale>5__7 [float]
//     field <allTextWaitTime>5__12 [float]
//     field <waitForFixedUpdate>5__10 [class<UnityEngine.WaitForFixedUpdate>]
//
// So a probe standing on that class reads the game's own timing instead of inferring it from wall clock
// (A29), and a door on its `MoveNext` says which branch of the coroutine the game is sitting in, how long
// it sat there, and whether the driver kept calling it at all. Those three answers are what decides
// whether the hole is a wait the fork can shorten or a round trip it must not touch (C3, C31).
//
// Run 19 read this door for the first time and named the hole: 4932 ms and 7866 ms between the training
// cut-in ending and the status play out on the two slow turns, against nothing on the four that played out
// at once. Its own branch numbers came back cross contaminated, because every training turn builds a new
// machine and the clock ran across all of them: `worst 65894 ms at branch 13` is a branch of a finished cut
// held through 66 s of menu, not a wait inside a cut. The clock is therefore windowed per cut, closed on the
// same door `cut run N closed` uses, and the stretch the hole clock measures is read off the coroutine too.
//
// Everything here is observe only. The wrappers hand the original its arguments and its return value back
// and write nothing, so a measurement made on a value cannot change it. A field handle is only used after
// it was resolved by name against the class this probe resolved, and the object it is read from is checked
// against that class first.
//
// Installed only when debug_mode is on, like TrainingCuttProbe, and after the scaling modules so the class
// lookup is the one they resolved.

// The class that owns the iterator. The machine the compiler built for its `PlayTrainingCut` is nested in
// it, and a nested type is not what a namespace and name lookup answers: run 18 reported
// `<PlayTrainingCut>d__70 is not in this client` while this client's own dump printed that class, its
// `MoveNext/0 -> bool()` and its captured locals, because the class is stored as
// Gallop.SingleModeMainTrainingCuttController/<PlayTrainingCut>d__70.
const OWNER_LABEL: &str = "Gallop.SingleModeMainTrainingCuttController";

// The number in a machine's name is the compiler's and moves when the owning class gains or loses an
// iterator, so the class is matched on the half that is a fact about the code: the method it was built for.
const MACHINE_PREFIX: &str = "<PlayTrainingCut>d__";

// The name a prefix match landed on, kept for the log so a run says which machine it measured. Before init
// resolved one there is no name to print, only the method the probe was looking for.
static MACHINE_NAME: OnceLock<String> = OnceLock::new();

fn machine_name() -> &'static str {
    MACHINE_NAME.get().map(String::as_str).unwrap_or(MACHINE_PREFIX)
}

const TIME_SCALE_NAME: &CStr = c"<timeScale>5__7";
const ALL_TEXT_WAIT_NAME: &CStr = c"<allTextWaitTime>5__12";
const HIGH_SPEED_NAME: &CStr = c"<isHighSpeedOnStart>5__9";
// The branch marker, spelled the way this client's dump spells it: `field <>1__state [int]`. It is private,
// so no field filter has ever printed it, and the probe reports a miss rather than guessing an offset.
const STATE_NAME: &CStr = c"<>1__state";

const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];

// A state machine's branch marker starts at -1 before its first move and ends at -2 or lower when it is
// finished, so no real state is this value and it is safe to keep as "nothing sampled yet".
pub(crate) const NO_STATE: i32 = i32::MIN;

// Whether a nested class name is the machine this probe stands on: the method it was built for, then the
// compiler's index, and nothing after it. `<PlayTrainingCutt>d__70` belongs to a different controller's
// iterator and is refused, as is a display class the same method generated or a machine with no index.
pub(crate) fn machine_name_matches(name: &str) -> bool {
    match name.strip_prefix(MACHINE_PREFIX) {
        Some(index) => !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()),
        None => false,
    }
}

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

// One window per training cut. `PlayTrainingCut` builds a fresh machine every turn, and a hold or a gap
// measured across two of them says something that never happened: run 18 printed `worst 65894 ms at branch
// 13` for a stretch where no training cut was open at all, because the clock behind that number was the
// last branch of the machine belonging to the cut that had already finished 66 s earlier. The cut window
// is opened with the machine and closed with the cut, and the run totals fold it in when they are read.
static CUT_OPEN_MS: AtomicI64 = AtomicI64::new(-1);
static CUT_STEPS: AtomicUsize = AtomicUsize::new(0);
static CUT_CHANGES: AtomicUsize = AtomicUsize::new(0);
static CUT_STATES_SEEN: AtomicU64 = AtomicU64::new(0);
static CUT_HOLDS: AtomicUsize = AtomicUsize::new(0);
static CUT_HOLD_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static CUT_HOLD_WORST_MS: AtomicI64 = AtomicI64::new(0);
static CUT_HOLD_WORST_STATE: AtomicI32 = AtomicI32::new(NO_STATE);
static CUT_GAP_RUNS: AtomicUsize = AtomicUsize::new(0);
static CUT_GAP_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static CUT_GAP_WORST_MS: AtomicI64 = AtomicI64::new(0);
static CUT_TIME_SCALE_PEAK: AtomicU32 = AtomicU32::new(0);
static CUT_ALL_TEXT_WAIT_PEAK: AtomicU32 = AtomicU32::new(0);

// Open while `TrainingCuttProbe` has a cut hole open, which is the stretch between the training cut-in
// ending and the status panel playing out, the leg run 19 measured at 4932 ms and 7866 ms. Counting the
// coroutine inside that window is the whole point: it separates a coroutine the driver stopped calling from
// one that is being called every frame while it waits on something the fork has no door on.
static HOLE_OPEN_MS: AtomicI64 = AtomicI64::new(-1);
static HOLE_START_STATE: AtomicI32 = AtomicI32::new(NO_STATE);
static HOLE_STEPS_AT_START: AtomicUsize = AtomicUsize::new(0);
static HOLE_CHANGES: AtomicUsize = AtomicUsize::new(0);
static HOLE_STATES_SEEN: AtomicU64 = AtomicU64::new(0);
static HOLE_GAP_WORST_MS: AtomicI64 = AtomicI64::new(0);
static CUTS_CLOSED: AtomicUsize = AtomicUsize::new(0);
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

// The largest value this window saw. Every door here runs on the game thread, so a load and a store is
// enough and no compare exchange is needed on a path the engine walks every frame it is playing a cut.
fn sample_peak(slot: &AtomicU32, value: f32) {
    if value <= f32::from_bits(slot.load(atomic::Ordering::Relaxed)) {
        return;
    }

    slot.store(value.to_bits(), atomic::Ordering::Relaxed);
}

// The values the game computed for this cut, read every frame the cut advances. A peak is enough: the
// question is how large the game's own number got, not what it was at one instant.
unsafe fn note_values(this: *mut Il2CppObject) {
    if let Some(value) = read_f32(this, TIME_SCALE_FIELD) {
        TIME_SCALE.sample(value);
        sample_peak(&CUT_TIME_SCALE_PEAK, value);
    }

    if let Some(value) = read_f32(this, ALL_TEXT_WAIT_FIELD) {
        ALL_TEXT_WAIT.sample(value);
        sample_peak(&CUT_ALL_TEXT_WAIT_PEAK, value);
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
    let previous_step_ms = LAST_STEP_MS.load(atomic::Ordering::Relaxed);
    let in_hole = HOLE_OPEN_MS.load(atomic::Ordering::Relaxed);

    CUT_STEPS.fetch_add(1, atomic::Ordering::Relaxed);

    match hold_of(previous, LAST_CHANGE_MS.load(atomic::Ordering::Relaxed), now, state) {
        Some(held) => {
            HOLDS.fetch_add(1, atomic::Ordering::Relaxed);
            HOLD_MS_TOTAL.fetch_add(held.held_ms, atomic::Ordering::Relaxed);
            CUT_HOLDS.fetch_add(1, atomic::Ordering::Relaxed);
            CUT_HOLD_MS_TOTAL.fetch_add(held.held_ms, atomic::Ordering::Relaxed);

            // A branch that never changes is the parked case, and its hold grows on every frame the
            // engine drives the coroutine, so the worst hold is kept without a second pass.
            if held.held_ms > HOLD_WORST_MS.load(atomic::Ordering::Relaxed) {
                HOLD_WORST_MS.store(held.held_ms, atomic::Ordering::Relaxed);
                HOLD_WORST_STATE.store(held.state, atomic::Ordering::Relaxed);
            }

            if held.held_ms > CUT_HOLD_WORST_MS.load(atomic::Ordering::Relaxed) {
                CUT_HOLD_WORST_MS.store(held.held_ms, atomic::Ordering::Relaxed);
                CUT_HOLD_WORST_STATE.store(held.state, atomic::Ordering::Relaxed);
            }

            if held.changed {
                let changes = STATE_CHANGES.fetch_add(1, atomic::Ordering::Relaxed) + 1;
                CUT_CHANGES.fetch_add(1, atomic::Ordering::Relaxed);
                LAST_CHANGE_MS.store(now, atomic::Ordering::Relaxed);

                if in_hole >= 0 {
                    HOLE_CHANGES.fetch_add(1, atomic::Ordering::Relaxed);
                }

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
    CUT_STATES_SEEN.fetch_or(state_bit(state), atomic::Ordering::Relaxed);

    if in_hole >= 0 {
        HOLE_STATES_SEEN.fetch_or(state_bit(state), atomic::Ordering::Relaxed);
    }

    if let Some(gap) = step_gap(previous_step_ms, now) {
        STEP_GAP_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        STEP_GAP_MS_TOTAL.fetch_add(gap, atomic::Ordering::Relaxed);
        CUT_GAP_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        CUT_GAP_MS_TOTAL.fetch_add(gap, atomic::Ordering::Relaxed);

        if gap > STEP_GAP_WORST_MS.load(atomic::Ordering::Relaxed) {
            STEP_GAP_WORST_MS.store(gap, atomic::Ordering::Relaxed);
        }

        if gap > CUT_GAP_WORST_MS.load(atomic::Ordering::Relaxed) {
            CUT_GAP_WORST_MS.store(gap, atomic::Ordering::Relaxed);
        }

        // A gap that began before the hole opened belongs to the stretch before it. Only a gap whose two
        // steps both sit in the hole is charged to the hole.
        if in_hole >= 0 && previous_step_ms >= in_hole && gap > HOLE_GAP_WORST_MS.load(atomic::Ordering::Relaxed) {
            HOLE_GAP_WORST_MS.store(gap, atomic::Ordering::Relaxed);
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
        let machine = machine_name();
        info!("Cut state probe: PlayTrainingCut handed back a {name} object, not {machine}");
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

    // A machine this cut built has no branch history, and this cut's window has none either. Without the
    // reset, the first step of a turn charges the time since the previous turn's last step to a branch of
    // the machine that finished: run 18 reported `worst 65894 ms at branch 13` across a stretch where no
    // training cut was open, which is the previous turn's last branch held for 66 s of menu, not a wait.
    LAST_STATE.store(NO_STATE, atomic::Ordering::Relaxed);
    LAST_CHANGE_MS.store(-1, atomic::Ordering::Relaxed);
    LAST_STEP_MS.store(-1, atomic::Ordering::Relaxed);
    CUT_OPEN_MS.store(elapsed_ms(), atomic::Ordering::Relaxed);
    CUT_STEPS.store(0, atomic::Ordering::Relaxed);
    CUT_CHANGES.store(0, atomic::Ordering::Relaxed);
    CUT_STATES_SEEN.store(0, atomic::Ordering::Relaxed);
    CUT_HOLDS.store(0, atomic::Ordering::Relaxed);
    CUT_HOLD_MS_TOTAL.store(0, atomic::Ordering::Relaxed);
    CUT_HOLD_WORST_MS.store(0, atomic::Ordering::Relaxed);
    CUT_HOLD_WORST_STATE.store(NO_STATE, atomic::Ordering::Relaxed);
    CUT_GAP_RUNS.store(0, atomic::Ordering::Relaxed);
    CUT_GAP_MS_TOTAL.store(0, atomic::Ordering::Relaxed);
    CUT_GAP_WORST_MS.store(0, atomic::Ordering::Relaxed);
    CUT_TIME_SCALE_PEAK.store(0, atomic::Ordering::Relaxed);
    CUT_ALL_TEXT_WAIT_PEAK.store(0, atomic::Ordering::Relaxed);

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

// The wall a cut window had, or nothing when no cut was open. A `CleanUpCutt` that finds nothing open is
// the game cleaning a cutt it already cleaned, which run 19 counted 8 times against 6 cuts.
pub(crate) fn cut_wall_ms(opened_ms: i64, now_ms: i64) -> Option<i64> {
    if opened_ms < 0 || now_ms < opened_ms {
        return None;
    }

    Some(now_ms - opened_ms)
}

// Steps between two readings of a counter that only ever grows.
pub(crate) fn steps_between(first: usize, last: usize) -> usize {
    last.saturating_sub(first)
}

fn mean_of(total: i64, count: usize) -> f64 {
    match count {
        0 => 0.0,
        n => total as f64 / n as f64,
    }
}

// Opens the coroutine window over the hole `TrainingCuttProbe` measured, from the training cut-in ending to
// the status panel playing out. The two probes share the pairing so the hole in the wall clock and the
// coroutine inside it are the same stretch.
pub(crate) fn note_hole_open() {
    HOLE_OPEN_MS.store(elapsed_ms(), atomic::Ordering::Relaxed);
    HOLE_START_STATE.store(LAST_STATE.load(atomic::Ordering::Relaxed), atomic::Ordering::Relaxed);
    HOLE_STEPS_AT_START.store(MOVENEXT.calls(), atomic::Ordering::Relaxed);
    HOLE_CHANGES.store(0, atomic::Ordering::Relaxed);
    HOLE_STATES_SEEN.store(0, atomic::Ordering::Relaxed);
    HOLE_GAP_WORST_MS.store(0, atomic::Ordering::Relaxed);
}

// Says what the cut's own coroutine did while the status panel was being held off. Zero steps and a branch
// that never moved says the driver stopped calling the coroutine, which a fork may not answer. Many steps
// with a branch that never moved says the coroutine is being driven every frame while it waits on a
// condition, which is the case a duration door or a setting could still reach.
pub(crate) fn note_hole_closed(span_ms: i64) {
    if HOLE_OPEN_MS.swap(-1, atomic::Ordering::Relaxed) < 0 {
        return;
    }

    let steps = steps_between(HOLE_STEPS_AT_START.load(atomic::Ordering::Relaxed), MOVENEXT.calls());
    let changes = HOLE_CHANGES.load(atomic::Ordering::Relaxed);
    let distinct = states_seen(HOLE_STATES_SEEN.load(atomic::Ordering::Relaxed));
    let worst_gap = HOLE_GAP_WORST_MS.load(atomic::Ordering::Relaxed);
    let open_branch = HOLE_START_STATE.load(atomic::Ordering::Relaxed);
    let close_branch = LAST_STATE.load(atomic::Ordering::Relaxed);

    info!(
        "Cut state probe cut hole {span_ms} ms with the cut coroutine in it: branch {open_branch} at the cut-in end, {close_branch} at the play out, {steps} MoveNext steps, {changes} branch changes over {distinct} branches, worst gap between steps {worst_gap} ms"
    );
}

// Closes the window a cut's coroutine had, one line per training cut, paired with `cut run N closed`
// because both open at `PlayTrainingCut` and close at `CleanUpCutt`. The window is then folded into the
// run totals the periodic report reads.
pub(crate) fn note_cut_run_closed() {
    let opened_ms = CUT_OPEN_MS.swap(-1, atomic::Ordering::Relaxed);
    let Some(wall_ms) = cut_wall_ms(opened_ms, elapsed_ms()) else {
        return;
    };

    let cuts = CUTS_CLOSED.fetch_add(1, atomic::Ordering::Relaxed) + 1;
    let steps = CUT_STEPS.load(atomic::Ordering::Relaxed);
    let changes = CUT_CHANGES.load(atomic::Ordering::Relaxed);
    let distinct = states_seen(CUT_STATES_SEEN.load(atomic::Ordering::Relaxed));
    let holds = CUT_HOLDS.load(atomic::Ordering::Relaxed);
    let hold_ms = CUT_HOLD_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let worst_hold = CUT_HOLD_WORST_MS.load(atomic::Ordering::Relaxed);
    let worst_branch = CUT_HOLD_WORST_STATE.load(atomic::Ordering::Relaxed);
    let gaps = CUT_GAP_RUNS.load(atomic::Ordering::Relaxed);
    let gap_ms = CUT_GAP_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let worst_gap = CUT_GAP_WORST_MS.load(atomic::Ordering::Relaxed);
    // A value never sampled keeps the bits of zero, and the bits of zero are the value zero.
    let time_scale = f32::from_bits(CUT_TIME_SCALE_PEAK.load(atomic::Ordering::Relaxed));
    let all_text_wait = f32::from_bits(CUT_ALL_TEXT_WAIT_PEAK.load(atomic::Ordering::Relaxed));
    let hold_mean = mean_of(hold_ms, holds);
    let gap_mean = mean_of(gap_ms, gaps);

    // A hole whose play out never arrived is dropped here rather than paired with the next cut's, which is
    // the rule `TrainingCuttProbe::close_cut_run` runs on.
    HOLE_OPEN_MS.store(-1, atomic::Ordering::Relaxed);

    // The run totals are charged on every `MoveNext`, so closing a window only empties the window. A report
    // taken between two cuts therefore has nothing left in it to count twice.
    CUT_STEPS.store(0, atomic::Ordering::Relaxed);
    CUT_CHANGES.store(0, atomic::Ordering::Relaxed);
    CUT_STATES_SEEN.store(0, atomic::Ordering::Relaxed);
    CUT_HOLDS.store(0, atomic::Ordering::Relaxed);
    CUT_HOLD_MS_TOTAL.store(0, atomic::Ordering::Relaxed);
    CUT_HOLD_WORST_MS.store(0, atomic::Ordering::Relaxed);
    CUT_HOLD_WORST_STATE.store(NO_STATE, atomic::Ordering::Relaxed);
    CUT_GAP_RUNS.store(0, atomic::Ordering::Relaxed);
    CUT_GAP_MS_TOTAL.store(0, atomic::Ordering::Relaxed);
    CUT_GAP_WORST_MS.store(0, atomic::Ordering::Relaxed);
    CUT_TIME_SCALE_PEAK.store(0, atomic::Ordering::Relaxed);
    CUT_ALL_TEXT_WAIT_PEAK.store(0, atomic::Ordering::Relaxed);

    info!(
        "Cut state probe cut {cuts} coroutine over {wall_ms} ms: {steps} MoveNext steps, {changes} branch changes over {distinct} branches, holds {holds} mean {hold_mean:.1} ms worst {worst_hold} ms at branch {worst_branch}, gaps {gaps} mean {gap_mean:.1} ms worst {worst_gap} ms, timeScale peak {time_scale:.4}, allTextWaitTime peak {all_text_wait:.4}"
    );
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());

    let owner = match class_for_label(umamusume, OWNER_LABEL) {
        Some(owner) => owner,
        None => {
            info!("Cut state probe: {OWNER_LABEL} is not in this client, the training cut coroutine is not measured");
            return;
        }
    };

    // The machine is a nested type of the class that owns the iterator, and a nested type is not what the
    // namespace and name lookup answers, so it is reached through its owner (C59).
    let (name, class) = match unsafe { find_nested_class_by_prefix(owner, MACHINE_PREFIX) } {
        Some((name, class)) if machine_name_matches(&name) => (name, class),
        Some((name, _)) => {
            info!("Cut state probe: {OWNER_LABEL} nests a {name} that is not the training cut coroutine, it is not measured");
            return;
        }
        None => {
            info!("Cut state probe: {OWNER_LABEL} nests no iterator built for PlayTrainingCut, the coroutine is not measured");
            return;
        }
    };

    let _ = MACHINE_NAME.set(name);
    MACHINE_CLASS.store(class, atomic::Ordering::Relaxed);

    unsafe {
        TIME_SCALE_FIELD = get_field_from_name(class, TIME_SCALE_NAME);
        ALL_TEXT_WAIT_FIELD = get_field_from_name(class, ALL_TEXT_WAIT_NAME);
        HIGH_SPEED_FIELD = get_field_from_name(class, HIGH_SPEED_NAME);
        STATE_FIELD = get_field_from_name(class, STATE_NAME);
    }

    let addr = unsafe { AnimationSpeed::resolve_method(class, "MoveNext", NO_PARAMS, BOOL) };

    if addr == 0 {
        let machine = machine_name();
        info!("Cut state probe: {machine} has no MoveNext with the dumped signature, only the opening values can be read");
        return;
    }

    new_hook!(addr, PlayTrainingCutStateMachine_MoveNext);

    let named = |field: *mut FieldInfo| if field.is_null() { "not found" } else { "resolved" };
    let machine = machine_name();

    info!(
        "Cut state probe: standing on {machine}::MoveNext, a nested type of {OWNER_LABEL}, timeScale {}, allTextWaitTime {}, isHighSpeedOnStart {}, branch marker {}",
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
    let hold_mean = mean_of(hold_ms, holds);
    let gap_mean = mean_of(STEP_GAP_MS_TOTAL.load(atomic::Ordering::Relaxed), gaps);
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

    #[test]
    fn the_machine_is_the_iterator_built_for_the_training_cut() {        assert!(machine_name_matches("<PlayTrainingCut>d__70"));
        assert!(machine_name_matches("<PlayTrainingCut>d__7"));

        // `TrainingCuttTeamRaceController` has an iterator whose name differs by one letter, the method this
        // probe stands on also generates display classes, and a prefix with no compiler index behind it is
        // not a machine. Each is refused so the door lands on one coroutine rather than on whatever matched
        // first in the owner's nested type list.
        assert!(!machine_name_matches("<PlayTrainingCutt>d__70"));
        assert!(!machine_name_matches("<>c__DisplayClass70_0"));
        assert!(!machine_name_matches("<PlayTrainingCut>d__70_1"));
        assert!(!machine_name_matches("<PlayTrainingCut>d__"));
        assert!(!machine_name_matches("MoveNext"));
    }

    #[test]
    fn a_cut_window_is_measured_only_while_a_cut_was_open() {
        // Run 19 closed 8 `CleanUpCutt` calls against 6 cuts, and the two extra ones must not be reported
        // as cuts with a coroutine of their own.
        assert_eq!(cut_wall_ms(-1, 142_915), None);
        assert_eq!(cut_wall_ms(132_453, 132_452), None);
        assert_eq!(cut_wall_ms(132_453, 142_915), Some(10_462));
    }

    #[test]
    fn steps_are_counted_between_two_marks_of_the_same_counter() {
        assert_eq!(steps_between(231, 612), 381);
        assert_eq!(steps_between(612, 612), 0);

        // A mark taken before the counter existed cannot produce a negative count.
        assert_eq!(steps_between(612, 231), 0);
    }

    #[test]
    fn a_mean_of_nothing_is_zero_rather_than_a_divide_by_zero() {
        assert_eq!(mean_of(6_150, 6), 1_025.0);
        assert_eq!(mean_of(0, 0), 0.0);
    }
}
