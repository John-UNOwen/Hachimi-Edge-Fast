use std::sync::{atomic::{self, AtomicBool, AtomicI32, AtomicI64, AtomicUsize}, Mutex};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{AnimationSpeed, HighSpeedSetting},
        symbols::{get_method_addr, GCHandle},
        types::*,
    },
};

static mut GET_ISFINISHED_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_IsFinished, GET_ISFINISHED_ADDR, bool, this: *mut Il2CppObject);

static mut GET_TIMELINEDATA_ADDR: usize = 0;
impl_addr_wrapper_fn!(get_TimelineData, GET_TIMELINEDATA_ADDR, *mut Il2CppObject, this: *mut Il2CppObject);

pub static CURRENT: Mutex<Option<GCHandle>> = Mutex::new(None);
static LAST_BLOCK_ID: AtomicI32 = AtomicI32::new(-1);

pub fn last_block_id() -> i32 {
    LAST_BLOCK_ID.load(atomic::Ordering::Relaxed)
}

type GotoBlockFn = extern "C" fn(this: *mut Il2CppObject, block_id: i32, weaken_cy_spring: bool, is_update: bool, is_choice: bool);
pub extern "C" fn GotoBlock(this: *mut Il2CppObject, block_id: i32, weaken_cy_spring: bool, is_update: bool, is_choice: bool) {
    if Hachimi::instance().config.load().enable_ipc {
        let mut guard = CURRENT.lock().unwrap();

        if !(*guard).as_ref().is_none_or(|h| h.target() == this) {
            *guard = Some(GCHandle::new_weak_ref(this, false));
        }
        LAST_BLOCK_ID.store(block_id, atomic::Ordering::Relaxed);
    }

    get_orig_fn!(GotoBlock, GotoBlockFn)(this, block_id, weaken_cy_spring, is_update, is_choice);
}

// The story timeline advances by `deltaTime * TimeScale`. `get_TimeScale` and
// `get_WaitFrameCountUntilNextBlock` are deliberately not touched: both are backed by
// state the game writes (there is a `set_TimeScale`, and the wait count is counted down
// internally), and scaling only the read half of a value the game also writes makes the
// factor compound instead of being applied once. These two functions are where the
// game computes the scale it then stores, so scaling here lands in the game's own state
// exactly once.
//
// Both are dumped as static: `GetTimeScaleByHighSpeedType/1 -> static float(bool)` and
// `GetTimeScaleHighSpeed/1 -> static float(bool)`. A static carries no hidden `this`, so the
// wrapper takes only the bool; a wrapper that declared an instance pointer would read the flag
// from the wrong register, which is why the matcher refused them for a whole ledger cycle (A3).
// They are installed as measurement first: the timeline speed they produce is the biggest story
// lever this class has, and a run has to report what the game returns for each flag before a
// factor is put on it. Run 5 is the reason for that order.
const STORY: AnimationSpeed::Group = AnimationSpeed::Group::Story;

static HIGH_SPEED_SCALE_CALLS: AtomicUsize = AtomicUsize::new(0);
static PLAIN_SCALE_CALLS: AtomicUsize = AtomicUsize::new(0);

fn log_scale(calls: &AtomicUsize, name: &'static str, flag: bool, value: f32) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;
    let flag = u8::from(flag);

    if calls <= STEP_DETAIL_LIMIT {
        debug!("Story scale {name}({flag}) call {calls} -> {value}");
    } else if calls % STEP_CHUNK == 0 {
        debug!("Story scale {name} {calls} calls, most recent value {value}");
    }
}

// The wrapper is class-prefixed, the way `CascadeShadow_GetShadowResolution` is separated from
// `CascadeShadowForRace_GetShadowResolution`. `disabled_hooks` keys on the bare wrapper name
// (C27), and `StoryViewController.rs` already detours a `GetTimeScaleByHighSpeedType`, so the bare
// name is one kill switch for two unrelated hooks: disabling this measurement hook would also
// disable the upstream story speed hook, and both `[DISABLED]` log lines read the same. The game's
// own method name is unchanged in the resolution string and in the log label.
type StoryTimelineController_GetTimeScaleByHighSpeedTypeFn = extern "C" fn(is_high_speed: bool) -> f32;
extern "C" fn StoryTimelineController_GetTimeScaleByHighSpeedType(is_high_speed: bool) -> f32 {
    let value = get_orig_fn!(StoryTimelineController_GetTimeScaleByHighSpeedType, StoryTimelineController_GetTimeScaleByHighSpeedTypeFn)(is_high_speed);
    log_scale(&HIGH_SPEED_SCALE_CALLS, "StoryTimelineController::GetTimeScaleByHighSpeedType", is_high_speed, value);

    value
}

type GetTimeScaleHighSpeedFn = extern "C" fn(is_high_speed: bool) -> f32;
extern "C" fn GetTimeScaleHighSpeed(is_high_speed: bool) -> f32 {
    let value = get_orig_fn!(GetTimeScaleHighSpeed, GetTimeScaleHighSpeedFn)(is_high_speed);
    log_scale(&PLAIN_SCALE_CALLS, "StoryTimelineController::GetTimeScaleHighSpeed", is_high_speed, value);

    value
}

// Pass through, counted. The int this setter receives is frame count state on the same path the
// skip hooks forward into each other (C34: `133 -> 733`, then `733 -> 1333`), and it is a
// candidate index into the game's readonly `_highSpeedFrameCountArray`. Dividing an index does
// not shorten anything, it moves the request to a different slot of that array, and
// `AnimationSpeed::scale_frame_count` floors a positive result at 1 (AnimationSpeed.rs:210-221),
// so the slot it lands on can hold a longer frame count than the entry the game chose. That is
// the run 5 failure shape: a frame target pushed past the block the timeline stands in stops
// advancement. No link here is proven safe, so this is the decision already taken for
// SkipFrameCount and SkipMotionFrame below: count the calls, log the value the game passed, and
// hand it on untouched.
type SetHighSpeedFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SetHighSpeedFrameCount(this: *mut Il2CppObject, frames: i32) {
    mark_story_activity();
    log_step(&SET_HIGH_SPEED_FRAMES_CALLS, "StoryTimelineController::SetHighSpeedFrameCount", frames);

    get_orig_fn!(SetHighSpeedFrameCount, SetHighSpeedFrameCountFn)(this, frames);
}

