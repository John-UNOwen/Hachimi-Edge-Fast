use std::ffi::CString;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI64, AtomicU32, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::AnimationSpeed,
        symbols::get_class,
        types::*,
    },
};

// Observe only hooks over the training screen's animation, the friendship training cut-in first.
//
// Run 8 read 104.7 s on `SingleModeMainView` and none of the training hooks AnimationSpeed installs
// printed a call line, so the mod has no measurement of what the cut-in costs. Every hook here hands
// its arguments to the original untouched and only records what it saw, which is why observing these
// paths cannot change them.
//
// The numbers this looks for are the ones a scaling decision needs: how long one cut-in run lasts in
// the cut's own timeline seconds and in wall clock, how large the playback rate the game already asks
// for is, and whether the game's own skip doors (`SkipRuntime`, `SkipTimeDirect`,
// `SingleModeMainViewTrainingCutStatus::Skip`) are ever reached on their own.
//
// `get_CurrentTime`, `get_CurrentTimeScale` and `get_WaitingTime` are sampled as a peak only. They are
// getters the engine reads every frame, so their hooks do one atomic increment and one atomic max, and
// the value is still handed back untouched.
//
// The run boundaries come from `ResetCurrentTime`, which the engine calls when a cut starts. That is a
// proven signature rather than an assumed one: `PlayTrainingCutt`, `OnStartTrainingCutt` and
// `OnStopTrainingCutt` are names in the metadata with no dumped signature yet.
//
// Installed only when debug_mode is on, like StoryFrameProbe, and installed after the scaling modules
// so both resolve the same class lookups.
const PROBE_DETAIL_LIMIT: usize = 8;
const PROBE_CHUNK: usize = 4096;
const REPORT_INTERVAL_SECS: i64 = 20;
// A timeline peak is summed as whole milliseconds so the total stays an integer.
const MS_PER_SECOND: f32 = 1000.0;

const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const I4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I4;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const ONE_FLOAT: &[Il2CppTypeEnum] = &[R4];
const ONE_FLAG: &[Il2CppTypeEnum] = &[BOOL];
const LIST_AND_FLOAT: &[Il2CppTypeEnum] = &[CLASS, R4];
const FRAMES_AND_FLAG: &[Il2CppTypeEnum] = &[I4, BOOL];

fn bit(flag: bool) -> f64 {
    if flag { 1.0 } else { 0.0 }
}

// The peak merge a frame hot getter needs: the largest value seen so far, kept as `f32` bits so one
// atomic is enough. A negative or non finite reading is ignored instead of becoming the peak, because
// a NaN would out rank every real value behind it, and a 0.0 answer from a class that is still being
// set up must not wipe a peak already measured.
fn peak_merge(current: u32, value: f32) -> u32 {
    if !value.is_finite() || value < 0.0 {
        return current;
    }

    let candidate = value.to_bits();

    if candidate > current { candidate } else { current }
}

fn peak_seconds(bits: u32) -> f32 {
    f32::from_bits(bits)
}

fn peak_milliseconds(bits: u32) -> i64 {
    (f32::from_bits(bits) * MS_PER_SECOND) as i64
}

// A report is due when the totals moved and the interval passed. A quiet path stays quiet, which is
// the rule StoryFrameProbe runs on.
fn report_due(now_sec: i64, last_sec: i64, totals: usize, last_totals: usize, interval: i64) -> bool {
    if totals == 0 || totals == last_totals {
        return false;
    }

    last_sec < 0 || now_sec - last_sec >= interval
}

struct CutProbe {
    name: &'static str,
    calls: AtomicUsize,
    // Set for the probes worth a peak: the largest value this path handed back all run.
    peaked: bool,
    peak: AtomicU32,
}

