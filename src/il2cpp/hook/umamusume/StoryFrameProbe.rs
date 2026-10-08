use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI64, AtomicUsize};
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

// Observe only hooks over the paths that could be stepping story and career frames.
//
// `GetNextFrameCount_HighSpeed` and `SetHighSpeedFrameCount` are both installed and signature
// verified, and a career run that reached training turns, races and result screens still never
// called either one, so which method this client uses to advance text and wait frames is unknown.
// Every hook here hands its arguments to the original untouched and only counts what it saw, so
// observing these paths cannot change them.
//
// The read side of a wait frame property and the write side are both probed on purpose: a large
// count on a getter measures the game polling it, while a count on the setter is the game actually
// advancing a frame. `get_TimeScale` and `set_TimeScale` are probed for the same reason, because
// C22 and C24 are about who writes that value and how often.
//
// Installed only when debug_mode is on. A probe that prints nothing at all is the answer that this
// client does not use that path, which is why a summary of the probes that never even resolved is
// printed at install time.
const PROBE_DETAIL_LIMIT: usize = 6;
// A predicate called about 110 times a second wrote 66 lines of a 1026 line log at the 512
// cadence, so the summary cadence is coarser now that the totals line carries the counts.
const PROBE_CHUNK: usize = 4096;
const REPORT_INTERVAL_SECS: i64 = 20;

// Frame counts, flags and scales all share one log slot. A whole number prints without a
// fraction, so `32` and `0.3` both read the way they should.
const fn bit(flag: bool) -> f64 {
    if flag { 1.0 } else { 0.0 }
}

struct FrameProbe {
    name: &'static str,
    calls: AtomicUsize,
}

impl FrameProbe {
    const fn new(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0) }
    }

    // Frame counts, flags and scales all fit the same slot. A whole number prints without a
    // fraction, so `32` and `0.3` both read the way they should.
    fn observe(&self, values: &[f64]) {
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        if calls <= PROBE_DETAIL_LIMIT {
            debug!("Frame probe {} call {}: {:?}", self.name, calls, values);
        }
        else if calls % PROBE_CHUNK == 0 {
            debug!("Frame probe {} {} calls", self.name, calls);
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(atomic::Ordering::Relaxed)
    }

    // The totals line carries the method name without its argument list.
    fn short(&self) -> &str {
        match self.name.split_once('(') {
            Some((head, _)) => head,
            None => self.name,
        }
    }
}

static SET_FRAME_COUNT_FOR_WAITING: FrameProbe = FrameProbe::new("StoryTimelineController::SetFrameCountForWaiting(frames)");
static WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK: FrameProbe = FrameProbe::new("StoryTimelineController::get_WaitFrameCountUntilNextBlock()");
static WAIT_FRAME_UNTIL_NEXT_BLOCK: FrameProbe = FrameProbe::new("StoryTimelineController::get_WaitFrameUntilNextBlock()");
static SET_WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK: FrameProbe = FrameProbe::new("StoryTimelineController::set_WaitFrameCountUntilNextBlock(frames)");
static SET_WAIT_FRAME_UNTIL_NEXT_BLOCK: FrameProbe = FrameProbe::new("StoryTimelineController::set_WaitFrameUntilNextBlock(frames)");
static WAITING_FRAME_COUNT: FrameProbe = FrameProbe::new("StoryTimelineController::get_WaitingFrameCount()");
static UPDATE_TIME_SCALE_BY_HIGHSPEED: FrameProbe = FrameProbe::new("StoryTimelineController::UpdateTimeScaleByHispeedType()");
static GET_TIME_SCALE: FrameProbe = FrameProbe::new("StoryTimelineController::get_TimeScale()");
static SET_TIME_SCALE: FrameProbe = FrameProbe::new("StoryTimelineController::set_TimeScale(scale)");
static IS_SKIP_TO_TEXT_CLIP: FrameProbe = FrameProbe::new("StoryTimelineController::IsSkipToTextClip(skip_text, skip_block)");
static STORY_END_FRAME_SKIPPED: FrameProbe = FrameProbe::new("StoryTimelineController::IsStoryEndFrameOrGrandLiveWaitFrameSkipped(frame, next_frame)");
static TEXT_CLIP_WAIT_FRAME: FrameProbe = FrameProbe::new("StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize()");

