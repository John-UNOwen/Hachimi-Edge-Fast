use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;
use std::fmt::Write as _;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed, SceneManager,
            TrainingCuttProbe::{
                bucket_for, class_for_label, current_view_id, flag_bit, peak_seconds, report_due, CutProbe,
                BUCKET_COUNT, BUCKET_NAMES, REPORT_INTERVAL_SECS,
            },
        },
        types::*,
    },
};

// Observe only hooks over the cut-in a story event plays, the family of short animated beats a story or
// event screen drops into the middle of its text.
//
// The training probe measures the career screen's own cut controller. A story event cut-in does not go
// through it: it goes through `Gallop.CutInHelper`, the cut-in door the story, event and mode screens
// share, and through the static extension doors in
// `Gallop.SingleModeTrainingCutHelperExtension.ContextExtension`, which is where the game drives a whole
// set of cut-ins at once. Run 9 measured 7.8 s on `Story` and 7.6 s on the succession event screen with
// no number for what any of that cut-in time was, so the fork has no baseline for the story side at all.
//
// The C47 lesson is why every target here is spelled out of the dump rather than guessed from a name: a
// probe built on a boundary the game never calls reports zero, and zero looks like a fast game. Every
// signature below was read from `introspect.log` before it was hooked.
//
// What this cannot answer yet: `Gallop.StoryViewController`, `Gallop.StorySceneController`,
// `Gallop.StoryEventMissionViewController` and `Gallop.StoryCharacterFade` are real classes in this
// client's metadata, and their skip and autoplay doors are where a story event option would live, but no
// dump has ever printed their signatures. They are in the `debug_mode` allowlist now, so the next run
// delivers them and a second pass can hook those doors the same way.
//
// Nothing here writes anything back. Installed only when debug_mode is on, after the scaling modules.
const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const STRING: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_STRING;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;

const NO_PARAMS: &[Il2CppTypeEnum] = &[];
// `Pause/2 -> void(bool, bool)` on the base helper (dump L26430).
const TWO_BOOLS: &[Il2CppTypeEnum] = &[BOOL, BOOL];
// `OnTerminateRuntime/1 -> void(class<Gallop.CutIn.Cutt.CutInTimelineController>)`: a reference held as an
// address and handed back untouched.
const ONE_CLASS: &[Il2CppTypeEnum] = &[CLASS];
// `set_Status/1 -> void(struct<Gallop.CutInHelper.CutInStatus:4B>)`. Four byte, proven by the dump, and the
// kind A5 confirms travels in a general purpose register.
const VALUETYPE: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE;
const ONE_VALUETYPE: &[Il2CppTypeEnum] = &[VALUETYPE];
// `Play/2 -> void(string<System.String>, class<UnityEngine.Transform>)`. The name is only printed, never
// dereferenced, so this client holds it as an address and hands it back untouched.
const NAME_AND_PARENT: &[Il2CppTypeEnum] = &[STRING, CLASS];
// `SetCurrentFrame/1 -> void(int)` and `FixedUpdateForHighSpeed/1 -> void(float)`.
const ONE_INT: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_I4];
const ONE_FLOAT: &[Il2CppTypeEnum] = &[R4];
// The static extension doors take a `generic<IList<SingleModeTrainingCutInHelper>>`, which reports
// GENERICINST rather than CLASS, so they resolve through the generic matcher (C48).
const HELPERS: &[Il2CppTypeEnum] = &[CLASS];
const HELPERS_AND_VALUE: &[Il2CppTypeEnum] = &[CLASS, R4];