impl CutProbe {
    const fn counted(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0), peaked: false, peak: AtomicU32::new(0) }
    }

    const fn peaked(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0), peaked: true, peak: AtomicU32::new(0) }
    }

    // First hits print the values, later ones only the count, so a path the game polls every frame
    // cannot fill the log.
    fn observe(&self, values: &[f64]) {
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        if calls <= PROBE_DETAIL_LIMIT {
            debug!("Cutt probe {} call {}: {:?}", self.name, calls, values);
        }
        else if calls % PROBE_CHUNK == 0 {
            debug!("Cutt probe {} {} calls", self.name, calls);
        }
    }

    fn count(&self) {
        self.observe(&[]);
    }

    // The frame hot shape: one increment, one max, no slice and no formatting until a chunk boundary.
    fn sample(&self, value: f32) {
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        if self.peaked {
            let bits = peak_merge(self.peak.load(atomic::Ordering::Relaxed), value);
            self.peak.fetch_max(bits, atomic::Ordering::Relaxed);
        }

        if calls % PROBE_CHUNK == 0 {
            debug!("Cutt probe {} {} calls peak {}", self.name, calls, f32::from_bits(self.peak.load(atomic::Ordering::Relaxed)));
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(atomic::Ordering::Relaxed)
    }

    fn peak_bits(&self) -> u32 {
        self.peak.load(atomic::Ordering::Relaxed)
    }

    // The totals line carries the method name without its argument list.
    fn short(&self) -> &str {
        match self.name.split_once('(') {
            Some((head, _)) => head,
            None => self.name,
        }
    }
}

static GET_TRAINING_CUT_TIME_SCALE: CutProbe = CutProbe::peaked("SingleModeUtils::GetTrainingCutTimeScale(scale)");
static CUT_IN_GET_TARGET_SPEED: CutProbe = CutProbe::peaked("SingleModeTrainingCutInHelper::GetTargetSpeed()");
static CUT_IN_IS_HIGH_SPEED_MODE: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::IsHighSpeedMode()");
static CUT_IN_SKIP_RUNTIME: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::SkipRuntime()");
static CUTT_RESET_CURRENT_TIME: CutProbe = CutProbe::counted("CutInTimelineController::ResetCurrentTime()");
static CUTT_GET_CURRENT_TIME: CutProbe = CutProbe::peaked("CutInTimelineController::get_CurrentTime()");
static CUTT_GET_CURRENT_TIME_SCALE: CutProbe = CutProbe::peaked("CutInTimelineController::get_CurrentTimeScale()");
static CUTT_GET_WAITING_TIME: CutProbe = CutProbe::peaked("CutInTimelineController::get_WaitingTime()");
static CUTT_SET_SPEED: CutProbe = CutProbe::peaked("CutInTimelineController::SetSpeed(speed)");
static CUTT_UPDATE_SPEED: CutProbe = CutProbe::counted("CutInTimelineController::UpdateSpeed()");
static CUTT_SKIP_RUNTIME_TIME: CutProbe = CutProbe::counted("CutInTimelineController::SkipRuntime(time)");
static CUTT_SKIP_RUNTIME_FRAMES: CutProbe = CutProbe::counted("CutInTimelineController::SkipRuntime(frames, keep)");
static CUTT_SKIP_TIME_DIRECT: CutProbe = CutProbe::counted("CutInTimelineController::SkipTimeDirect(time)");
static CUT_STATUS_SKIP: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::Skip(skip)");
static CUTT_WAIT_TAP_ASYNC: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::WaitTapAsync()");
static CUTT_FADE_OUT_RESULT_FLASH: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::FadeOutResultFlash()");
static PLATE_INITIALIZE_LIST: CutProbe = CutProbe::peaked("TrainingParamChangeUI::InitializePlateList(list, interval)");
static MAIN_COROUTINE_DOTWEEN_SCALE: CutProbe = CutProbe::counted("SingleModeMainViewController::CoroutineDoTweenTimeScale()");
static MAIN_WAIT_TAP: CutProbe = CutProbe::counted("SingleModeMainViewController::WaitTap()");