static PROBES: [&FrameProbe; 12] = [
    &SET_FRAME_COUNT_FOR_WAITING,
    &WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK,
    &WAIT_FRAME_UNTIL_NEXT_BLOCK,
    &SET_WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK,
    &SET_WAIT_FRAME_UNTIL_NEXT_BLOCK,
    &WAITING_FRAME_COUNT,
    &UPDATE_TIME_SCALE_BY_HIGHSPEED,
    &GET_TIME_SCALE,
    &SET_TIME_SCALE,
    &IS_SKIP_TO_TEXT_CLIP,
    &STORY_END_FRAME_SKIPPED,
    &TEXT_CLIP_WAIT_FRAME,
];

type SetFrameCountForWaitingFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SetFrameCountForWaiting(this: *mut Il2CppObject, frames: i32) {
    SET_FRAME_COUNT_FOR_WAITING.observe(&[frames as f64]);

    get_orig_fn!(SetFrameCountForWaiting, SetFrameCountForWaitingFn)(this, frames);
}

type GetWaitFrameCountUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitFrameCountUntilNextBlock(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitFrameCountUntilNextBlock, GetWaitFrameCountUntilNextBlockFn)(this);
    WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK.observe(&[value as f64]);

    value
}

type GetWaitFrameUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitFrameUntilNextBlock(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitFrameUntilNextBlock, GetWaitFrameUntilNextBlockFn)(this);
    WAIT_FRAME_UNTIL_NEXT_BLOCK.observe(&[value as f64]);

    value
}

type SetWaitFrameCountUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn set_WaitFrameCountUntilNextBlock(this: *mut Il2CppObject, frames: i32) {
    SET_WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK.observe(&[frames as f64]);

    get_orig_fn!(set_WaitFrameCountUntilNextBlock, SetWaitFrameCountUntilNextBlockFn)(this, frames);
}

type SetWaitFrameUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn set_WaitFrameUntilNextBlock(this: *mut Il2CppObject, frames: i32) {
    SET_WAIT_FRAME_UNTIL_NEXT_BLOCK.observe(&[frames as f64]);

    get_orig_fn!(set_WaitFrameUntilNextBlock, SetWaitFrameUntilNextBlockFn)(this, frames);
}

type GetWaitingFrameCountFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitingFrameCount(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitingFrameCount, GetWaitingFrameCountFn)(this);
    WAITING_FRAME_COUNT.observe(&[value as f64]);

    value
}

type UpdateTimeScaleByHispeedTypeFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn UpdateTimeScaleByHispeedType(this: *mut Il2CppObject) {
    UPDATE_TIME_SCALE_BY_HIGHSPEED.observe(&[]);

    get_orig_fn!(UpdateTimeScaleByHispeedType, UpdateTimeScaleByHispeedTypeFn)(this);
}

// Both are dumped as static: `get_TimeScale/0 -> static float()` and
// `set_TimeScale/1 -> static void(float)`, so the class scale is shared state rather than per
// instance state, and these two counters say who is reading and writing it.
type GetTimeScaleFn = extern "C" fn() -> f32;
extern "C" fn get_TimeScale() -> f32 {
    let value = get_orig_fn!(get_TimeScale, GetTimeScaleFn)();
    GET_TIME_SCALE.observe(&[value as f64]);

    value
}

type SetTimeScaleFn = extern "C" fn(scale: f32);
extern "C" fn set_TimeScale(scale: f32) {
    SET_TIME_SCALE.observe(&[scale as f64]);

    get_orig_fn!(set_TimeScale, SetTimeScaleFn)(scale);
}