static CUT_IN_PLAY: CutProbe = CutProbe::counted("CutInHelper::Play(timeline, parent)");
static CUT_IN_ON_PLAY_CUT_IN: CutProbe = CutProbe::counted("CutInHelper::OnPlayCutIn(timeline, parent)");
static CUT_IN_IS_PLAYING: CutProbe = CutProbe::peaked("CutInHelper::IsPlaying()");
static CUT_IN_GET_TOTAL_TIME: CutProbe = CutProbe::peaked("CutInHelper::GetTotalTime()");
static CUT_IN_GET_TARGET_SPEED: CutProbe = CutProbe::peaked("CutInHelper::GetTargetSpeed()");
static CUT_IN_FIXED_UPDATE_HIGH_SPEED: CutProbe = CutProbe::peaked("CutInHelper::FixedUpdateForHighSpeed(rate)");
static CUT_IN_SET_CURRENT_FRAME: CutProbe = CutProbe::peaked("CutInHelper::SetCurrentFrame(frame)");
static CUT_IN_CLEANUP_PLAYING: CutProbe = CutProbe::counted("CutInHelper::CleanupPlaying()");
static CONTEXT_SKIP_RUNTIME_ALL: CutProbe = CutProbe::counted("ContextExtension::SkipRuntimeAll(helpers)");
static CONTEXT_SKIP_PAUSE: CutProbe = CutProbe::counted("ContextExtension::SkipPause(helpers)");
static CONTEXT_SET_TIME_ALL: CutProbe = CutProbe::peaked("ContextExtension::SetTimeAll(helpers, time)");
static CONTEXT_FIXED_UPDATE_HIGH_SPEED: CutProbe = CutProbe::counted("ContextExtension::FixedUpdateForHighSpeed(helpers)");
static CONTEXT_FIXED_UPDATE_HIGH_SPEED_RATE: CutProbe = CutProbe::peaked("ContextExtension::FixedUpdateForHighSpeed(helpers, rate)");
static CONTEXT_GET_CURRENT_TIME: CutProbe = CutProbe::peaked("ContextExtension::GetCurrentTime(helpers)");
// The lifecycle, pause and status doors on the base helper. Run 10 measured a busy cut-in engine with no
// opener, and these are the doors the dump names that no hook has ever asked for (C50). `set_Status` takes
// `struct<Gallop.CutInHelper.CutInStatus:4B>`, a four byte enum of the kind A5 confirms travels in a general
// register, and this wrapper only passes it back.
static CUT_IN_ON_START: CutProbe = CutProbe::counted("CutInHelper::OnStart()");
static CUT_IN_ON_END: CutProbe = CutProbe::counted("CutInHelper::OnEnd()");
static CUT_IN_ON_END_CUT_IN: CutProbe = CutProbe::counted("CutInHelper::OnEndCutIn()");
static CUT_IN_STOP_REQUEST: CutProbe = CutProbe::counted("CutInHelper::StopRequest()");
static CUT_IN_PAUSE: CutProbe = CutProbe::counted("CutInHelper::Pause(play, pause)");
static CUT_IN_IS_PAUSE: CutProbe = CutProbe::peaked("CutInHelper::IsPause()");
static CUT_IN_SET_STATUS: CutProbe = CutProbe::peaked("CutInHelper::set_Status(status)");
static CUT_IN_ON_RESTART: CutProbe = CutProbe::counted("CutInHelper::OnRestart()");
// The derived training helper declares its own zero argument entry set, which is why the base
// `Play/2(string, Transform)` stayed silent all through run 10 (C50).
static TRAINING_CUT_IN_PLAY: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::Play()");
static TRAINING_CUT_IN_ON_PLAY_CUT_IN: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::OnPlayCutIn()");
static TRAINING_CUT_IN_ON_START_CUT_IN: CutProbe = CutProbe::peaked("SingleModeTrainingCutInHelper::OnStartCutIn()");
static TRAINING_CUT_IN_ON_PLAY_MAIN: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::OnPlayMainCutIn()");
static TRAINING_CUT_IN_ON_END_CUT_IN: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::OnEndCutIn()");
static TRAINING_CUT_IN_CLEANUP_PLAYING: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::CleanupPlaying()");
static TRAINING_CUT_IN_ON_TERMINATE_RUNTIME: CutProbe = CutProbe::counted("SingleModeTrainingCutInHelper::OnTerminateRuntime(controller)");
static TRAINING_CUT_IN_ON_TIMELINE_UPDATE_POST: CutProbe = CutProbe::peaked("SingleModeTrainingCutInHelper::OnTimelineUpdatePost(time)");

static PROBES: [&CutProbe; 30] = [
    &CUT_IN_PLAY,
    &CUT_IN_ON_PLAY_CUT_IN,
    &CUT_IN_IS_PLAYING,
    &CUT_IN_GET_TOTAL_TIME,
    &CUT_IN_GET_TARGET_SPEED,
    &CUT_IN_FIXED_UPDATE_HIGH_SPEED,
    &CUT_IN_SET_CURRENT_FRAME,
    &CUT_IN_CLEANUP_PLAYING,
    &CONTEXT_SKIP_RUNTIME_ALL,
    &CONTEXT_SKIP_PAUSE,
    &CONTEXT_SET_TIME_ALL,
    &CONTEXT_FIXED_UPDATE_HIGH_SPEED,
    &CONTEXT_FIXED_UPDATE_HIGH_SPEED_RATE,
    &CONTEXT_GET_CURRENT_TIME,
    &CUT_IN_ON_START,
    &CUT_IN_ON_END,
    &CUT_IN_ON_END_CUT_IN,
    &CUT_IN_STOP_REQUEST,
    &CUT_IN_PAUSE,
    &CUT_IN_IS_PAUSE,
    &CUT_IN_SET_STATUS,
    &CUT_IN_ON_RESTART,
    &TRAINING_CUT_IN_PLAY,
    &TRAINING_CUT_IN_ON_PLAY_CUT_IN,
    &TRAINING_CUT_IN_ON_START_CUT_IN,
    &TRAINING_CUT_IN_ON_PLAY_MAIN,
    &TRAINING_CUT_IN_ON_END_CUT_IN,
    &TRAINING_CUT_IN_CLEANUP_PLAYING,
    &TRAINING_CUT_IN_ON_TERMINATE_RUNTIME,
    &TRAINING_CUT_IN_ON_TIMELINE_UPDATE_POST,
];