// The high speed story path reports its next step through two reference parameters, so both
// values only exist once the original has filled the caller's storage, and the byref flag has
// been confirmed on the resolved overload before either one is read (A7).
//
// Neither half is written back. The pair comes out of the game's own readonly
// `_highSpeedFrameCountArray`, so the int is a candidate index into that array rather than a wait
// length, and a scaled index selects a different entry - one that can hold a longer frame count,
// because `scale_frame_count` floors the result at 1. The pair is also what the game hands to the
// next link on this path, which is how C34 measured its skip chain, so a write here compounds
// along the request instead of landing once. Which single link is safe to scale is a question a
// run has to answer, not a signature. Until then the hook reads: the counter says the path ran,
// and the logged pair is the game's own value with the story factor beside it, so a run can tell
// "inert at the neutral default" from "option on, left alone".
const REF_LOG_LIMIT: usize = 6;
static REF_LOGGED: AtomicUsize = AtomicUsize::new(0);
static NULL_REF_WARNED: AtomicBool = AtomicBool::new(false);

type GetNextFrameCountHighSpeedFn = extern "C" fn(this: *mut Il2CppObject, frames: *mut f32, count: *mut i32);
extern "C" fn GetNextFrameCount_HighSpeed(this: *mut Il2CppObject, frames: *mut f32, count: *mut i32) {
    count_step(&NEXT_FRAME_CALLS, "StoryTimelineController::GetNextFrameCount_HighSpeed");

    get_orig_fn!(GetNextFrameCount_HighSpeed, GetNextFrameCountHighSpeedFn)(this, frames, count);

    if frames.is_null() || count.is_null() {
        if !NULL_REF_WARNED.swap(true, atomic::Ordering::AcqRel) {
            warn!("StoryTimelineController::GetNextFrameCount_HighSpeed returned a null reference, leaving the path alone");
        }

        return;
    }

    // Read only: the caller's storage keeps whatever the game put in it.
    let (raw_frames, raw_count) = unsafe { (*frames, *count) };

    let seen = REF_LOGGED.fetch_add(1, atomic::Ordering::Relaxed);

    if seen < REF_LOG_LIMIT {
        debug!(
            "StoryTimelineController::GetNextFrameCount_HighSpeed frames {raw_frames}, count {raw_count} (story x{}, both passed through)",
            AnimationSpeed::factor(STORY)
        );
    }
}

// Installed is not the same as called, so each story stepping path keeps a plain counter that
// logs its first few calls and then one line every STEP_CHUNK calls. This is what closes the
// question of whether a path ran at all, which a scaling log cannot answer because it only
// prints when the value changed.
const STEP_DETAIL_LIMIT: usize = 6;
const STEP_CHUNK: usize = 4096;

static SKIP_FRAME_CALLS: AtomicUsize = AtomicUsize::new(0);
static SKIP_MOTION_CALLS: AtomicUsize = AtomicUsize::new(0);
static NEXT_FRAME_CALLS: AtomicUsize = AtomicUsize::new(0);
static SET_HIGH_SPEED_FRAMES_CALLS: AtomicUsize = AtomicUsize::new(0);

fn count_step(calls: &AtomicUsize, name: &'static str) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if calls <= STEP_DETAIL_LIMIT || calls % STEP_CHUNK == 0 {
        debug!("Story step {name} call {calls}");
    }
}

// Same counter for the paths whose value is known on the way in.
fn log_step(calls: &AtomicUsize, name: &'static str, value: i32) {
    let calls = calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if calls <= STEP_DETAIL_LIMIT {
        debug!("Story step {name} call {calls}: {value}");
    } else if calls % STEP_CHUNK == 0 {
        debug!("Story step {name} {calls} calls");
    }
}

// These two paths are hooked to measure them, not to change them. A run that extended the frame
// value they carry froze the story timeline: SkipFrameCount(133) went on as 733, the game handed
// that 733 to SkipMotionFrame, and this hook raised it again to 1333. The value one call receives
// is fed into the next, so scaling either one compounds along the chain instead of landing once.
// The wait count then stopped moving at 96, UpdateTimeScaleByHispeedType stopped at 49, and
// IsHighSpeedMode ran at a flat 3600 calls per 20 s while nothing advanced. The measured values
// (133, 243, 161, 263, 691, 163) read like frame targets inside the timeline rather than wait
// lengths, so there is no multiplier here that is safe. Every argument is passed through untouched.
type SkipFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool);
extern "C" fn SkipFrameCount(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool) {
    mark_story_activity();
    log_step(&SKIP_FRAME_CALLS, "StoryTimelineController::SkipFrameCount", frames);

    get_orig_fn!(SkipFrameCount, SkipFrameCountFn)(this, frames, flag1, flag2);
}

type SkipMotionFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SkipMotionFrame(this: *mut Il2CppObject, frames: i32) {
    mark_story_activity();
    log_step(&SKIP_MOTION_CALLS, "StoryTimelineController::SkipMotionFrame", frames);

    get_orig_fn!(SkipMotionFrame, SkipMotionFrameFn)(this, frames);
}

// The game holds its story high speed mode in a static field of StoryTimelineController and
// reads it back through IsHighSpeedMode. A career run reported that mode as off the whole time
// while the saved High Speed setting sat at its maximum, so the setting and the mode the
// timeline consults are separate state, and raising the setting alone does not engage it.
//
// These three helpers are the game's own door: `SetHighSpeedType/1 -> static
// void(struct<StoryTimelineController.HighSpeedType:4B>)`, `IsHighSpeedMode/1 -> static
// bool(struct<...:4B>)` and `IsHighSpeedMode/0 -> static bool()`. A 4 byte enum travels in a
// general register, the same shape HighSpeedSetting.rs already uses for SaveHighSpeedType, so no
// enum value is invented here: the value comes from StoryManager::GetMaxHighSpeedType and the
// game's own predicate decides whether that value means high speed on this client.
static mut SET_STORY_HIGH_SPEED_TYPE_ADDR: usize = 0;
impl_addr_wrapper_fn!(SetStoryHighSpeedType, SET_STORY_HIGH_SPEED_TYPE_ADDR, (), high_speed_type: i32);