static PROBES: [&CutProbe; 19] = [
    &GET_TRAINING_CUT_TIME_SCALE,
    &CUT_IN_GET_TARGET_SPEED,
    &CUT_IN_IS_HIGH_SPEED_MODE,
    &CUT_IN_SKIP_RUNTIME,
    &CUTT_RESET_CURRENT_TIME,
    &CUTT_GET_CURRENT_TIME,
    &CUTT_GET_CURRENT_TIME_SCALE,
    &CUTT_GET_WAITING_TIME,
    &CUTT_SET_SPEED,
    &CUTT_UPDATE_SPEED,
    &CUTT_SKIP_RUNTIME_TIME,
    &CUTT_SKIP_RUNTIME_FRAMES,
    &CUTT_SKIP_TIME_DIRECT,
    &CUT_STATUS_SKIP,
    &CUTT_WAIT_TAP_ASYNC,
    &CUTT_FADE_OUT_RESULT_FLASH,
    &PLATE_INITIALIZE_LIST,
    &MAIN_COROUTINE_DOTWEEN_SCALE,
    &MAIN_WAIT_TAP,
];

// One cut-in run, opened by `ResetCurrentTime` and closed by the next one. The wall clock between the
// two is what the player waits, and the peak of `get_CurrentTime` reached inside it is how long the
// cut's own timeline ran, which is the number a rate hook has to shorten.
static RUN_OPENED_MS: AtomicI64 = AtomicI64::new(-1);
static RUN_PEAK_BITS: AtomicU32 = AtomicU32::new(0);
static RUNS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static RUN_WALL_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static RUN_PEAK_MS_TOTAL: AtomicI64 = AtomicI64::new(0);

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

// Called on every `ResetCurrentTime`. The run before is closed first, so the first cut-in a session
// plays is measured the same way as the last one.
fn open_cut_run() {
    let now = elapsed_ms();
    CUTT_RESET_CURRENT_TIME.count();

    if now < 0 {
        return;
    }

    let opened = RUN_OPENED_MS.swap(now, atomic::Ordering::Relaxed);
    let peak = RUN_PEAK_BITS.swap(0, atomic::Ordering::Relaxed);

    if opened < 0 {
        return;
    }

    let run_ms = now - opened;
    let runs = RUNS_CLOSED.fetch_add(1, atomic::Ordering::Relaxed) + 1;
    RUN_WALL_MS_TOTAL.fetch_add(run_ms, atomic::Ordering::Relaxed);
    RUN_PEAK_MS_TOTAL.fetch_add(peak_milliseconds(peak), atomic::Ordering::Relaxed);

    if runs <= PROBE_DETAIL_LIMIT {
        info!("Cutt probe: cut run {} closed at {now} ms, timeline peak {} s, wall {run_ms} ms", runs, peak_seconds(peak));
    }
}

type GetTrainingCutTimeScaleFn = extern "C" fn(scale: f32) -> f32;
// Dumped static: `GetTrainingCutTimeScale/1 -> static float(float)`, so the wrapper declares the
// dumped argument and no `this` (A3).
extern "C" fn TrainingCuttUtils_GetTrainingCutTimeScale(scale: f32) -> f32 {
    let value = get_orig_fn!(TrainingCuttUtils_GetTrainingCutTimeScale, GetTrainingCutTimeScaleFn)(scale);
    GET_TRAINING_CUT_TIME_SCALE.observe(&[scale as f64, value as f64]);
    GET_TRAINING_CUT_TIME_SCALE.sample(value);

    value
}

type CutInSkipRuntimeFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn TrainingCuttHelper_SkipRuntime(this: *mut Il2CppObject) {
    CUT_IN_SKIP_RUNTIME.count();

    get_orig_fn!(TrainingCuttHelper_SkipRuntime, CutInSkipRuntimeFn)(this);
}

type CutInGetTargetSpeedFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
extern "C" fn TrainingCuttHelper_GetTargetSpeed(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(TrainingCuttHelper_GetTargetSpeed, CutInGetTargetSpeedFn)(this);
    CUT_IN_GET_TARGET_SPEED.observe(&[value as f64]);
    CUT_IN_GET_TARGET_SPEED.sample(value);

    value
}

// Dumped static: `IsHighSpeedMode/0 -> static bool()`. Counted only, because a training screen reads
// it every frame and a bool has no value worth printing per call.
type CutInIsHighSpeedModeFn = extern "C" fn() -> bool;
extern "C" fn TrainingCuttHelper_IsHighSpeedMode() -> bool {
    CUT_IN_IS_HIGH_SPEED_MODE.count();

    get_orig_fn!(TrainingCuttHelper_IsHighSpeedMode, CutInIsHighSpeedModeFn)()
}

type CuttResetCurrentTimeFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn CuttTimeline_ResetCurrentTime(this: *mut Il2CppObject) {
    open_cut_run();

    get_orig_fn!(CuttTimeline_ResetCurrentTime, CuttResetCurrentTimeFn)(this);
}

type CuttGetCurrentTimeFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
extern "C" fn CuttTimeline_GetCurrentTime(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CuttTimeline_GetCurrentTime, CuttGetCurrentTimeFn)(this);
    CUTT_GET_CURRENT_TIME.sample(value);

    let bits = peak_merge(RUN_PEAK_BITS.load(atomic::Ordering::Relaxed), value);
    RUN_PEAK_BITS.fetch_max(bits, atomic::Ordering::Relaxed);

    value
}

type CuttGetCurrentTimeScaleFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
extern "C" fn CuttTimeline_GetCurrentTimeScale(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CuttTimeline_GetCurrentTimeScale, CuttGetCurrentTimeScaleFn)(this);
    CUTT_GET_CURRENT_TIME_SCALE.sample(value);

    value
}

type CuttGetWaitingTimeFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
extern "C" fn CuttTimeline_GetWaitingTime(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CuttTimeline_GetWaitingTime, CuttGetWaitingTimeFn)(this);
    CUTT_GET_WAITING_TIME.sample(value);

    value
}

type CuttSetSpeedFn = extern "C" fn(this: *mut Il2CppObject, speed: f32);
extern "C" fn CuttTimeline_SetSpeed(this: *mut Il2CppObject, speed: f32) {
    CUTT_SET_SPEED.observe(&[speed as f64]);
    CUTT_SET_SPEED.sample(speed);

    get_orig_fn!(CuttTimeline_SetSpeed, CuttSetSpeedFn)(this, speed);
}

type CuttUpdateSpeedFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn CuttTimeline_UpdateSpeed(this: *mut Il2CppObject) {
    CUTT_UPDATE_SPEED.count();

    get_orig_fn!(CuttTimeline_UpdateSpeed, CuttUpdateSpeedFn)(this);
}

type CuttSkipRuntimeTimeFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
extern "C" fn CuttTimeline_SkipRuntimeTime(this: *mut Il2CppObject, time: f32) {
    CUTT_SKIP_RUNTIME_TIME.observe(&[time as f64]);

    get_orig_fn!(CuttTimeline_SkipRuntimeTime, CuttSkipRuntimeTimeFn)(this, time);
}

type CuttSkipRuntimeFramesFn = extern "C" fn(this: *mut Il2CppObject, frames: i32, keep: bool);
extern "C" fn CuttTimeline_SkipRuntimeFrames(this: *mut Il2CppObject, frames: i32, keep: bool) {
    CUTT_SKIP_RUNTIME_FRAMES.observe(&[frames as f64, bit(keep)]);

    get_orig_fn!(CuttTimeline_SkipRuntimeFrames, CuttSkipRuntimeFramesFn)(this, frames, keep);
}

type CuttSkipTimeDirectFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
extern "C" fn CuttTimeline_SkipTimeDirect(this: *mut Il2CppObject, time: f32) {
    CUTT_SKIP_TIME_DIRECT.observe(&[time as f64]);

    get_orig_fn!(CuttTimeline_SkipTimeDirect, CuttSkipTimeDirectFn)(this, time);
}

type CutStatusSkipFn = extern "C" fn(this: *mut Il2CppObject, skip: bool);
extern "C" fn TrainingCutStatus_Skip(this: *mut Il2CppObject, skip: bool) {
    CUT_STATUS_SKIP.observe(&[bit(skip)]);

    get_orig_fn!(TrainingCutStatus_Skip, CutStatusSkipFn)(this, skip);
}