// The pairing is stated from dump doors, not inferred from a class name, and each run records which door
// opened it and which closed it. C47 and C49 are what a named boundary costs, so the log has to say.
const OPEN_DOOR_NAMES: [&str; 5] = [
    "CutInHelper::Play",
    "CutInHelper::OnStart",
    "SingleModeTrainingCutInHelper::Play",
    "SingleModeTrainingCutInHelper::OnPlayCutIn",
    "CutInHelper::OnRestart",
];
const CLOSE_DOOR_NAMES: [&str; 6] = [
    "CutInHelper::CleanupPlaying",
    "CutInHelper::OnEnd",
    "CutInHelper::OnEndCutIn",
    "SingleModeTrainingCutInHelper::OnEndCutIn",
    "SingleModeTrainingCutInHelper::CleanupPlaying",
    "SingleModeTrainingCutInHelper::OnTerminateRuntime",
];
static RUN_OPENED_MS: AtomicI64 = AtomicI64::new(-1);
static RUN_OPEN_DOOR: AtomicUsize = AtomicUsize::new(0);
static RUN_CLOSE_DOOR: AtomicUsize = AtomicUsize::new(0);
static RUN_VIEW_ID: AtomicI32 = AtomicI32::new(0);
static RUN_BUCKET: AtomicUsize = AtomicUsize::new(BUCKET_COUNT - 1);
static RUNS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static RUN_WALL_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static RUN_BUCKET_COUNTS: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static RUN_BUCKET_VIEW: [AtomicI32; BUCKET_COUNT] = [const { AtomicI32::new(0) }; BUCKET_COUNT];
static RUN_BUCKET_WALL_MS: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

// A cut-in run only exists when the close lands after its open. A negative open means nothing was
// playing, and a close that lands before the open is the game clearing a flag that was already clear.
fn paired_run_milliseconds(opened: i64, now: i64) -> Option<i64> {
    if opened < 0 || now < opened {
        return None;
    }

    Some(now - opened)
}

// A run keeps the open it got first. Two doors can open the same cut-in, and moving the start clock to the
// later one would report a shorter cut than the game took.
fn open_story_run(door: usize) {
    let now = elapsed_ms();

    if now < 0 || RUN_OPENED_MS.load(atomic::Ordering::Relaxed) >= 0 {
        return;
    }

    let view = current_view_id();

    RUN_OPENED_MS.store(now, atomic::Ordering::Relaxed);
    RUN_OPEN_DOOR.store(door, atomic::Ordering::Relaxed);
    RUN_VIEW_ID.store(view, atomic::Ordering::Relaxed);
    RUN_BUCKET.store(bucket_for(view, SceneManager::is_race_scene_family()) as usize, atomic::Ordering::Relaxed);
}

fn close_story_run(door: usize) {
    let opened = RUN_OPENED_MS.swap(-1, atomic::Ordering::Relaxed);
    let open_door = RUN_OPEN_DOOR.load(atomic::Ordering::Relaxed);
    let run_ms = match paired_run_milliseconds(opened, elapsed_ms()) {
        Some(run_ms) => run_ms,
        None => return,
    };

    let bucket = RUN_BUCKET.load(atomic::Ordering::Relaxed);
    let view = RUN_VIEW_ID.load(atomic::Ordering::Relaxed);
    let runs = RUNS_CLOSED.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    RUN_CLOSE_DOOR.store(door, atomic::Ordering::Relaxed);
    RUN_WALL_MS_TOTAL.fetch_add(run_ms, atomic::Ordering::Relaxed);
    RUN_BUCKET_COUNTS[bucket].fetch_add(1, atomic::Ordering::Relaxed);
    RUN_BUCKET_WALL_MS[bucket].fetch_add(run_ms, atomic::Ordering::Relaxed);
    RUN_BUCKET_VIEW[bucket].store(view, atomic::Ordering::Relaxed);

    if runs <= 8 {
        info!("Story event probe: cut-in {} closed after {run_ms} ms from {} to {} on view {view} {}", runs, OPEN_DOOR_NAMES[open_door], CLOSE_DOOR_NAMES[door], BUCKET_NAMES[bucket]);
    }
}