// A 4 byte enum travels in a general register, so the argument stays an i32 (A5). The return is
// declared `bool` because these are BOOLEAN targets: the matcher only hands back their addresses
// when `il2cpp_method_get_return_type` reports `IL2CPP_TYPE_BOOLEAN`, and a bool result is
// delivered in AL alone, the upper 24 bits of EAX holding whatever the callee last computed. A
// wrapper declaring `i32` branches on those undefined bits, which reads as "the mode is already
// on" for a mode the game reports off: reproduced by calling one address through both signatures,
// where the i32 read gave 0x000aae00 (nonzero, so the write was skipped) and the bool read gave
// false. An unresolved address now falls back to `false`, so the inert case is "do nothing".
static mut IS_HIGH_SPEED_MODE_VALUE_ADDR: usize = 0;
impl_addr_wrapper_fn!(IsHighSpeedModeValue, IS_HIGH_SPEED_MODE_VALUE_ADDR, bool, high_speed_type: i32);

static mut IS_HIGH_SPEED_MODE_ADDR: usize = 0;
impl_addr_wrapper_fn!(IsStoryHighSpeedMode, IS_HIGH_SPEED_MODE_ADDR, bool,);

// The static `SetHighSpeedType` writes is game state, so writing it needs the bookkeeping
// AnimationSpeed uses for the fields it rewrites: remember the value the game had, and put it
// back once when the option is turned off (AnimationSpeed.rs :136-151, :728, :756). The value
// the game last put there is recorded by the detour below, in the shape Time.rs's
// GAME_REQUESTED has and for the same reason - the state can only be read back as the value
// this module wrote. No HighSpeedType the game reports can be i32::MIN, so it stands for
// "nothing recorded yet".
const HIGH_SPEED_UNSET: i32 = i32::MIN;

static GAME_HIGH_SPEED_TYPE: AtomicI32 = AtomicI32::new(HIGH_SPEED_UNSET);
static HIGH_SPEED_LAST_WRITTEN: AtomicI32 = AtomicI32::new(HIGH_SPEED_UNSET);

// Which version of the static's state this module last wrote for, and the version the static is
// in now. The recorder below ticks the game version once per write the game itself made to
// `SetHighSpeedType`, which is the only observable change to that state: the static has no getter,
// and `IsHighSpeedMode` is a predicate over it, not the value. A write is therefore due when the
// two differ, and is not due when they match, no matter how often `IsHighSpeedMode` answers "off".
// NO_GENERATION is "this module has written nothing for any state", and it cannot collide with a
// real version because the counter starts at 0.
const NO_GENERATION: usize = usize::MAX;

static GAME_HIGH_SPEED_GENERATION: AtomicUsize = AtomicUsize::new(0);
static HIGH_SPEED_APPLIED_VALUE: AtomicI32 = AtomicI32::new(HIGH_SPEED_UNSET);
static HIGH_SPEED_APPLIED_GENERATION: AtomicUsize = AtomicUsize::new(NO_GENERATION);

// Every value this module put on the static, engage and restore alike, counted so a run can say
// how many mode switch calls it caused. The engage line carries the running total beside each
// write, so the number that A10 had to be counted by hand is printed.
static HIGH_SPEED_WRITES: AtomicUsize = AtomicUsize::new(0);

// Raised around our own call to the game's setter so the recorder below does not mistake it
// for the game's value. Time.rs's APPLYING flag is the same device.
static HIGH_SPEED_OUR_WRITE: AtomicBool = AtomicBool::new(false);

// The marker that makes the pass back to the option being off restore the static once instead
// of never, which is what `AnimationSpeed`'s `APPLIED_FACTORS` markers do for the duration
// constants.
static HIGH_SPEED_WAS_SET: AtomicBool = AtomicBool::new(false);

// A pass-through detour on the game's own setter. Same signature as the target, so the 4 byte
// enum leaves in the general register it arrived in and nothing here can misread it (A5); the
// argument is handed on untouched. Only writes the guard above marks are excluded from the
// recording, and an unarmed hook leaves the recorder with nothing to record. A game write is also
// the only evidence that the static's state changed, so it ticks the version counter the engage
// path compares its applied marker against. Without this detour no version ever moves and the
// engage path writes once, which is what a session that turned the option on after install gets.
type SetHighSpeedTypeFn = extern "C" fn(high_speed_type: i32);
extern "C" fn SetHighSpeedType(high_speed_type: i32) {
    if !HIGH_SPEED_OUR_WRITE.load(atomic::Ordering::Relaxed) {
        GAME_HIGH_SPEED_TYPE.store(high_speed_type, atomic::Ordering::Relaxed);
        GAME_HIGH_SPEED_GENERATION.fetch_add(1, atomic::Ordering::Relaxed);
    }

    get_orig_fn!(SetHighSpeedType, SetHighSpeedTypeFn)(high_speed_type);
}

// The only path that writes the static. It marks the write so the recorder above keeps the
// game's own value as the baseline, and counts it so a run can read how many mode switch calls
// this module caused. The caller decides whether the write is one the restore pass has to undo:
// the engage path raises the marker, the restore pass lowers it and clears the applied generation.
// Returns which write number this was.
fn set_story_high_speed_type(value: i32) -> usize {
    HIGH_SPEED_OUR_WRITE.store(true, atomic::Ordering::Release);
    SetStoryHighSpeedType(value);
    HIGH_SPEED_OUR_WRITE.store(false, atomic::Ordering::Release);

    HIGH_SPEED_LAST_WRITTEN.store(value, atomic::Ordering::Relaxed);

    HIGH_SPEED_WRITES.fetch_add(1, atomic::Ordering::Relaxed) + 1
}