// `WaitTapAsync/0 -> class<System.Collections.IEnumerator>()` and `FadeOutResultFlash/0 -> void()` are
// the two ends of the cut that already have a dumped signature. The coroutine object is handed back
// untouched.
type WaitTapAsyncFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
extern "C" fn TrainingCutt_WaitTapAsync(this: *mut Il2CppObject) -> *mut Il2CppObject {
    CUTT_WAIT_TAP_ASYNC.count();

    get_orig_fn!(TrainingCutt_WaitTapAsync, WaitTapAsyncFn)(this)
}

type FadeOutResultFlashFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn TrainingCutt_FadeOutResultFlash(this: *mut Il2CppObject) {
    CUTT_FADE_OUT_RESULT_FLASH.count();

    get_orig_fn!(TrainingCutt_FadeOutResultFlash, FadeOutResultFlashFn)(this);
}

type InitializePlateListFn = extern "C" fn(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32);
// `InitializePlateList/2 -> void(class<Gallop.SingleModeTrainingCutInHelper list>, float)`: the list
// travels as a pointer and is passed straight through, and only the float is recorded.
extern "C" fn TrainingParamChangeUI_InitializePlateList(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32) {
    PLATE_INITIALIZE_LIST.observe(&[interval as f64]);
    PLATE_INITIALIZE_LIST.sample(interval);

    get_orig_fn!(TrainingParamChangeUI_InitializePlateList, InitializePlateListFn)(this, list, interval);
}

type CoroutineReturnFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
extern "C" fn SingleModeMain_CoroutineDoTweenTimeScale(this: *mut Il2CppObject) -> *mut Il2CppObject {
    MAIN_COROUTINE_DOTWEEN_SCALE.count();

    get_orig_fn!(SingleModeMain_CoroutineDoTweenTimeScale, CoroutineReturnFn)(this)
}

extern "C" fn SingleModeMain_WaitTap(this: *mut Il2CppObject) -> *mut Il2CppObject {
    MAIN_WAIT_TAP.count();

    get_orig_fn!(SingleModeMain_WaitTap, CoroutineReturnFn)(this)
}