type IsSkipToTextClipFn = extern "C" fn(this: *mut Il2CppObject, skip_text: bool, skip_block: bool) -> bool;
extern "C" fn IsSkipToTextClip(this: *mut Il2CppObject, skip_text: bool, skip_block: bool) -> bool {
    let value = get_orig_fn!(IsSkipToTextClip, IsSkipToTextClipFn)(this, skip_text, skip_block);
    IS_SKIP_TO_TEXT_CLIP.observe(&[bit(skip_text), bit(skip_block), bit(value)]);

    value
}

// `IsHighSpeedMode()` is not probed. The game polls it about 130 times a second while a scene is
// running, and a probe on a path that hot pays a `get_orig_fn!` lookup on every poll, which is the
// mod's own cost rather than the game's. The mode is read once per report instead, through the
// guarded wrapper StoryTimelineController already installs for its own decision.
// Dumped as `IsStoryEndFrameOrGrandLiveWaitFrameSkipped/4 -> static bool(class, class, int, int)`,
// so the first two registers are real arguments rather than an instance pointer.
type StoryEndFrameSkippedFn = extern "C" fn(
    controller: *mut Il2CppObject,
    block_data: *mut Il2CppObject,
    frame: i32,
    next_frame: i32,
) -> bool;
extern "C" fn IsStoryEndFrameOrGrandLiveWaitFrameSkipped(
    controller: *mut Il2CppObject,
    block_data: *mut Il2CppObject,
    frame: i32,
    next_frame: i32,
) -> bool {
    let value = get_orig_fn!(IsStoryEndFrameOrGrandLiveWaitFrameSkipped, StoryEndFrameSkippedFn)(
        controller, block_data, frame, next_frame,
    );
    STORY_END_FRAME_SKIPPED.observe(&[frame as f64, next_frame as f64, bit(value)]);

    value
}