// How many values this module has put on the static this session.
pub fn high_speed_write_count() -> usize {
    HIGH_SPEED_WRITES.load(atomic::Ordering::Relaxed)
}

// The value to put back when the game never wrote the static itself. 0 is offered only when
// this client's own predicate says 0 is not a high speed mode, and never when the predicate is
// unresolved. Kept separate from the pass below so the decision is testable, the way
// Time.rs's plan_write is.
fn pick_high_speed_baseline(recorded: i32, zero_reads_as_high_speed: Option<bool>) -> Option<i32> {
    if recorded != HIGH_SPEED_UNSET {
        return Some(recorded);
    }

    match zero_reads_as_high_speed {
        Some(false) => Some(0),
        _ => None,
    }
}

// The applied marker question, kept free of il2cpp so the sequence is testable the way Time.rs's
// plan_write and HighSpeedSetting's plan_pass are. `applied_generation` is the state version this
// module last wrote for (NO_GENERATION when it has never written), `state_generation` is the
// version the static is in now, ticked by the game's own writes. Matching versions mean the value
// this module already wrote is sitting on the state the game is standing in, so re-writing it
// would only replay the game's mode switch side effects; a differing version means the game moved
// that state and the mode has to be asked for again.
fn already_written_for_state(applied_generation: usize, state_generation: usize) -> bool {
    applied_generation != NO_GENERATION && applied_generation == state_generation
}

// Put the static back the way the game had it, once, when the option is turned off.
//
// The baseline is the last value the game itself passed to its own setter, which is safe by
// construction. If the game never wrote the static in this session there is no recorded value,
// and the only value left that this module is not inventing is one this client's own predicate
// reads as *not* a high speed mode: the engage path only wrote because IsHighSpeedMode()
// reported the mode as off, so an off value returns the static to the state the game was in
// before the write. The pass runs once whatever it decides, the way AnimationSpeed's single
// restore pass does.
fn restore_high_speed_type() {
    if !HIGH_SPEED_WAS_SET.swap(false, atomic::Ordering::AcqRel) {
        return;
    }

    // The engage marker is dropped with the write it stands for: this pass undoes that write (or
    // finds it already undone), so nothing is owed for the current state any more, and turning the
    // option back on has to be able to ask for the mode again.
    HIGH_SPEED_APPLIED_VALUE.store(HIGH_SPEED_UNSET, atomic::Ordering::Relaxed);
    HIGH_SPEED_APPLIED_GENERATION.store(NO_GENERATION, atomic::Ordering::Relaxed);

    if unsafe { SET_STORY_HIGH_SPEED_TYPE_ADDR } == 0 {
        return;
    }

    let recorded = GAME_HIGH_SPEED_TYPE.load(atomic::Ordering::Relaxed);

    // The game is only asked about 0 when there is no recorded value to restore from.
    let zero_reads_as_high_speed = if recorded == HIGH_SPEED_UNSET && unsafe { IS_HIGH_SPEED_MODE_VALUE_ADDR } != 0 {
        Some(IsHighSpeedModeValue(0))
    } else {
        None
    };

    let Some(baseline) = pick_high_speed_baseline(recorded, zero_reads_as_high_speed) else {
        warn!("StoryTimelineController: story high speed mode turned off with no value the game wrote on record, leaving the static alone");
        return;
    };

    // The static already holds the value this pass would write, because the game itself put it
    // there. Calling the setter again would be a call into game code for nothing.
    if baseline == HIGH_SPEED_LAST_WRITTEN.load(atomic::Ordering::Relaxed) {
        debug!("StoryTimelineController: story high speed type {baseline} is already the value the game left on the static");
        return;
    }

    let before = IsStoryHighSpeedMode();
    let writes = set_story_high_speed_type(baseline);

    info!(
        "StoryTimelineController: restored story high speed type {baseline} (write {writes} this session), IsHighSpeedMode {} -> {}",
        u8::from(before),
        u8::from(IsStoryHighSpeedMode())
    );
}

// The stepping paths fire in bursts when the game jumps between blocks, not on a steady cadence:
// one run stepped them at 47 s into a scene and its next game thread report was 98 s later. A
// window measured in seconds therefore reads as "no story running" in the middle of a scene. The
// window is generous so a live scene is never missed; a menu or a race never stamps it at all,
// because nothing there reaches these methods.
const STORY_ACTIVE_WINDOW_SECS: i64 = 300;
const ENGAGE_INTERVAL_SECS: i64 = 10;

static HIGH_SPEED_ENABLED: AtomicBool = AtomicBool::new(false);
static HIGH_SPEED_VALUE_REJECTED: AtomicI32 = AtomicI32::new(0);

// Whether the pass through detour that ticks the state version is actually armed. It is installed
// only while the option was on at install, so an option switched on mid session has no recorder
// and no state change it can see; that has to be readable in the log rather than inferred from
// the absence of writes.
static HIGH_SPEED_RECORDER_ARMED: AtomicBool = AtomicBool::new(false);

static STORY_START: OnceLock<Instant> = OnceLock::new();
static STORY_LAST_SEC: AtomicI64 = AtomicI64::new(-1);
static ENGAGE_LAST_SEC: AtomicI64 = AtomicI64::new(-1);

fn elapsed_secs() -> i64 {
    STORY_START.get_or_init(Instant::now).elapsed().as_secs() as i64
}

// Stamped by the stepping hooks, so it says that story timeline code is running right now.
fn mark_story_activity() {
    if HIGH_SPEED_ENABLED.load(atomic::Ordering::Relaxed) {
        STORY_LAST_SEC.store(elapsed_secs(), atomic::Ordering::Relaxed);
    }
}