// The dump prints a class as `namespace.name`, and this client has only ever looked classes up under
// `Gallop`. The cut-in engine sits under `Gallop.CutIn.Cutt` (A27), so a label is resolved by taking
// its last segment as the class name and the rest as the namespace.
fn class_for_label(image: *const Il2CppImage, label: &str) -> Option<*mut Il2CppClass> {
    let (namespace, name) = match label.rsplit_once('.') {
        Some((namespace, name)) => (namespace, name),
        None => ("", label),
    };

    let namespace = match CString::new(namespace) {
        Ok(value) => value,
        Err(_) => return None,
    };
    let name = match CString::new(name) {
        Ok(value) => value,
        Err(_) => return None,
    };

    match get_class(image, &namespace, &name) {
        Ok(class) => Some(class),
        Err(_) => None,
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());

    let single_mode_utils = class_for_label(umamusume, "Gallop.SingleModeUtils");
    let cut_in_helper = class_for_label(umamusume, "Gallop.SingleModeTrainingCutInHelper");
    let timeline = class_for_label(umamusume, "Gallop.CutIn.Cutt.CutInTimelineController");
    let cut_status = class_for_label(umamusume, "Gallop.SingleModeMainViewTrainingCutStatus");
    let cutt_controller = class_for_label(umamusume, "Gallop.SingleModeMainTrainingCuttController");
    let plate_ui = class_for_label(umamusume, "Gallop.TrainingParamChangeUI");
    let main_view = class_for_label(umamusume, "Gallop.SingleModeMainViewController");

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;

    // Instance candidates go through the same matcher the scaling hooks use: the dumped parameter
    // types, the return type, the generic check, and reference flags. Nothing is installed on a name
    // plus an arity.
    macro_rules! probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        installed += 1;
                    }
                    else {
                        missing.push($label);
                    }
                },
                None => missing.push($label),
            }
        };
    }

    // A static candidate has no hidden `this`, so it resolves through the static matcher and its
    // wrapper declares only the dumped arguments.
    macro_rules! static_probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_static_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        installed += 1;
                    }
                    else {
                        missing.push($label);
                    }
                },
                None => missing.push($label),
            }
        };
    }

    static_probe!(single_mode_utils, TrainingCuttUtils_GetTrainingCutTimeScale, "GetTrainingCutTimeScale", ONE_FLOAT, R4, "SingleModeUtils::GetTrainingCutTimeScale");

    probe!(cut_in_helper, TrainingCuttHelper_SkipRuntime, "SkipRuntime", NO_PARAMS, VOID, "SingleModeTrainingCutInHelper::SkipRuntime");
    probe!(cut_in_helper, TrainingCuttHelper_GetTargetSpeed, "GetTargetSpeed", NO_PARAMS, R4, "SingleModeTrainingCutInHelper::GetTargetSpeed");
    static_probe!(cut_in_helper, TrainingCuttHelper_IsHighSpeedMode, "IsHighSpeedMode", NO_PARAMS, BOOL, "SingleModeTrainingCutInHelper::IsHighSpeedMode");

    // The two `SkipRuntime` overloads are distinct by parameter list, which is the only way to tell
    // them apart: CLASS matches every reference type, so arity alone would not (A2).
    probe!(timeline, CuttTimeline_ResetCurrentTime, "ResetCurrentTime", NO_PARAMS, VOID, "CutInTimelineController::ResetCurrentTime");
    probe!(timeline, CuttTimeline_GetCurrentTime, "get_CurrentTime", NO_PARAMS, R4, "CutInTimelineController::get_CurrentTime");
    probe!(timeline, CuttTimeline_GetCurrentTimeScale, "get_CurrentTimeScale", NO_PARAMS, R4, "CutInTimelineController::get_CurrentTimeScale");
    probe!(timeline, CuttTimeline_GetWaitingTime, "get_WaitingTime", NO_PARAMS, R4, "CutInTimelineController::get_WaitingTime");
    probe!(timeline, CuttTimeline_SetSpeed, "SetSpeed", ONE_FLOAT, VOID, "CutInTimelineController::SetSpeed");
    probe!(timeline, CuttTimeline_UpdateSpeed, "UpdateSpeed", NO_PARAMS, VOID, "CutInTimelineController::UpdateSpeed");
    probe!(timeline, CuttTimeline_SkipRuntimeTime, "SkipRuntime", ONE_FLOAT, VOID, "CutInTimelineController::SkipRuntime(time)");
    probe!(timeline, CuttTimeline_SkipRuntimeFrames, "SkipRuntime", FRAMES_AND_FLAG, VOID, "CutInTimelineController::SkipRuntime(frames, keep)");
    probe!(timeline, CuttTimeline_SkipTimeDirect, "SkipTimeDirect", ONE_FLOAT, VOID, "CutInTimelineController::SkipTimeDirect");

    probe!(cut_status, TrainingCutStatus_Skip, "Skip", ONE_FLAG, VOID, "SingleModeMainViewTrainingCutStatus::Skip");
    probe!(cutt_controller, TrainingCutt_WaitTapAsync, "WaitTapAsync", NO_PARAMS, CLASS, "SingleModeMainTrainingCuttController::WaitTapAsync");
    probe!(cutt_controller, TrainingCutt_FadeOutResultFlash, "FadeOutResultFlash", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::FadeOutResultFlash");
    probe!(plate_ui, TrainingParamChangeUI_InitializePlateList, "InitializePlateList", LIST_AND_FLOAT, VOID, "TrainingParamChangeUI::InitializePlateList");
    probe!(main_view, SingleModeMain_CoroutineDoTweenTimeScale, "CoroutineDoTweenTimeScale", NO_PARAMS, CLASS, "SingleModeMainViewController::CoroutineDoTweenTimeScale");
    probe!(main_view, SingleModeMain_WaitTap, "WaitTap", NO_PARAMS, CLASS, "SingleModeMainViewController::WaitTap");

    info!("Cutt probe: {installed} of {} observe only probes installed, cut runs measured from ResetCurrentTime", PROBES.len());

    if !missing.is_empty() {
        info!("Cutt probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the frame probe report.
pub fn report_if_due() {
    if START.get().is_none() {
        return;
    }

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();
    let now_sec = elapsed_ms() / 1000;
    let last_sec = LAST_REPORT_SEC.load(atomic::Ordering::Relaxed);
    let last_totals = LAST_TOTALS.load(atomic::Ordering::Relaxed);

    if !report_due(now_sec, last_sec, totals, last_totals, REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_TOTALS.store(totals, atomic::Ordering::Relaxed);

    let mut line = String::new();

    for probe in PROBES.iter() {
        let calls = probe.calls();

        if calls == 0 {
            continue;
        }

        if probe.peaked {
            let _ = write!(line, " {}={} peak {:.3}", probe.short(), calls, peak_seconds(probe.peak_bits()));
        }
        else {
            let _ = write!(line, " {}={}", probe.short(), calls);
        }
    }

    let runs = RUNS_CLOSED.load(atomic::Ordering::Relaxed);
    let wall_ms = RUN_WALL_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let peak_ms = RUN_PEAK_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let opened = RUN_OPENED_MS.load(atomic::Ordering::Relaxed);
    let open_for = if opened < 0 { 0 } else { elapsed_ms() - opened };

    info!("Cutt probe totals at {now_sec} s:{line} cut runs {runs} wall {wall_ms} ms timeline {peak_ms} ms open {open_for} ms");
}

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_merge_keeps_the_largest_finite_non_negative_value() {
        let after = peak_merge(0, 1.5);

        assert_eq!(f32::from_bits(peak_merge(after, 0.25)), 1.5);
        assert_eq!(f32::from_bits(peak_merge(after, 3.0)), 3.0);
        assert_eq!(peak_merge(after, f32::NAN), after);
        assert_eq!(peak_merge(after, -8.0), after);
    }

    #[test]
    fn peak_merge_survives_a_zero_reading() {
        // A getter that answers 0.0 while a class is still being set up must not wipe a real peak.
        assert_eq!(f32::from_bits(peak_merge(3.0f32.to_bits(), 0.0)), 3.0);
    }

    #[test]
    fn peak_milliseconds_turns_a_timeline_peak_into_an_integer() {
        assert_eq!(peak_milliseconds(2.5f32.to_bits()), 2500);
        assert_eq!(peak_milliseconds(0), 0);
    }

    #[test]
    fn report_waits_for_motion_and_for_the_interval() {
        // Nothing seen yet: quiet.
        assert!(!report_due(40, -1, 0, 0, REPORT_INTERVAL_SECS));
        // The first report lands without waiting for the interval, because there is no previous one.
        assert!(report_due(3, -1, 12, 0, REPORT_INTERVAL_SECS));
        // Same totals means a quiet stretch, so there is nothing to say.
        assert!(!report_due(60, 3, 12, 12, REPORT_INTERVAL_SECS));
        // Motion before the interval has passed waits.
        assert!(!report_due(15, 3, 40, 12, REPORT_INTERVAL_SECS));
        assert!(report_due(24, 3, 40, 12, REPORT_INTERVAL_SECS));
    }

    #[test]
    fn probe_names_in_the_totals_line_drop_their_argument_list() {
        assert_eq!(CUTT_SKIP_RUNTIME_FRAMES.short(), "CutInTimelineController::SkipRuntime");
        assert_eq!(CUTT_GET_CURRENT_TIME.short(), "CutInTimelineController::get_CurrentTime");
    }

    #[test]
    fn a_class_label_splits_into_namespace_and_name() {
        // The two shapes this probe has to resolve: a plain `Gallop.` class, and the cut-in engine
        // under a nested namespace that a `Gallop.` lookup alone cannot find.
        let (namespace, name) = "Gallop.CutIn.Cutt.CutInTimelineController".rsplit_once('.').unwrap();
        assert_eq!(namespace, "Gallop.CutIn.Cutt");
        assert_eq!(name, "CutInTimelineController");

        let (namespace, name) = "Gallop.SingleModeUtils".rsplit_once('.').unwrap();
        assert_eq!(namespace, "Gallop");
        assert_eq!(name, "SingleModeUtils");
    }
}