type GetWaitFrameUntilNextBlockLocalizeFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn GetWaitFrameUntilNextBlockLocalize(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(GetWaitFrameUntilNextBlockLocalize, GetWaitFrameUntilNextBlockLocalizeFn)(this);
    TEXT_CLIP_WAIT_FRAME.observe(&[value as f64]);

    value
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let Ok(controller) = get_class(umamusume, c"Gallop", c"StoryTimelineController") else {
        debug!("Frame probe: StoryTimelineController not found");
        return;
    };

    let mut missing: Vec<&str> = Vec::new();
    let mut installed = 0usize;

    // Every candidate goes through the same matcher the scaling hooks use: the dumped parameter
    // types, the return type, the generic check, and either a required or a forbidden reference
    // flag per parameter. Nothing here is installed on a name plus an arity.
    macro_rules! probe {
        ($addr:ident, $hook:ident, $label:literal) => {
            if $addr != 0 {
                new_hook!($addr, $hook);
                installed += 1;
            }
            else {
                missing.push($label);
            }
        };
    }

    let set_waiting_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "SetFrameCountForWaiting", &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    probe!(set_waiting_addr, SetFrameCountForWaiting, "SetFrameCountForWaiting");

    let count_until_next_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "get_WaitFrameCountUntilNextBlock", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
    ) };
    probe!(count_until_next_addr, get_WaitFrameCountUntilNextBlock, "get_WaitFrameCountUntilNextBlock");

    let frame_until_next_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "get_WaitFrameUntilNextBlock", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
    ) };
    probe!(frame_until_next_addr, get_WaitFrameUntilNextBlock, "get_WaitFrameUntilNextBlock");

    let set_count_until_next_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "set_WaitFrameCountUntilNextBlock", &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    probe!(set_count_until_next_addr, set_WaitFrameCountUntilNextBlock, "set_WaitFrameCountUntilNextBlock");

    let set_frame_until_next_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "set_WaitFrameUntilNextBlock", &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    probe!(set_frame_until_next_addr, set_WaitFrameUntilNextBlock, "set_WaitFrameUntilNextBlock");

    let waiting_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "get_WaitingFrameCount", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
    ) };
    probe!(waiting_addr, get_WaitingFrameCount, "get_WaitingFrameCount");

    let time_scale_update_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "UpdateTimeScaleByHispeedType", &[], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    probe!(time_scale_update_addr, UpdateTimeScaleByHispeedType, "UpdateTimeScaleByHispeedType");

    let get_scale_addr = unsafe { AnimationSpeed::resolve_static_method(
        controller, "get_TimeScale", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4,
    ) };
    probe!(get_scale_addr, get_TimeScale, "get_TimeScale");

    let set_scale_addr = unsafe { AnimationSpeed::resolve_static_method(
        controller, "set_TimeScale", &[Il2CppTypeEnum_IL2CPP_TYPE_R4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    probe!(set_scale_addr, set_TimeScale, "set_TimeScale");

    let skip_clip_addr = unsafe { AnimationSpeed::resolve_method(
        controller, "IsSkipToTextClip",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
    ) };
    probe!(skip_clip_addr, IsSkipToTextClip, "IsSkipToTextClip");

    let story_end_addr = unsafe { AnimationSpeed::resolve_static_method(
        controller, "IsStoryEndFrameOrGrandLiveWaitFrameSkipped",
        &[
            Il2CppTypeEnum_IL2CPP_TYPE_CLASS, Il2CppTypeEnum_IL2CPP_TYPE_CLASS,
            Il2CppTypeEnum_IL2CPP_TYPE_I4, Il2CppTypeEnum_IL2CPP_TYPE_I4,
        ],
        Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
    ) };
    probe!(story_end_addr, IsStoryEndFrameOrGrandLiveWaitFrameSkipped, "IsStoryEndFrameOrGrandLiveWaitFrameSkipped");

    if let Ok(text_clip) = get_class(umamusume, c"Gallop", c"StoryTimelineTextClipData") {
        let wait_localize_addr = unsafe { AnimationSpeed::resolve_method(
            text_clip, "GetWaitFrameUntilNextBlockLocalize", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
        ) };
        probe!(wait_localize_addr, GetWaitFrameUntilNextBlockLocalize, "StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize");
    }
    else {
        debug!("Frame probe: StoryTimelineTextClipData not found");
        missing.push("StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize");
    }

    let _ = START.set(Instant::now());
    info!("Frame probe: {installed} of {} observe only probes installed, totals reported every {REPORT_INTERVAL_SECS} s", PROBES.len());

    if !missing.is_empty() {
        info!("Frame probe: not installed, no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour. A report is printed once per interval and only while
// the totals are still growing, so a quiet path stays quiet.
pub fn report_if_due() {
    let Some(start) = START.get() else { return };

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();

    if totals == 0 {
        return;
    }

    let now = start.elapsed().as_secs() as i64;
    let last_sec = LAST_REPORT_SEC.load(atomic::Ordering::Relaxed);

    if totals == LAST_TOTALS.load(atomic::Ordering::Relaxed) {
        return;
    }

    if last_sec >= 0 && now - last_sec < REPORT_INTERVAL_SECS {
        return;
    }

    LAST_REPORT_SEC.store(now, atomic::Ordering::Relaxed);
    LAST_TOTALS.store(totals, atomic::Ordering::Relaxed);

    let mut line = String::new();

    for probe in PROBES.iter() {
        let _ = write!(line, " {}={}", probe.short(), probe.calls());
    }

    // The mode the timeline is actually in, read once per report through the guarded wrapper
    // instead of from a hook on the path the game polls a hundred and thirty times a second. The
    // wrapper returns a bool, so the report prints the game's answer and not a register width.
    // Beside it, how many values this module has put on the high speed static so far: the number
    // that says whether the engagement is one write per story state or one write per block.
    let mode = crate::il2cpp::hook::umamusume::StoryTimelineController::IsStoryHighSpeedMode();
    let writes = crate::il2cpp::hook::umamusume::StoryTimelineController::high_speed_write_count();

    info!("Frame probe totals at {now} s:{line} story high speed mode {} writes {writes}", u8::from(mode));
}

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);