// Config is read here, not in a detour, so a change lands on the game thread and the hooks only
// read an atom. The line is the proof that the option reached this layer: a run had the option on
// and produced no engagement line at all, and without this there is no way to tell whether the
// option was off or the mirror never ran.
pub fn apply_config() {
    let config = Hachimi::instance().config.load();
    let enabled = config.story_high_speed_mode;

    // The same config read as the High Speed settings half, charged to the pass that ran it.
    AnimationSpeed::note_config_read();

    if HIGH_SPEED_ENABLED.swap(enabled, atomic::Ordering::AcqRel) != enabled {
        // The recorder is armed at install, so an option switched on mid session can never see a
        // state change. Saying it here is what lets a run tell "asked once, by design" from
        // "asked once and then silently stopped".
        if enabled && !HIGH_SPEED_RECORDER_ARMED.load(atomic::Ordering::Relaxed) {
            info!("StoryTimelineController: story high speed mode option on with no recorder on the game's setter armed, the static is asked for once and not re-asked until the game writes it");
        } else {
            info!("StoryTimelineController: story high speed mode option {}", if enabled { "on" } else { "off" });
        }
    }

    // Turning the option off is the only thing that undoes the write, because the engage path
    // is gated on the option being on. Without this pass the static keeps the type this module
    // asked for until the game happens to set it again.
    if !enabled {
        restore_high_speed_type();
    }
}

// Each reason an attempt stops gets its own slot and speaks once per session. A shared budget of
// six lines was spent on the menu phase of a run before story content was reached, so the exit
// that mattered printed nothing and the run could not be read.
const REASON_WINDOW: usize = 1 << 0;
const REASON_NO_MAX: usize = 1 << 1;
const REASON_ALREADY_ON: usize = 1 << 2;
const REASON_ATTEMPT: usize = 1 << 3;
const REASON_ALREADY_WRITTEN: usize = 1 << 4;
const REASON_NO_SETTER: usize = 1 << 5;

static ENGAGE_REASONS: AtomicUsize = AtomicUsize::new(0);

fn note_engage(bit: usize, message: &str) {
    if ENGAGE_REASONS.fetch_or(bit, atomic::Ordering::AcqRel) & bit == 0 {
        debug!("StoryTimelineController: {message}");
    }
}

// Called from the game thread. It does nothing while the option is off, and only acts while the
// story stepping paths were reached inside the last window, so the static is never written from
// a menu or a race.
//
// The gate that decides whether a write is due is the applied marker, not the mode read. Reading
// `IsHighSpeedMode()` and writing whenever it answers "off" is what made this path hammer the
// static: a scene whose blocks or clips clear the mode answers "off" again every interval, so one
// scene paid the write many times over, and every one of them is a call into the game's own mode
// switch - the fade and BGM handling this class holds around high speed (`FADE_TIME_FOR_HIGH_SPEED`,
// `SetBgmClipEnabledOnSwitchHighSpeedMode`, `ForceStopBgmOnHighSpeed`). It is also A10's shape
// again, a module re-asserting a value because its idempotency test asked the wrong question.
//
// So the pass remembers what it wrote and for which state version, and a write is due only when
// the game moved that state underneath it. `IsHighSpeedMode()` stays a guard against overwriting a
// mode the game engaged on its own, not as the reason to write. The interval stays as a rate cap:
// several state changes inside one interval are one write when the interval opens.
pub fn engage_high_speed_mode() {
    if !HIGH_SPEED_ENABLED.load(atomic::Ordering::Relaxed) {
        return;
    }

    let now = elapsed_secs();
    let last_activity = STORY_LAST_SEC.load(atomic::Ordering::Relaxed);
    let last_engage = ENGAGE_LAST_SEC.load(atomic::Ordering::Relaxed);

    // No story frame has stepped in this session yet, which is the menu and loading screen case
    // where the static must not be written. Silence here is the correct behaviour.
    if last_activity < 0 {
        return;
    }

    if now - last_activity > STORY_ACTIVE_WINDOW_SECS {
        note_engage(REASON_WINDOW, "story high speed mode left alone, the story stepping paths have been quiet since the last scene");
        return;
    }

    if last_engage >= 0 && now - last_engage < ENGAGE_INTERVAL_SECS {
        return;
    }

    ENGAGE_LAST_SEC.store(now, atomic::Ordering::Relaxed);

    let target = HighSpeedSetting::GetMaxHighSpeedType();

    if target <= 0 {
        note_engage(REASON_NO_MAX, "story high speed mode left alone, StoryManager reported no high speed type in this context");
        return;
    }

    // Both are bool now, matching the BOOLEAN return of the targets, so the gates ask the game's
    // question instead of testing undefined bits of a register.
    let before = IsStoryHighSpeedMode();
    let accepted = IsHighSpeedModeValue(target);

    // The state version the decision is made against, and the value and version this module last
    // wrote for.
    let state_generation = GAME_HIGH_SPEED_GENERATION.load(atomic::Ordering::Relaxed);
    let applied_generation = HIGH_SPEED_APPLIED_GENERATION.load(atomic::Ordering::Relaxed);
    let applied_value = HIGH_SPEED_APPLIED_VALUE.load(atomic::Ordering::Relaxed);

    // The one line that carries the numbers the decision is made from. Without it a run that
    // writes nothing cannot be told apart from a run that never looked. Printed as 0/1 so a run
    // reads the same way as the earlier logs.
    if ENGAGE_REASONS.load(atomic::Ordering::Relaxed) & REASON_ATTEMPT == 0 {
        note_engage(REASON_ATTEMPT, &format!(
            "story high speed attempt, StoryManager max {target}, IsHighSpeedMode {}, IsHighSpeedMode({target}) {}, story state {state_generation}, written for state {applied_generation}",
            u8::from(before),
            u8::from(accepted),
        ));
    }

    if before {
        note_engage(REASON_ALREADY_ON, "story high speed mode left alone, the game already reports the mode as on");
        return;
    }

    if !accepted {
        if HIGH_SPEED_VALUE_REJECTED.swap(target, atomic::Ordering::AcqRel) != target {
            warn!("StoryTimelineController: this client does not read HighSpeedType {target} as a high speed mode, leaving the story timeline alone");
        }

        return;
    }

    // Nothing can be written, so nothing may be remembered as written: recording an applied
    // marker for a call that never reached the game would make every later pass believe the mode
    // was owed to it.
    if unsafe { SET_STORY_HIGH_SPEED_TYPE_ADDR } == 0 {
        note_engage(REASON_NO_SETTER, "story high speed mode left alone, StoryTimelineController::SetHighSpeedType did not resolve");
        return;
    }

    // The applied marker gate: this module already put its value on the state the static is in
    // now, and the game has not written that state since. Writing again would be the repeated
    // identical write, so the pass stops here and says what it already wrote, with the count of
    // writes it did make.
    if already_written_for_state(applied_generation, state_generation) {
        note_engage(REASON_ALREADY_WRITTEN, &format!(
            "story high speed type {applied_value} already written for story state {state_generation}, leaving it alone, {} writes so far",
            HIGH_SPEED_WRITES.load(atomic::Ordering::Relaxed),
        ));

        return;
    }

    let writes = set_story_high_speed_type(target);
    HIGH_SPEED_WAS_SET.store(true, atomic::Ordering::Release);
    HIGH_SPEED_APPLIED_VALUE.store(target, atomic::Ordering::Relaxed);
    HIGH_SPEED_APPLIED_GENERATION.store(state_generation, atomic::Ordering::Relaxed);

    info!(
        "StoryTimelineController: asked the story timeline for high speed type {target} (write {writes}, story state {state_generation}), IsHighSpeedMode {} -> {}",
        u8::from(before),
        u8::from(IsStoryHighSpeedMode())
    );
}