type CutInPlayFn = extern "C" fn(this: *mut Il2CppObject, timeline: *mut Il2CppObject, parent: *mut Il2CppObject);
// Dumped: `Play/2 -> void(string<System.String>, class<UnityEngine.Transform>)`. Both arguments are
// references, held as addresses and passed straight back.
extern "C" fn CutInHelper_Play(this: *mut Il2CppObject, timeline: *mut Il2CppObject, parent: *mut Il2CppObject) {
    CUT_IN_PLAY.count();
    open_story_run(0);

    get_orig_fn!(CutInHelper_Play, CutInPlayFn)(this, timeline, parent);
}

extern "C" fn CutInHelper_OnPlayCutIn(this: *mut Il2CppObject, timeline: *mut Il2CppObject, parent: *mut Il2CppObject) {
    CUT_IN_ON_PLAY_CUT_IN.count();

    get_orig_fn!(CutInHelper_OnPlayCutIn, CutInPlayFn)(this, timeline, parent);
}

type CutInBoolFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
extern "C" fn CutInHelper_IsPlaying(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(CutInHelper_IsPlaying, CutInBoolFn)(this);
    CUT_IN_IS_PLAYING.sample(flag_bit(value));

    value
}

type CutInFloatFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
// `GetTotalTime/0 -> float()`: the length the cut-in timeline reports for itself. This is the number a
// story event speed option would have to be judged against, and nothing else in the fork measures it.
extern "C" fn CutInHelper_GetTotalTime(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CutInHelper_GetTotalTime, CutInFloatFn)(this);
    CUT_IN_GET_TOTAL_TIME.sample(value);

    value
}

extern "C" fn CutInHelper_GetTargetSpeed(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CutInHelper_GetTargetSpeed, CutInFloatFn)(this);
    CUT_IN_GET_TARGET_SPEED.sample(value);

    value
}

type CutInRateFn = extern "C" fn(this: *mut Il2CppObject, rate: f32);
// `FixedUpdateForHighSpeed/1 -> void(float)`: the game's own high speed driving of a cut-in, with the
// rate it is driving at. Sampling the argument is what tells the fork whether a story cut-in already has a
// fast path, which is the question item 41 has to answer before anything is scaled.
extern "C" fn CutInHelper_FixedUpdateForHighSpeed(this: *mut Il2CppObject, rate: f32) {
    CUT_IN_FIXED_UPDATE_HIGH_SPEED.observe(&[rate as f64]);
    CUT_IN_FIXED_UPDATE_HIGH_SPEED.sample(rate);

    get_orig_fn!(CutInHelper_FixedUpdateForHighSpeed, CutInRateFn)(this, rate);
}

type CutInFrameFn = extern "C" fn(this: *mut Il2CppObject, frame: i32);
// `SetCurrentFrame/1 -> void(int)`: whether the game jumps a cut-in forward on its own. The frame index
// is sampled as a peak and written back untouched.
extern "C" fn CutInHelper_SetCurrentFrame(this: *mut Il2CppObject, frame: i32) {
    CUT_IN_SET_CURRENT_FRAME.observe(&[frame as f64]);
    CUT_IN_SET_CURRENT_FRAME.sample(frame as f32);

    get_orig_fn!(CutInHelper_SetCurrentFrame, CutInFrameFn)(this, frame);
}

type CutInVoidFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn CutInHelper_CleanupPlaying(this: *mut Il2CppObject) {
    CUT_IN_CLEANUP_PLAYING.count();
    close_story_run(0);

    get_orig_fn!(CutInHelper_CleanupPlaying, CutInVoidFn)(this);
}

// Dumped on `Gallop.CutInHelper`: `OnStart/0 -> void()` (L26462), `OnEnd/0 -> void()` (L26463),
// `OnEndCutIn/0 -> void()` (L26438), `OnRestart/0 -> void()` (L26434), `StopRequest/0 -> void()` (L26428).
// These are the lifecycle the base helper runs around a cut-in and no hook has ever asked for them. Which
// of them this client actually walks through, next to `Play` and `CleanupPlaying`, is what C50 needs to know
// before anything on this screen is paired or scaled.
extern "C" fn CutInHelper_OnStart(this: *mut Il2CppObject) {
    CUT_IN_ON_START.count();
    open_story_run(1);

    get_orig_fn!(CutInHelper_OnStart, CutInVoidFn)(this);
}

extern "C" fn CutInHelper_OnEnd(this: *mut Il2CppObject) {
    CUT_IN_ON_END.count();
    close_story_run(1);

    get_orig_fn!(CutInHelper_OnEnd, CutInVoidFn)(this);
}

