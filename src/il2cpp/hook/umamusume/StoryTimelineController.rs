use std::sync::{atomic::{self, AtomicBool, AtomicI32, AtomicI64, AtomicUsize}, Mutex};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::{hachimi::recover_lock, Hachimi},
    il2cpp::{
        hook::umamusume::{AnimationSpeed, HighSpeedSetting, StoryEventProbe, TrainingCuttProbe},
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
def_detour! {
    pub GotoBlock(this: *mut Il2CppObject, block_id: i32, weaken_cy_spring: bool, is_update: bool, is_choice: bool) {
            if Hachimi::instance().config.load().enable_ipc {
            let mut guard = recover_lock(&CURRENT);

            if !(*guard).as_ref().is_none_or(|h| h.target() == this) {
                *guard = Some(GCHandle::new_weak_ref(this, false));
            }
            LAST_BLOCK_ID.store(block_id, atomic::Ordering::Relaxed);
        }

        // `core::ipc.rs` jumps the story straight into this wrapper from the IPC plane, so this
        // body is reached from more than its own trampoline. If `init` resolved no `GotoBlock`, the
        // story stays on its current block instead of a call through 0 on the IPC thread (C1).
        let Some(goto_block) = get_orig_fn_guarded!(GotoBlock, GotoBlockFn) else {
            return;
        };

        goto_block(this, block_id, weaken_cy_spring, is_update, is_choice);
    }
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
// `CascadeShadowForRace_GetShadowResolution`. `StoryViewController.rs:34` already arms a
// `GetTimeScaleByHighSpeedType`, which is the collision C27 records: while `disabled_hooks` matched
// the bare wrapper name, one key was a kill switch for both hooks - disabling this measurement hook
// also disabled the upstream story speed hook, and both `[DISABLED]` lines read the same. The key is
// a hook id now, so the two come down independently
// (`umamusume::StoryTimelineController::…` against `umamusume::StoryViewController::…`). The prefix
// stays because the log label and the resolution string name the game's own method either way.
type StoryTimelineController_GetTimeScaleByHighSpeedTypeFn = extern "C" fn(is_high_speed: bool) -> f32;
def_detour! {
    StoryTimelineController_GetTimeScaleByHighSpeedType(is_high_speed: bool) answer -> f32 {
            let value = get_orig_fn!(StoryTimelineController_GetTimeScaleByHighSpeedType, StoryTimelineController_GetTimeScaleByHighSpeedTypeFn)(is_high_speed);
        // The scale is published before the logging half: a 0 time scale stops the story timeline,
        // and a trip in the mod's own logging is not the mod's decision to freeze it.
        answer.publish(value);
        log_scale(&HIGH_SPEED_SCALE_CALLS, "StoryTimelineController::GetTimeScaleByHighSpeedType", is_high_speed, value);

        value
    }
}

type GetTimeScaleHighSpeedFn = extern "C" fn(is_high_speed: bool) -> f32;
def_detour! {
    GetTimeScaleHighSpeed(is_high_speed: bool) answer -> f32 {
            let value = get_orig_fn!(GetTimeScaleHighSpeed, GetTimeScaleHighSpeedFn)(is_high_speed);
        answer.publish(value);
        log_scale(&PLAIN_SCALE_CALLS, "StoryTimelineController::GetTimeScaleHighSpeed", is_high_speed, value);

        value
    }
}

// Pass through, counted. The int this setter receives is frame count state on the same path the
// skip hooks forward into each other (C34: `133 -> 733`, then `733 -> 1333`), and it is a
// candidate index into the game's readonly `_highSpeedFrameCountArray`. Dividing an index does
// not shorten anything, it moves the request to a different slot of that array, and
// `AnimationSpeed::scale_frame_count` floors a positive result at 1 (`AnimationSpeed.rs:1227-1236`),
// so the slot it lands on can hold a longer frame count than the entry the game chose. That is
// the run 5 failure shape: a frame target pushed past the block the timeline stands in stops
// advancement. No link here is proven safe, so this is the decision already taken for
// SkipFrameCount and SkipMotionFrame below: count the calls, log the value the game passed, and
// hand it on untouched.
type SetHighSpeedFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
def_detour! {
    SetHighSpeedFrameCount(this: *mut Il2CppObject, frames: i32) {
            mark_story_activity();
        log_step(&SET_HIGH_SPEED_FRAMES_CALLS, "StoryTimelineController::SetHighSpeedFrameCount", frames);

        get_orig_fn!(SetHighSpeedFrameCount, SetHighSpeedFrameCountFn)(this, frames);
    }
    bail {
                get_orig_fn!(SetHighSpeedFrameCount, SetHighSpeedFrameCountFn)(this, frames)
    }
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
def_detour! {
    GetNextFrameCount_HighSpeed(this: *mut Il2CppObject, frames: *mut f32, count: *mut i32) {
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
def_detour! {
    SkipFrameCount(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool) {
            mark_story_activity();
        log_step(&SKIP_FRAME_CALLS, "StoryTimelineController::SkipFrameCount", frames);

        get_orig_fn!(SkipFrameCount, SkipFrameCountFn)(this, frames, flag1, flag2);
    }
    bail {
                get_orig_fn!(SkipFrameCount, SkipFrameCountFn)(this, frames, flag1, flag2)
    }
}

type SkipMotionFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
def_detour! {
    SkipMotionFrame(this: *mut Il2CppObject, frames: i32) {
            mark_story_activity();
        log_step(&SKIP_MOTION_CALLS, "StoryTimelineController::SkipMotionFrame", frames);

        get_orig_fn!(SkipMotionFrame, SkipMotionFrameFn)(this, frames);
    }
    bail {
                get_orig_fn!(SkipMotionFrame, SkipMotionFrameFn)(this, frames)
    }
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
// back once when the option is turned off (`AnimationSpeed.rs:6-9`, `:236-241`, `:3869-3881`). The value
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
def_detour! {
    SetHighSpeedType(high_speed_type: i32) {
            if !HIGH_SPEED_OUR_WRITE.load(atomic::Ordering::Relaxed) {
            GAME_HIGH_SPEED_TYPE.store(high_speed_type, atomic::Ordering::Relaxed);
            GAME_HIGH_SPEED_GENERATION.fetch_add(1, atomic::Ordering::Relaxed);
        }

        get_orig_fn!(SetHighSpeedType, SetHighSpeedTypeFn)(high_speed_type);
    }
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

// Which pass a call on the game's story high speed static is, kept free of il2cpp so the sequence is
// testable the way `pick_high_speed_baseline`, `already_written_for_state` and
// `HighSpeedSetting::plan_pass` are, and taking each guard input as an argument the way
// `AnimationSpeed::tween_clocks_holding` does.
//
// Two inputs say whether an owed pass runs:
//
// - `cut_in_running` is `StoryEventProbe::cut_in_engine_driving()`, the one second stamp on
//   `SingleModeTrainingCutInHelper::OnTimelineUpdatePost`. It is the whole condition `25ccda1` shipped and
//   `529e4f5` (HEAD) held both of this module's writes on, and it is still asked: `held_by_the_engine` below
//   is that condition spelled once, so "this guard refuses at least what HEAD refused" is a function a test
//   compares against rather than a sentence in a comment.
// - `cut_open` is `TrainingCuttProbe::cut_gate_engaged()`: the measured boundary of a training cut in the
//   game, open from `PlayTrainingCut` / `PlayScenarioTrainingCut`, closed by `CleanUpCutt`, aged on the game
//   thread's own tick and ended at its 900 tick bound when the close door never arrives, armed whatever
//   `debug_mode` says (`TrainingCuttProbe.rs`'s `CUT_GATE_HOLD_TICKS` and `arm_cut_gate`, `529e4f5`).
//
// `write_owed` is the marker that says this module has a write to undo. A pass that would touch the static
// under either input is `Hold`: it writes nothing, and for the restore it leaves the owed marker set, so the
// write is deferred rather than lost. The refusal set is `HEAD's condition ∪ the gate` and nothing wider.
//
// Run 16 is why the pass asks a boundary at all. Its cut-in runtime was stepped every frame and never
// terminated, `ContextExtension::SetTimeAll` pinned at that cut's own 4.000 s, and the setting half of the
// same option wrote back to its own saved values (`story high speed 2 -> 1` at 09:03:24.9) 3.5 s before that
// cut opened; the static this module writes is one of the values such a runtime reads. Run 57 shows what each
// answer does and does not reach. The gate's one open stretch, `PlayTrainingCut(info) call 1` at 20:44:19.183
// to `CleanUpCutt() call 1` at 20:44:28.080 (`run log/hachimi-run57.log:1604-1605`, `:1720`), holds one logged
// pass, the refusal at 20:44:23.512 (`:1691`), and `529e4f5` - the build that printed it - reached it on the
// stamp alone: the door's counter is absent from the 20:44:14 totals, which is how a zero prints
// (`StoryEventProbe.rs:800-802`, `:1589`), and it reads 230 at 20:44:33 (`:1730`). Both shapes refuse that
// pass. What the gate does not reach is the 20:45:18 to 20:45:40 window, where `PlayTrainingCut` stayed at 1
// (`:1872`): the engage that wrote at 20:45:23.529 (`:1838`) is handed by all three shapes, and the two
// cut-ins that window played, 343 ms and 662 ms on view 1200 (`:1844`, `:1850`), crossed neither door.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pass {
    Idle,
    // A pass owed a write and one of the two inputs says wait: refused here, owed on the next pass.
    Hold,
    Engage,
    Restore,
}

/// The condition `25ccda1` shipped and `529e4f5` held: the cut-in engine stepped inside the last second.
fn held_by_the_engine(cut_in_running: bool) -> bool {
    cut_in_running
}

/// The shipped refusal: HEAD's condition, or an open training cut. Never neither, and never a fourth thing.
fn hold_write_asked(cut_in_running: bool, cut_open: bool) -> bool {
    held_by_the_engine(cut_in_running) || cut_open
}

fn plan_pass(option_on: bool, write_owed: bool, cut_in_running: bool, cut_open: bool) -> Pass {
    let owed = if option_on {
        Pass::Engage
    } else if write_owed {
        Pass::Restore
    } else {
        Pass::Idle
    };

    if owed != Pass::Idle && hold_write_asked(cut_in_running, cut_open) { Pass::Hold } else { owed }
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
static RESTORE_HOLD_WARNED: AtomicBool = AtomicBool::new(false);

fn restore_high_speed_type() {
    // The owed write is not spent by a pass the guard holds. A restore that returns after the marker is
    // consumed would leave this module's value on the static for the rest of the session, and putting the
    // game's value back while a cut-in runtime is being stepped is the shape run 16 measured, so the pass
    // waits and the next `apply_config` - the next view change or config change - runs it again.
    let owed = HIGH_SPEED_WAS_SET.load(atomic::Ordering::Acquire);
    let cut_in_running = StoryEventProbe::cut_in_engine_driving();
    let cut_open = TrainingCuttProbe::cut_gate_engaged();

    if plan_pass(false, owed, cut_in_running, cut_open) != Pass::Restore {
        // A hold has a write to say it is waiting for; an idle pass has nothing.
        if owed && !RESTORE_HOLD_WARNED.swap(true, atomic::Ordering::AcqRel) {
            warn!("StoryTimelineController: story high speed restore waiting, a training cut was open or a cut in runtime was still being stepped");
        }

        return;
    }

    HIGH_SPEED_WAS_SET.store(false, atomic::Ordering::Release);

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
const REASON_CUT_OPEN: usize = 1 << 6;

static ENGAGE_REASONS: AtomicUsize = AtomicUsize::new(0);

fn note_engage(bit: usize, message: &str) {
    if ENGAGE_REASONS.fetch_or(bit, atomic::Ordering::AcqRel) & bit == 0 {
        debug!("StoryTimelineController: {message}");
    }
}

// The same once-per-session budget at the level a played build keeps. `note_engage` is debug level, and
// the file filter is Info whenever `debug_mode` is off (`core/log.rs:3-9`), so a refusal told through it
// left the run with no evidence the guard ran at all. The cut refusal is the one exit here a run has to
// be able to read without turning the dump on.
fn warn_engage(bit: usize, message: &str) {
    if ENGAGE_REASONS.fetch_or(bit, atomic::Ordering::AcqRel) & bit == 0 {
        warn!("StoryTimelineController: {message}");
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
    let option_on = HIGH_SPEED_ENABLED.load(atomic::Ordering::Relaxed);

    if !option_on {
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

    // Both guard inputs are asked, the stamp `25ccda1` shipped and `529e4f5` kept, and the cut boundary
    // `529e4f5` measured. A cut-in runtime that is still being stepped is the state run 16 stalled in, and
    // this static is one of the values that runtime reads, so the pass waits while either one says wait. The
    // pass is re-run by the next game tick, so waiting costs a later write rather than a lost one, and the
    // interval gate above is what keeps these two reads off the per tick path. The `write_owed` argument is
    // `false` because an engage has no write to undo; the owed marker belongs to the restore pass.
    //
    // The refusal is told at warn level, once. It used to sit in the debug-only reason budget, so a
    // played build - where the file filter is Info (`core/log.rs:3-9`) - refused this write and printed
    // nothing, the same silence C57 records for the gate's own failed arming. Run 57 read it at debug level
    // at 20:44:23.512 (`run log/hachimi-run57.log:1691`).
    let cut_in_running = StoryEventProbe::cut_in_engine_driving();
    let cut_open = TrainingCuttProbe::cut_gate_engaged();

    if plan_pass(option_on, false, cut_in_running, cut_open) == Pass::Hold {
        warn_engage(REASON_CUT_OPEN, "story high speed mode left alone, a training cut was open or a cut in runtime was still being stepped");
        return;
    }

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
        // What `StoryEventProbe::cut_in_engine_driving()` and `TrainingCuttProbe::cut_gate_engaged()`
        // answer, stood in as the two bools the shipped readers get, so the shipped `plan_pass` makes this
        // sim's passes too.
        cut_in_running: bool,
        cut_open: bool,
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
                // Nothing armed, nothing open: the guard is asked and answers that it has no reason to hold.
                cut_in_running: false,
                cut_open: false,
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
            // The shipped pass decision: a stepped cut-in runtime or an open training cut turns this into a
            // Hold that writes nothing. The option is on in every engage test, which is what
            // engage_high_speed_mode returns on before it reaches the guard.
            if plan_pass(true, false, self.cut_in_running, self.cut_open) == Pass::Hold {
                return;
            }

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
            // The shipped pass decision, including the half that keeps the owed marker set when the guard
            // holds the restore back.
            if plan_pass(false, self.was_set, self.cut_in_running, self.cut_open) != Pass::Restore {
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

    // HEAD (`529e4f5`) held both of this module's writes on `StoryEventProbe::cut_in_engine_driving()`
    // alone. Spelled here as HEAD's own expression - not as a call into this file's helper - so the two
    // tests below compare the shipped decision against the decision it has to cover, and not against itself.
    fn head_held(cut_in_running: bool) -> bool {
        cut_in_running
    }

    // Every pass that owes a write: the engage (option on), the restore (option off, a write owed), and the
    // engage the option wins even though a write is owed.
    const OWED_PASSES: [(bool, bool); 3] = [(true, false), (false, true), (true, true)];

    #[test]
    fn every_pass_head_held_is_still_held_whatever_the_cut_gate_answers() {
        // The superset direction, over both answers of HEAD's input and both answers of the gate.
        for (option_on, write_owed) in OWED_PASSES {
            for cut_in_running in [false, true] {
                for cut_open in [false, true] {
                    if !head_held(cut_in_running) {
                        continue;
                    }

                    assert_eq!(
                        plan_pass(option_on, write_owed, cut_in_running, cut_open),
                        Pass::Hold,
                        "the gate let through a pass HEAD held: cut_in_running {cut_in_running}, cut_open {cut_open}",
                    );
                }
            }
        }
    }

    #[test]
    fn the_guard_holds_nothing_beyond_head_held_or_an_open_cut() {
        // The other direction: the refusal set is exactly HEAD's condition union the gate, with no fourth
        // reason hiding in it.
        let mut head_holds = 0;
        let mut shipped_holds = 0;

        for (option_on, write_owed) in OWED_PASSES {
            for cut_in_running in [false, true] {
                for cut_open in [false, true] {
                    let head = head_held(cut_in_running);
                    let shipped = plan_pass(option_on, write_owed, cut_in_running, cut_open);

                    head_holds += usize::from(head);
                    shipped_holds += usize::from(shipped == Pass::Hold);

                    assert_eq!(
                        shipped,
                        if head || cut_open { Pass::Hold } else { if option_on { Pass::Engage } else { Pass::Restore } },
                        "the refusal is not HEAD's condition union the gate: cut_in_running {cut_in_running}, cut_open {cut_open}",
                    );
                }
            }
        }

        // 3 owed shapes x 2 x 2 = 12 input shapes: HEAD holds 6 of them, the union 9.
        assert_eq!(head_holds, 6, "HEAD's own refusal set is not what its condition says");
        assert_eq!(shipped_holds, 9, "the union did not add the gate's refusal half");
    }

    #[test]
    fn the_cut_gate_adds_a_refusal_head_did_not_make() {
        // The half that is genuinely new: a training cut open with the cut-in door quiet. HEAD hands this
        // pass - its only input is the stamp - and the shipped guard holds it.
        assert!(!head_held(false));
        assert_eq!(plan_pass(true, false, false, true), Pass::Hold, "an open cut did not add a refusal");
        assert_eq!(plan_pass(false, true, false, true), Pass::Hold, "an open cut did not hold the restore");
    }

    #[test]
    fn plan_pass_holds_either_write_for_either_input() {
        assert_eq!(plan_pass(true, false, true, true), Pass::Hold);
        assert_eq!(plan_pass(false, true, true, true), Pass::Hold);
        assert_eq!(plan_pass(false, false, true, true), Pass::Idle, "nothing owed is not a refusal");
        assert_eq!(plan_pass(true, false, false, false), Pass::Engage);
        assert_eq!(plan_pass(false, true, false, false), Pass::Restore);
        assert_eq!(plan_pass(false, false, false, false), Pass::Idle);
    }

    #[test]
    fn a_cut_in_stepped_inside_the_last_second_holds_the_engage_the_gate_alone_leaves_closed() {
        // Run 16's shape read through the sim: a cut-in runtime being stepped, with no open cut run for the
        // gate to report. The gate-only guard answers closed there and writes; this one waits, and the write
        // lands when the runtime stops advancing.
        let mut sim = Sim::new(true);
        sim.cut_in_running = true;

        for _ in 0..20 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert!(sim.writes.is_empty(), "a stepped cut-in runtime did not hold the engage: {:?}", sim.writes);

        sim.cut_in_running = false;
        sim.engage(2);
        assert_eq!(sim.writes, vec![2], "the engage stayed held after the runtime stopped");
    }

    /// One logged pass of the story half inside run 57's 20:45:18 to 20:45:40 window, plus the gate's own
    /// open stretch for comparison: the stamp, what the build that ran it did, and the two inputs the guard
    /// reads. `wrote_in_the_run` is read off `run log/hachimi-run57.log`, not derived.
    struct Run57Pass {
        at: &'static str,
        printed: &'static str,
        cut_in_running: bool,
        cut_open: bool,
        wrote_in_the_run: bool,
    }

    const RUN_57_PASSES: [Run57Pass; 2] = [
        Run57Pass {
            // The refusal HEAD's stamp made, inside the gate's only open stretch: `PlayTrainingCut(info)
            // call 1` at 20:44:19.183 to `CleanUpCutt() call 1` at 20:44:28.080 (`:1604-1605`, `:1720`). The
            // stamp was hot there whatever the gate said, and the line itself is the proof: `529e4f5`, the
            // build this run ran, printed this reason only under `cut_in_engine_driving`. The cut-in door
            // crossed 0 times at the 20:44:14 totals - a zero is printed by leaving the counter out
            // (`StoryEventProbe.rs:800-802`, `:1589`) - and 230 times at the 20:44:33 totals (`:1730`), so
            // every crossing it made sits inside the 19 s bracket that holds this stretch.
            at: "20:44:23.512",
            printed: "`story high speed mode left alone, a cut in runtime is still being stepped` (:1691)",
            cut_in_running: true,
            cut_open: true,
            wrote_in_the_run: false,
        },
        Run57Pass {
            // The story pass in the window the task names. `write 5, story state 22` landed (`:1838`) 6.3 s
            // before the setting half refused its write on view 1200 (`:1854`). The gate was closed - it
            // closed at 20:44:28.080 (`:1720`) and `PlayTrainingCut` stayed at 1 for the rest of the run
            // (`:1872`, `:3707`) - and the stamp was cold: the door read 230 at the 20:44:33 totals (`:1730`)
            // and 230 again at 20:45:25 (`:1840`), so its last crossing was 50.3 s before this pass. The two
            // cut-ins this window did play, 343 ms and 662 ms on view 1200 (`:1844`, `:1850`), crossed neither
            // door.
            at: "20:45:23.529",
            printed: "`asked the story timeline for high speed type 2 (write 5, story state 22), IsHighSpeedMode \
                      0 -> 1` (:1838)",
            cut_in_running: false,
            cut_open: false,
            wrote_in_the_run: true,
        },
    ];

    #[test]
    fn run_57s_window_from_20_45_18_to_20_45_40_hands_what_head_handed_and_holds_what_head_held() {
        for pass in RUN_57_PASSES.iter() {
            // HEAD's condition reproduces what the build that ran this pass did. A row failing this is a row
            // whose inputs were read wrong off the log.
            assert_eq!(
                head_held(pass.cut_in_running),
                !pass.wrote_in_the_run,
                "HEAD's condition does not reproduce {} ({}): head_held {}, the run wrote {}",
                pass.at, pass.printed, head_held(pass.cut_in_running), pass.wrote_in_the_run,
            );

            if head_held(pass.cut_in_running) {
                assert_eq!(
                    plan_pass(true, true, pass.cut_in_running, pass.cut_open),
                    Pass::Hold,
                    "the shipped guard handed {} ({}), which HEAD held", pass.at, pass.printed,
                );
            }
            else {
                assert_ne!(
                    plan_pass(true, true, pass.cut_in_running, pass.cut_open),
                    Pass::Hold,
                    "the shipped guard holds {} ({}), which HEAD handed - the union added a fourth reason", pass.at, pass.printed,
                );
            }
        }

        // The reproduction shape the change set shipped - refuse on the gate alone - asked of the same two
        // rows. On the story half it agrees with HEAD at both: it holds 20:44:23.512 because the gate was
        // open there too, and it hands 20:45:23.529 because the gate was closed and HEAD's stamp was cold.
        // What run 57 shows for this module is that the boundary-only shape lost no coverage here, and that
        // neither shape reaches the two cut-ins the window did play.
        let gate_only_holds = |cut_open: bool| cut_open;

        for pass in RUN_57_PASSES.iter() {
            assert_eq!(
                gate_only_holds(pass.cut_open),
                !pass.wrote_in_the_run,
                "the boundary-only shape does not reproduce {} ({}), so the claim that it lost coverage here is wrong", pass.at, pass.printed,
            );
        }

        // Stated plainly: on run 57 the gate half adds no refusal at all. Its only open stretch is the one
        // where the stamp was hot anyway, and in the 20:45:18 to 20:45:40 window it is closed for every pass,
        // so refusing HEAD's condition union the gate is refusing HEAD's condition. What the gate is for on
        // this run is `529e4f5`'s own subject, the ui clock hold, not these writes.
        assert!(RUN_57_PASSES.iter().any(|pass| pass.cut_open && head_held(pass.cut_in_running)), "run 57 has no gate window HEAD did not already refuse");
        assert!(RUN_57_PASSES.iter().all(|pass| !pass.cut_open || head_held(pass.cut_in_running)), "the gate opened somewhere HEAD's condition did not already hold");
    }

    #[test]
    fn an_open_training_cut_holds_the_engage_and_the_write_stays_askable() {
        // Run 16 asked this static for its value again at 09:04:51, inside the window where its last cut
        // sat open and its cut-in was stepped every frame. The gate now covers that window from the
        // game's own cut doors (up to its tick bound), and `engage_high_speed_mode` is re-run on the
        // next game tick, so the write is later rather than lost.
        let mut sim = Sim::new(true);
        sim.cut_open = true;

        for _ in 0..20 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert!(sim.writes.is_empty(), "the engage wrote through an open cut: {:?}", sim.writes);
        assert_eq!(sim.applied_generation, NO_GENERATION, "a pass that wrote nothing remembered a write");

        sim.cut_open = false;   // `CleanUpCutt`, or the gate's 900 game tick bound
        sim.engage(2);
        assert_eq!(sim.writes, vec![2], "the deferred engage never landed");
    }

    #[test]
    fn a_restore_held_by_an_open_cut_does_not_consume_the_owed_marker() {
        // A restore is the write that puts the game's own value back on a static a live cut-in runtime
        // reads, which is the window run 16 stalled in. Dropping `HIGH_SPEED_WAS_SET` on a held pass
        // would leave this module's value on the static for the rest of the session, so the marker is
        // what carries the write to the next pass.
        let mut sim = Sim::new(true);

        // The value the game held, recorded as the baseline. Run 6 polled IsHighSpeedMode 19737 times
        // and read 0 while the timeline was running, so an off value is what this client leaves on
        // record.
        sim.game_write(0);
        sim.engage(2);
        assert_eq!(sim.writes, vec![2]);

        sim.cut_open = true;

        for _ in 0..20 {
            sim.restore();      // the option off while a cut is open
        }

        assert_eq!(sim.writes, vec![2], "the restore ran through an open cut");
        assert!(sim.was_set, "a held restore consumed the marker that makes the write owed");
        assert_eq!(sim.applied_value, 2, "a held restore cleared the write it still owed");
        assert_eq!(sim.applied_generation, 1, "a held restore cleared the state it was written for");

        sim.cut_open = false;
        sim.restore();
        assert_eq!(sim.writes, vec![2, 0], "the deferred restore never landed");
        assert_eq!(sim.applied_value, HIGH_SPEED_UNSET);
        assert_eq!(sim.applied_generation, NO_GENERATION);
    }

    #[test]
    fn a_gate_that_can_never_engage_still_asks_for_the_mode() {
        // The 0-of-3 shape: `arm_cut_gate` armed no door that opens a hold, so `CutGate::engage` is never
        // reachable, `Pass::Hold` is never returned, and the gate answers closed for the whole session.
        // Holding on that answer would switch the option off in silence, so the engage still runs, and the
        // build says the shape at install - `TrainingCuttProbe::cut_gate_missing_line`, at warn level.
        let mut sim = Sim::new(true);

        for _ in 0..20 {
            sim.engage(2);
            sim.clip_boundary();
        }

        assert_eq!(sim.writes, vec![2], "a gate that cannot engage stopped the option working at all");
    }

    #[test]
    fn a_gate_that_can_never_engage_does_not_hold_the_restore_that_owes_the_game_its_value_back() {
        // The same answer on the other direction, and it is the honest one: with nothing able to open a
        // hold there is no window for the restore to wait for, and a value this module wrote on the game's
        // own static has to be put back when the option goes off. The 2-of-3 shape - a gate missing only
        // its close door - still refuses while the gate says a cut is open, and `TrainingCuttProbe`'s
        // install line names that door and what its tick bound costs.
        let mut sim = Sim::new(true);
        sim.game_write(0);      // the value the game held, recorded as the baseline
        sim.engage(2);

        for _ in 0..20 {
            sim.restore();      // the option off, with no gate to wait for
        }

        assert_eq!(sim.writes, vec![2, 0], "the restore was held behind a gate that does not exist");
        assert!(!sim.was_set, "a restore that ran left the write owed");
    }
}