// Every spelling of the HighSpeedType parameter that a wrapper declaring an `i32` could be right
// about. The dump spells it `struct<StoryTimelineController.HighSpeedType:4B>`; `enum` is the same
// value type under the other spelling il2cpp metadata uses, and `class` is left on the list so a
// client that spells the parameter as a reference is named in the log instead of quietly matching
// nothing. It is refused there rather than bound: `SetStoryHighSpeedType` and
// `IsHighSpeedModeValue` pass an `i32`, and a reference parameter wants an object pointer in that
// register.
const HIGH_SPEED_TYPE_CANDIDATES: [Il2CppTypeEnum; 3] = [
    Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE,
    Il2CppTypeEnum_IL2CPP_TYPE_ENUM,
    Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
];

// Try the spellings in order through the matcher that proves the parameter travels by value, and
// report the address together with the candidate that bound, so the install line can name it.
unsafe fn resolve_high_speed_value_method(
    class: *mut Il2CppClass,
    name: &str,
    ret: Il2CppTypeEnum,
) -> Option<(usize, Il2CppTypeEnum)> {
    for candidate in HIGH_SPEED_TYPE_CANDIDATES {
        let addr = AnimationSpeed::resolve_static_value_method(class, name, &[candidate], ret);

        if addr != 0 {
            return Some((addr, candidate));
        }
    }

    None
}