extern "C" fn CutInHelper_OnEndCutIn(this: *mut Il2CppObject) {
    CUT_IN_ON_END_CUT_IN.count();
    close_story_run(2);

    get_orig_fn!(CutInHelper_OnEndCutIn, CutInVoidFn)(this);
}

extern "C" fn CutInHelper_OnRestart(this: *mut Il2CppObject) {
    CUT_IN_ON_RESTART.count();
    open_story_run(4);

    get_orig_fn!(CutInHelper_OnRestart, CutInVoidFn)(this);
}

// `StopRequest/0 -> void()`: the game asking its own cut-in to end. Counted only. A cut-in that the game is
// already ending is the one place a speed option would be free, so the count comes before the idea.
extern "C" fn CutInHelper_StopRequest(this: *mut Il2CppObject) {
    CUT_IN_STOP_REQUEST.count();

    get_orig_fn!(CutInHelper_StopRequest, CutInVoidFn)(this);
}

type CutInPauseFn = extern "C" fn(this: *mut Il2CppObject, play: bool, second: bool);
// Dumped: `Pause/2 -> void(bool, bool)` (L26430) with `IsPause/0 -> bool()` (L26431). The game's own pause
// door for a cut-in. Both booleans are handed back exactly as they arrived and neither is read here, so the
// only thing this detour can change is nothing (C50, item 41).
extern "C" fn CutInHelper_Pause(this: *mut Il2CppObject, play: bool, second: bool) {
    CUT_IN_PAUSE.count();

    get_orig_fn!(CutInHelper_Pause, CutInPauseFn)(this, play, second);
}

extern "C" fn CutInHelper_IsPause(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(CutInHelper_IsPause, CutInBoolFn)(this);
    CUT_IN_IS_PAUSE.sample(flag_bit(value));

    value
}

type CutInStatusFn = extern "C" fn(this: *mut Il2CppObject, status: u32);
// Dumped: `set_Status/1 -> void(struct<Gallop.CutInHelper.CutInStatus:4B>)` (L26410). A four byte enum is
// proven to travel in a general purpose register (A5), so the wrapper declares it as one word, passes it
// back untouched, and samples the value it was given. That turns the cut-in's own state machine into a set
// of numbers this fork can read instead of guess.
extern "C" fn CutInHelper_SetStatus(this: *mut Il2CppObject, status: u32) {
    CUT_IN_SET_STATUS.sample(status as f32);

    get_orig_fn!(CutInHelper_SetStatus, CutInStatusFn)(this, status);
}

type CutInControllerFn = extern "C" fn(this: *mut Il2CppObject, controller: *mut Il2CppObject);
// Dumped on `Gallop.SingleModeTrainingCutInHelper`: `OnTerminateRuntime/1 -> void(class<
// Gallop.CutIn.Cutt.CutInTimelineController>)` (L26595). The controller is held as an address and returned
// to the original untouched.
extern "C" fn TrainingCutInHelper_OnTerminateRuntime(this: *mut Il2CppObject, controller: *mut Il2CppObject) {
    TRAINING_CUT_IN_ON_TERMINATE_RUNTIME.count();
    close_story_run(5);

    get_orig_fn!(TrainingCutInHelper_OnTerminateRuntime, CutInControllerFn)(this, controller);
}

// `OnTimelineUpdatePost/1 -> void(float)` (L26607): the per frame post update a training cut-in runs, with
// the time value it is stepped by. The argument is sampled as a peak and written back untouched.
extern "C" fn TrainingCutInHelper_OnTimelineUpdatePost(this: *mut Il2CppObject, time: f32) {
    TRAINING_CUT_IN_ON_TIMELINE_UPDATE_POST.observe(&[time as f64]);
    TRAINING_CUT_IN_ON_TIMELINE_UPDATE_POST.sample(time);

    get_orig_fn!(TrainingCutInHelper_OnTimelineUpdatePost, CutInRateFn)(this, time);
}

// The entry set the derived helper declares for itself (dump L26598 to L26604). The base `Play/2` stayed
// silent through a whole career because this is the door the training cut-in comes through (C50).
extern "C" fn TrainingCutInHelper_Play(this: *mut Il2CppObject) {
    TRAINING_CUT_IN_PLAY.count();
    open_story_run(2);

    get_orig_fn!(TrainingCutInHelper_Play, CutInVoidFn)(this);
}

extern "C" fn TrainingCutInHelper_OnPlayCutIn(this: *mut Il2CppObject) {
    TRAINING_CUT_IN_ON_PLAY_CUT_IN.count();
    open_story_run(3);

    get_orig_fn!(TrainingCutInHelper_OnPlayCutIn, CutInVoidFn)(this);
}