pub fn init(umamusume: *const Il2CppImage) {
    get_class_or_return!(umamusume, Gallop, StoryTimelineController);

    let GotoBlock_addr = get_method_addr(StoryTimelineController, c"GotoBlock", 4);

    new_hook!(GotoBlock_addr, GotoBlock);

    unsafe {
        GET_ISFINISHED_ADDR = get_method_addr(StoryTimelineController, c"get_IsFinished", 0);
        GET_TIMELINEDATA_ADDR = get_method_addr(StoryTimelineController, c"get_TimelineData", 0);
    }

    let by_high_speed_addr = unsafe { AnimationSpeed::resolve_static_method(
        StoryTimelineController, "GetTimeScaleByHighSpeedType",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_R4,
    ) };
    if by_high_speed_addr != 0 { new_hook!(by_high_speed_addr, StoryTimelineController_GetTimeScaleByHighSpeedType); }

    let high_speed_addr = unsafe { AnimationSpeed::resolve_static_method(
        StoryTimelineController, "GetTimeScaleHighSpeed",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_R4,
    ) };
    if high_speed_addr != 0 { new_hook!(high_speed_addr, GetTimeScaleHighSpeed); }

    let set_frames_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "SetHighSpeedFrameCount",
        &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if set_frames_addr != 0 { new_hook!(set_frames_addr, SetHighSpeedFrameCount); }

    // Dumped as `GetNextFrameCount_HighSpeed/2 -> void(float&, int&)`, one overload, and the
    // element types stay R4 and I4 with the reference marked by the byref bit.
    let next_frames_addr = unsafe { AnimationSpeed::resolve_ref_method(
        StoryTimelineController, "GetNextFrameCount_HighSpeed",
        &[Il2CppTypeEnum_IL2CPP_TYPE_R4, Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if next_frames_addr != 0 { new_hook!(next_frames_addr, GetNextFrameCount_HighSpeed); }

    let skip_frames_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "SkipFrameCount",
        &[
            Il2CppTypeEnum_IL2CPP_TYPE_I4,
            Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
            Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
        ],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if skip_frames_addr != 0 { new_hook!(skip_frames_addr, SkipFrameCount); }

    let skip_motion_addr = unsafe { AnimationSpeed::resolve_method(
        StoryTimelineController, "SkipMotionFrame",
        &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if skip_motion_addr != 0 { new_hook!(skip_motion_addr, SkipMotionFrame); }

    // The dump spells the enum parameter as `struct<StoryTimelineController.HighSpeedType:4B>`.
    // Both targets sit behind wrappers that declare an `i32`, so the parameter is only safe if it
    // really is a value type of at most 4 bytes: the matcher measures the class behind the spelling
    // and refuses anything bigger, refuses `class<...>` outright, and prints the shape it accepted.
    // Which candidate bound is printed in the install line below, so a `class` install can never be
    // mistaken for a `struct` one.
    let set_type = unsafe {
        resolve_high_speed_value_method(
            StoryTimelineController, "SetHighSpeedType", Il2CppTypeEnum_IL2CPP_TYPE_VOID,
        )
    };

    if let Some((addr, _)) = set_type {
        unsafe { SET_STORY_HIGH_SPEED_TYPE_ADDR = addr };
    }

    let mode_value = unsafe {
        resolve_high_speed_value_method(
            StoryTimelineController, "IsHighSpeedMode", Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
        )
    };

    if let Some((addr, _)) = mode_value {
        unsafe { IS_HIGH_SPEED_MODE_VALUE_ADDR = addr };
    }

    unsafe {
        IS_HIGH_SPEED_MODE_ADDR = AnimationSpeed::resolve_static_method(
            StoryTimelineController, "IsHighSpeedMode", &[], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
        );
    }

    let helpers = unsafe {
        (
            SET_STORY_HIGH_SPEED_TYPE_ADDR != 0,
            IS_HIGH_SPEED_MODE_VALUE_ADDR != 0,
            IS_HIGH_SPEED_MODE_ADDR != 0,
        )
    };

    // The recorder that makes the write reversible is installed only while the option is on,
    // the way StoryFrameProbe installs its probes only under debug_mode: at the neutral default
    // this module adds no detour to a game path at all. It is the only way to learn what the
    // game had in the static, because the value is observable from the game's own call and from
    // nowhere else. An option turned on mid session gets no recorder, and the restore pass then
    // works from its verified fallback.
    let set_high_speed_addr = unsafe { SET_STORY_HIGH_SPEED_TYPE_ADDR };
    let recorder = set_high_speed_addr != 0 && Hachimi::instance().config.load().story_high_speed_mode;

    if recorder {
        new_hook!(set_high_speed_addr, SetHighSpeedType);
    }

    HIGH_SPEED_RECORDER_ARMED.store(recorder, atomic::Ordering::Release);

    debug!(
        "StoryTimelineController: story high speed helpers setter {} ({}), value predicate {} ({}), state reader {}, baseline recorder {}",
        helpers.0, AnimationSpeed::candidate_word(set_type.map(|(_, candidate)| candidate)),
        helpers.1, AnimationSpeed::candidate_word(mode_value.map(|(_, candidate)| candidate)),
        helpers.2, u8::from(recorder)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // The baseline the restore pass writes back. `recorded` is the last value the game itself
    // passed to SetHighSpeedType (HIGH_SPEED_UNSET when it never wrote one) and
    // `zero_reads_as_high_speed` is the game's own answer to IsHighSpeedMode(0), None when the
    // predicate is unresolved.
    #[test]
    fn the_value_the_game_wrote_is_the_baseline_and_the_predicate_is_never_consulted() {
        assert_eq!(pick_high_speed_baseline(2, None), Some(2));
        assert_eq!(pick_high_speed_baseline(2, Some(true)), Some(2));
        assert_eq!(pick_high_speed_baseline(0, Some(true)), Some(0));
    }

    #[test]
    fn a_value_that_is_not_the_game_is_only_used_when_the_game_wrote_nothing() {
        assert_eq!(pick_high_speed_baseline(HIGH_SPEED_UNSET, Some(false)), Some(0), "0 the client reads as no high speed mode");
        assert_eq!(pick_high_speed_baseline(HIGH_SPEED_UNSET, Some(true)), None, "even 0 means high speed here");
        assert_eq!(pick_high_speed_baseline(HIGH_SPEED_UNSET, None), None, "no predicate to ask");
    }

    #[test]
    fn the_marker_only_matches_the_state_it_was_written_for() {
        assert!(!already_written_for_state(NO_GENERATION, 0), "nothing has been written yet");
        assert!(already_written_for_state(0, 0), "written for the state the static is in");
        assert!(!already_written_for_state(0, 1), "the game moved the state underneath the write");
        assert!(already_written_for_state(7, 7), "written for the state after seven game writes");
        assert!(!already_written_for_state(NO_GENERATION, NO_GENERATION), "the sentinel is never a match");
    }

    // The engage and restore sequence without il2cpp. `mode_on` is what IsHighSpeedMode() answers,
    // which is not the static: run 6 polled that predicate 19737 times and read 0 in every sample
    // while the timeline was running, so a mode read is not evidence about what the static holds.
    // `generation` is GAME_HIGH_SPEED_GENERATION, ticked only by the recorder detour on a write the
    // game made itself, and the other fields are this module's markers. `engage` mirrors the gate
    // order of engage_high_speed_mode and `restore` mirrors restore_high_speed_type, both calling
    // the same decision functions the production paths call.
    struct Sim {
        mode_on: bool,
        recorded: i32,
        generation: usize,
        applied_value: i32,
        applied_generation: usize,
        last_written: i32,
        was_set: bool,
        recorder: bool,
        setter_resolved: bool,
        zero_is_high_speed: bool,
        writes: Vec<i32>,
    }

    impl Sim {
        fn new(recorder: bool) -> Self {
            Self {
                mode_on: false,
                recorded: HIGH_SPEED_UNSET,
                generation: 0,
                applied_value: HIGH_SPEED_UNSET,
                applied_generation: NO_GENERATION,
                last_written: HIGH_SPEED_UNSET,
                was_set: false,
                recorder,
                setter_resolved: true,
                zero_is_high_speed: false,
                writes: Vec::new(),
            }
        }

        // What this client's IsHighSpeedMode(value) predicate answers for a HighSpeedType.
        fn reads_as_high_speed(&self, value: i32) -> bool {
            if value == 0 { self.zero_is_high_speed } else { value > 0 }
        }

        // The game's own write to the static. The recorder sees it and ticks the state version;
        // with no recorder armed the module has no way to see it and the version stands still.
        fn game_write(&mut self, value: i32) {
            self.mode_on = self.reads_as_high_speed(value);

            if self.recorder {
                self.recorded = value;
                self.generation += 1;
            }
        }

        // A block or clip boundary where the game stops reporting the mode as on without going
        // through the setter the recorder watches. This is the shape that used to drive a write
        // every interval inside a single scene.
        fn clip_boundary(&mut self) {
            self.mode_on = false;
        }

        fn engage(&mut self, target: i32) {
            let before = self.mode_on;
            let accepted = self.reads_as_high_speed(target);

            if before || !accepted {
                return;
            }

            // The production gate that refuses to remember a write the game never received.
            if !self.setter_resolved {
                return;
            }

            if already_written_for_state(self.applied_generation, self.generation) {
                return;
            }

            self.writes.push(target);
            self.last_written = target;
            self.was_set = true;
            self.applied_value = target;
            self.applied_generation = self.generation;
            self.mode_on = self.reads_as_high_speed(target);
        }

        // The gate this fix replaced: write whenever the mode read says off. Kept here so the
        // tests below are a reproduction and not just a description of the new behaviour.
        fn engage_with_no_applied_marker(&mut self, target: i32) {
            if self.mode_on || !self.reads_as_high_speed(target) {
                return;
            }

            self.writes.push(target);
            self.last_written = target;
            self.was_set = true;
            self.mode_on = self.reads_as_high_speed(target);
        }

        fn restore(&mut self) {
            if !self.was_set {
                return;
            }

            self.was_set = false;
            self.applied_value = HIGH_SPEED_UNSET;
            self.applied_generation = NO_GENERATION;

            let zero = if self.recorded == HIGH_SPEED_UNSET { Some(self.zero_is_high_speed) } else { None };

            let Some(baseline) = pick_high_speed_baseline(self.recorded, zero) else {
                return;
            };

            if baseline == self.last_written {
                return;
            }

            self.writes.push(baseline);
            self.last_written = baseline;
            self.mode_on = self.reads_as_high_speed(baseline);
        }
    }

    #[test]
    fn one_interval_of_a_scene_that_clears_the_mode_used_to_pay_one_write_each_time() {
        // 40 passes over 400 s inside one scene, the mode cleared at every clip boundary.
        let mut sim = Sim::new(true);

        for _ in 0..40 {
            sim.engage_with_no_applied_marker(2);
            sim.clip_boundary();
        }

        assert_eq!(sim.writes.len(), 40, "the reproduction did not reproduce: {:?}", sim.writes);
    }

    #[test]
    fn one_scene_that_clears_the_mode_is_asked_once_not_once_per_interval() {
        let mut sim = Sim::new(true);

        for _ in 0..40 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert_eq!(sim.writes, vec![2], "the game's mode switch ran {} times inside one scene", sim.writes.len());
    }

    #[test]
    fn a_state_the_game_really_changed_is_asked_again() {
        let mut sim = Sim::new(true);

        sim.engage(2);
        assert_eq!(sim.writes, vec![2]);

        sim.game_write(0);   // the game moved the static: a real state change
        sim.engage(2);
        assert_eq!(sim.writes, vec![2, 2], "a state the game changed did not earn a second ask");

        for _ in 0..10 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert_eq!(sim.writes, vec![2, 2], "the second write re-applied itself {} times", sim.writes.len() - 2);
    }

    #[test]
    fn a_mode_the_game_engaged_itself_is_never_written_over() {
        let mut sim = Sim::new(true);

        sim.game_write(2);   // the game put high speed on and its predicate agrees

        for _ in 0..5 {
            sim.engage(2);
        }

        assert!(sim.writes.is_empty(), "the module wrote over the mode the game chose: {:?}", sim.writes);
    }

    #[test]
    fn a_value_this_client_does_not_read_as_high_speed_is_never_written() {
        let mut sim = Sim::new(true);

        for _ in 0..5 {
            sim.engage(0);
        }

        assert!(sim.writes.is_empty());
        assert_eq!(sim.applied_generation, NO_GENERATION);
    }

    #[test]
    fn a_write_that_could_not_reach_the_game_is_not_remembered_as_written() {
        // The setter address did not resolve: nothing was written, so nothing may be recorded as
        // owed, and every later pass has to keep trying rather than believe its marker.
        let mut sim = Sim::new(true);
        sim.setter_resolved = false;

        for _ in 0..5 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert!(sim.writes.is_empty());
        assert_eq!(sim.applied_generation, NO_GENERATION, "a write that never happened was remembered");
    }

    #[test]
    fn with_no_recorder_armed_the_static_is_asked_for_once_for_the_whole_session() {
        // The option turned on after install: no detour on the game's setter, so no state change
        // is observable and the honest behaviour is one ask, not a write per interval.
        let mut sim = Sim::new(false);

        for _ in 0..30 {
            sim.game_write(0);   // invisible to this module
            sim.engage(2);
            sim.clip_boundary();
        }

        assert_eq!(sim.writes, vec![2], "without a recorder the pass wrote {} times", sim.writes.len());
    }

    #[test]
    fn turning_the_option_off_clears_the_marker_so_a_later_scene_can_ask_again() {
        let mut sim = Sim::new(true);

        sim.engage(2);
        sim.restore();
        assert_eq!(sim.writes, vec![2, 0], "the restore did not put the value the game held back");
        assert_eq!(sim.applied_value, HIGH_SPEED_UNSET, "the restore left the applied value on record");
        assert_eq!(sim.applied_generation, NO_GENERATION, "the restore left the applied state on record");

        for _ in 0..5 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert_eq!(sim.writes, vec![2, 0, 2], "the option coming back on could not ask for the mode");
    }

    #[test]
    fn the_write_counter_numbers_the_writes_a_run_reads() {
        let before = high_speed_write_count();

        // The increment set_story_high_speed_type makes for every value it hands the game's setter.
        let numbers: Vec<usize> = (0..3).map(|_| HIGH_SPEED_WRITES.fetch_add(1, atomic::Ordering::Relaxed) + 1).collect();

        assert_eq!(high_speed_write_count() - before, 3, "a write went uncounted");
        assert_eq!(numbers.len(), 3);
        assert!(numbers.windows(2).all(|pair| pair[1] == pair[0] + 1), "the numbers are not a running total: {numbers:?}");
        assert!(numbers[0] > before, "the first write number did not come after the writes already counted");
    }
}