// `OnStartCutIn/0 -> bool()` (L26600): the game's own answer to whether the cut-in may start at all. Read,
// reported as a peak of 1.0 when it ever said yes, and returned untouched.
extern "C" fn TrainingCutInHelper_OnStartCutIn(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(TrainingCutInHelper_OnStartCutIn, CutInBoolFn)(this);
    TRAINING_CUT_IN_ON_START_CUT_IN.sample(flag_bit(value));

    value
}

extern "C" fn TrainingCutInHelper_OnPlayMainCutIn(this: *mut Il2CppObject) {
    TRAINING_CUT_IN_ON_PLAY_MAIN.count();

    get_orig_fn!(TrainingCutInHelper_OnPlayMainCutIn, CutInVoidFn)(this);
}

extern "C" fn TrainingCutInHelper_OnEndCutIn(this: *mut Il2CppObject) {
    TRAINING_CUT_IN_ON_END_CUT_IN.count();
    close_story_run(3);

    get_orig_fn!(TrainingCutInHelper_OnEndCutIn, CutInVoidFn)(this);
}

extern "C" fn TrainingCutInHelper_CleanupPlaying(this: *mut Il2CppObject) {
    TRAINING_CUT_IN_CLEANUP_PLAYING.count();
    close_story_run(4);

    get_orig_fn!(TrainingCutInHelper_CleanupPlaying, CutInVoidFn)(this);
}

type ContextListFn = extern "C" fn(helpers: *mut Il2CppObject);
// Dumped: `SkipRuntimeAll/1 -> static void(generic<IList<SingleModeTrainingCutInHelper>>)`. A static
// target has no hidden `this`, so the wrapper declares only the dumped argument (A3), and the argument is
// a generic instantiation held as an address (C48).
extern "C" fn ContextExtension_SkipRuntimeAll(helpers: *mut Il2CppObject) {
    CONTEXT_SKIP_RUNTIME_ALL.count();

    get_orig_fn!(ContextExtension_SkipRuntimeAll, ContextListFn)(helpers);
}

extern "C" fn ContextExtension_SkipPause(helpers: *mut Il2CppObject) {
    CONTEXT_SKIP_PAUSE.count();

    get_orig_fn!(ContextExtension_SkipPause, ContextListFn)(helpers);
}

extern "C" fn ContextExtension_FixedUpdateForHighSpeed(helpers: *mut Il2CppObject) {
    CONTEXT_FIXED_UPDATE_HIGH_SPEED.count();

    get_orig_fn!(ContextExtension_FixedUpdateForHighSpeed, ContextListFn)(helpers);
}

type ContextListValueFn = extern "C" fn(helpers: *mut Il2CppObject, value: f32);
// `SetTimeAll/2 -> static void(generic<IList<...>>, float)` and
// `FixedUpdateForHighSpeed/2 -> static void(generic<IList<...>>, float)`. The float is the second slot in
// the mixed calling convention and is only read, never written.
extern "C" fn ContextExtension_SetTimeAll(helpers: *mut Il2CppObject, time: f32) {
    CONTEXT_SET_TIME_ALL.observe(&[time as f64]);
    CONTEXT_SET_TIME_ALL.sample(time);

    get_orig_fn!(ContextExtension_SetTimeAll, ContextListValueFn)(helpers, time);
}

extern "C" fn ContextExtension_FixedUpdateForHighSpeedRate(helpers: *mut Il2CppObject, rate: f32) {
    CONTEXT_FIXED_UPDATE_HIGH_SPEED_RATE.observe(&[rate as f64]);
    CONTEXT_FIXED_UPDATE_HIGH_SPEED_RATE.sample(rate);

    get_orig_fn!(ContextExtension_FixedUpdateForHighSpeedRate, ContextListValueFn)(helpers, rate);
}

type ContextGetTimeFn = extern "C" fn(helpers: *mut Il2CppObject) -> f32;
// `GetCurrentTime/1 -> static float(generic<IEnumerable<...>>)`: how far the set of cut-ins has got,
// reported by the game.
extern "C" fn ContextExtension_GetCurrentTime(helpers: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(ContextExtension_GetCurrentTime, ContextGetTimeFn)(helpers);
    CONTEXT_GET_CURRENT_TIME.sample(value);

    value
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());

    let cut_in_helper = class_for_label(umamusume, "Gallop.CutInHelper");
    let training_cut_in_helper = class_for_label(umamusume, "Gallop.SingleModeTrainingCutInHelper");
    let context_extension = class_for_label(umamusume, "Gallop.SingleModeTrainingCutHelperExtension.ContextExtension");

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;

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

    macro_rules! static_generic_probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_static_generic_ref_method(class, $method, $params, $ret) };

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

    probe!(cut_in_helper, CutInHelper_Play, "Play", NAME_AND_PARENT, VOID, "CutInHelper::Play");
    probe!(cut_in_helper, CutInHelper_OnPlayCutIn, "OnPlayCutIn", NAME_AND_PARENT, VOID, "CutInHelper::OnPlayCutIn");
    probe!(cut_in_helper, CutInHelper_IsPlaying, "IsPlaying", NO_PARAMS, BOOL, "CutInHelper::IsPlaying");
    probe!(cut_in_helper, CutInHelper_GetTotalTime, "GetTotalTime", NO_PARAMS, R4, "CutInHelper::GetTotalTime");
    probe!(cut_in_helper, CutInHelper_GetTargetSpeed, "GetTargetSpeed", NO_PARAMS, R4, "CutInHelper::GetTargetSpeed");
    probe!(cut_in_helper, CutInHelper_FixedUpdateForHighSpeed, "FixedUpdateForHighSpeed", ONE_FLOAT, VOID, "CutInHelper::FixedUpdateForHighSpeed(rate)");
    probe!(cut_in_helper, CutInHelper_SetCurrentFrame, "SetCurrentFrame", ONE_INT, VOID, "CutInHelper::SetCurrentFrame");
    probe!(cut_in_helper, CutInHelper_CleanupPlaying, "CleanupPlaying", NO_PARAMS, VOID, "CutInHelper::CleanupPlaying");
    probe!(cut_in_helper, CutInHelper_OnStart, "OnStart", NO_PARAMS, VOID, "CutInHelper::OnStart");
    probe!(cut_in_helper, CutInHelper_OnEnd, "OnEnd", NO_PARAMS, VOID, "CutInHelper::OnEnd");
    probe!(cut_in_helper, CutInHelper_OnEndCutIn, "OnEndCutIn", NO_PARAMS, VOID, "CutInHelper::OnEndCutIn");
    probe!(cut_in_helper, CutInHelper_OnRestart, "OnRestart", NO_PARAMS, VOID, "CutInHelper::OnRestart");
    probe!(cut_in_helper, CutInHelper_StopRequest, "StopRequest", NO_PARAMS, VOID, "CutInHelper::StopRequest");
    probe!(cut_in_helper, CutInHelper_Pause, "Pause", TWO_BOOLS, VOID, "CutInHelper::Pause(play, pause)");
    probe!(cut_in_helper, CutInHelper_IsPause, "IsPause", NO_PARAMS, BOOL, "CutInHelper::IsPause");
    probe!(cut_in_helper, CutInHelper_SetStatus, "set_Status", ONE_VALUETYPE, VOID, "CutInHelper::set_Status");

    probe!(training_cut_in_helper, TrainingCutInHelper_Play, "Play", NO_PARAMS, VOID, "SingleModeTrainingCutInHelper::Play");
    probe!(training_cut_in_helper, TrainingCutInHelper_OnPlayCutIn, "OnPlayCutIn", NO_PARAMS, VOID, "SingleModeTrainingCutInHelper::OnPlayCutIn");
    probe!(training_cut_in_helper, TrainingCutInHelper_OnStartCutIn, "OnStartCutIn", NO_PARAMS, BOOL, "SingleModeTrainingCutInHelper::OnStartCutIn");
    probe!(training_cut_in_helper, TrainingCutInHelper_OnPlayMainCutIn, "OnPlayMainCutIn", NO_PARAMS, VOID, "SingleModeTrainingCutInHelper::OnPlayMainCutIn");
    probe!(training_cut_in_helper, TrainingCutInHelper_OnEndCutIn, "OnEndCutIn", NO_PARAMS, VOID, "SingleModeTrainingCutInHelper::OnEndCutIn");
    probe!(training_cut_in_helper, TrainingCutInHelper_CleanupPlaying, "CleanupPlaying", NO_PARAMS, VOID, "SingleModeTrainingCutInHelper::CleanupPlaying");
    probe!(training_cut_in_helper, TrainingCutInHelper_OnTerminateRuntime, "OnTerminateRuntime", ONE_CLASS, VOID, "SingleModeTrainingCutInHelper::OnTerminateRuntime");
    probe!(training_cut_in_helper, TrainingCutInHelper_OnTimelineUpdatePost, "OnTimelineUpdatePost", ONE_FLOAT, VOID, "SingleModeTrainingCutInHelper::OnTimelineUpdatePost");

    static_generic_probe!(context_extension, ContextExtension_SkipRuntimeAll, "SkipRuntimeAll", HELPERS, VOID, "ContextExtension::SkipRuntimeAll");
    static_generic_probe!(context_extension, ContextExtension_SkipPause, "SkipPause", HELPERS, VOID, "ContextExtension::SkipPause");
    static_generic_probe!(context_extension, ContextExtension_FixedUpdateForHighSpeed, "FixedUpdateForHighSpeed", HELPERS, VOID, "ContextExtension::FixedUpdateForHighSpeed(helpers)");
    static_generic_probe!(context_extension, ContextExtension_FixedUpdateForHighSpeedRate, "FixedUpdateForHighSpeed", HELPERS_AND_VALUE, VOID, "ContextExtension::FixedUpdateForHighSpeed(helpers, rate)");
    static_generic_probe!(context_extension, ContextExtension_SetTimeAll, "SetTimeAll", HELPERS_AND_VALUE, VOID, "ContextExtension::SetTimeAll");
    static_generic_probe!(context_extension, ContextExtension_GetCurrentTime, "GetCurrentTime", HELPERS, R4, "ContextExtension::GetCurrentTime");

    info!("Story event probe: {installed} doors installed, {} of them carry a counted kind in the totals line, cut-in length read from the cut-in engine and attributed to the view it plays on", PROBES.len());

    if !missing.is_empty() {
        info!("Story event probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the other probe reports.
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

        if probe.is_peaked() {
            let _ = write!(line, " {}={} peak {:.3}", probe.label(), calls, peak_seconds(probe.peak_bits()));
        }
        else {
            let _ = write!(line, " {}={}", probe.label(), calls);
        }
    }

    let mut buckets = String::new();

    for index in 0..BUCKET_COUNT {
        let count = RUN_BUCKET_COUNTS[index].load(atomic::Ordering::Relaxed);

        if count == 0 {
            continue;
        }

        let _ = write!(
            buckets,
            " {}(view {}) runs {count} wall {} ms",
            BUCKET_NAMES[index],
            RUN_BUCKET_VIEW[index].load(atomic::Ordering::Relaxed),
            RUN_BUCKET_WALL_MS[index].load(atomic::Ordering::Relaxed)
        );
    }

    let runs = RUNS_CLOSED.load(atomic::Ordering::Relaxed);
    let wall_ms = RUN_WALL_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let opened = RUN_OPENED_MS.load(atomic::Ordering::Relaxed);
    let open_for = match paired_run_milliseconds(opened, elapsed_ms()) {
        Some(open_for) => open_for,
        None => 0,
    };

    info!("Story event probe totals at {now_sec} s:{line} cut-ins {runs} wall {wall_ms} ms open {open_for} ms");
    info!("Story event probe cut-ins by screen:{buckets}");
}

#[cfg(test)]
mod tests {
    use super::*;

    // Only the tests spell a view out by name; the probe itself reads the id from the game.
    use crate::il2cpp::hook::umamusume::{SceneDefine::ViewId, TrainingCuttProbe::ViewBucket};

    #[test]
    fn a_cut_in_run_needs_an_open_that_the_close_follows() {
        assert_eq!(paired_run_milliseconds(-1, 500), None);
        // A close that lands before the open is a flag being cleared that was already clear.
        assert_eq!(paired_run_milliseconds(900, 500), None);
        assert_eq!(paired_run_milliseconds(500, 500), Some(0));
        assert_eq!(paired_run_milliseconds(500, 3_500), Some(3_000));
    }

    #[test]
    fn a_story_event_cut_in_is_attributed_to_the_story_event_screen() {
        assert_eq!(bucket_for(ViewId::StoryEventMission as i32, false), ViewBucket::StoryEvent);
        assert_eq!(bucket_for(ViewId::Story as i32, false), ViewBucket::Story);
        // A race scene still wins over a view id, because a cut-in inside a race is not a story beat.
        assert_eq!(bucket_for(ViewId::StoryEventMission as i32, true), ViewBucket::Race);
    }

    #[test]
    fn no_hook_is_measured_under_two_names() {
        // The two `FixedUpdateForHighSpeed` overloads are separate doors, and a totals line that printed
        // them under one name would read as a single count.
        let mut names: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();
        let total = names.len();

        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), total);
    }

    #[test]
    fn every_pairing_door_has_a_name_the_run_line_can_print() {
        // The wrappers hand `close_story_run` and `open_story_run` a fixed index into these two tables. An
        // index past the end would panic inside a detour, which is a panic across FFI (C2), so the tables
        // have to cover every door the pairing uses and the tests have to say how many that is.
        assert_eq!(OPEN_DOOR_NAMES.len(), 5);
        assert_eq!(CLOSE_DOOR_NAMES.len(), 6);
        assert_eq!(PROBES.len(), OPEN_DOOR_NAMES.len() + CLOSE_DOOR_NAMES.len() + 19);
    }
}
