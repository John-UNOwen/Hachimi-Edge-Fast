use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::AnimationSpeed,
        symbols::{get_class, get_method},
        types::*,
    },
};

// Observe only hooks over the paths that could be stepping story and career frames.
//
// `GetNextFrameCount_HighSpeed` and `SetHighSpeedFrameCount` are both installed and signature
// verified, and a career run that reached training turns, races and result screens still never
// called either one, so which method this client uses to advance text and wait frames is unknown.
// Every hook here hands its arguments to the original untouched and only counts what it saw, so
// observing these paths cannot change them. The detail lines cover the first calls of each probe,
// a chunk line marks heavy traffic, and `report_if_due` prints the running totals so a run ends
// with a count for every candidate instead of a guess.
//
// Installed only when debug_mode is on. A probe that prints nothing at all is the answer that the
// client does not use that path.
const PROBE_DETAIL_LIMIT: usize = 6;
const PROBE_CHUNK: usize = 512;
const REPORT_INTERVAL_SECS: i64 = 20;
const METHOD_ATTRIBUTE_STATIC: u16 = 0x0010;

struct FrameProbe {
    name: &'static str,
    calls: AtomicUsize,
}

impl FrameProbe {
    const fn new(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0) }
    }

    fn observe(&self, values: &[i64]) {
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
}

static SKIP_FRAME_COUNT: FrameProbe = FrameProbe::new("StoryTimelineController::SkipFrameCount(frames, flag1, flag2)");
static SET_FRAME_COUNT_FOR_WAITING: FrameProbe = FrameProbe::new("StoryTimelineController::SetFrameCountForWaiting(frames)");
static WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK: FrameProbe = FrameProbe::new("StoryTimelineController::get_WaitFrameCountUntilNextBlock()");
static WAIT_FRAME_UNTIL_NEXT_BLOCK: FrameProbe = FrameProbe::new("StoryTimelineController::get_WaitFrameUntilNextBlock()");
static WAITING_FRAME_COUNT: FrameProbe = FrameProbe::new("StoryTimelineController::get_WaitingFrameCount()");
static UPDATE_TIME_SCALE_BY_HIGHSPEED: FrameProbe = FrameProbe::new("StoryTimelineController::UpdateTimeScaleByHispeedType()");
static SKIP_MOTION_FRAME: FrameProbe = FrameProbe::new("StoryTimelineController::SkipMotionFrame(frames)");
static IS_SKIP_TO_TEXT_CLIP: FrameProbe = FrameProbe::new("StoryTimelineController::IsSkipToTextClip(skip_text, skip_block)");
static IS_HIGH_SPEED_MODE: FrameProbe = FrameProbe::new("StoryTimelineController::IsHighSpeedMode()");
static STORY_END_FRAME_SKIPPED: FrameProbe = FrameProbe::new("StoryTimelineController::IsStoryEndFrameOrGrandLiveWaitFrameSkipped(a, b)");
static TEXT_CLIP_WAIT_FRAME: FrameProbe = FrameProbe::new("StoryTimelineTextClipData::GetWaitFrameUntilNextBlockLocalize()");

static PROBES: [&FrameProbe; 11] = [
    &SKIP_FRAME_COUNT,
    &SET_FRAME_COUNT_FOR_WAITING,
    &WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK,
    &WAIT_FRAME_UNTIL_NEXT_BLOCK,
    &WAITING_FRAME_COUNT,
    &UPDATE_TIME_SCALE_BY_HIGHSPEED,
    &SKIP_MOTION_FRAME,
    &IS_SKIP_TO_TEXT_CLIP,
    &IS_HIGH_SPEED_MODE,
    &STORY_END_FRAME_SKIPPED,
    &TEXT_CLIP_WAIT_FRAME,
];

type SkipFrameCountFn = extern "C" fn(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool);
extern "C" fn SkipFrameCount(this: *mut Il2CppObject, frames: i32, flag1: bool, flag2: bool) {
    SKIP_FRAME_COUNT.observe(&[frames as i64, flag1 as i64, flag2 as i64]);

    get_orig_fn!(SkipFrameCount, SkipFrameCountFn)(this, frames, flag1, flag2);
}

type SetFrameCountForWaitingFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SetFrameCountForWaiting(this: *mut Il2CppObject, frames: i32) {
    SET_FRAME_COUNT_FOR_WAITING.observe(&[frames as i64]);

    get_orig_fn!(SetFrameCountForWaiting, SetFrameCountForWaitingFn)(this, frames);
}

type GetWaitFrameCountUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitFrameCountUntilNextBlock(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitFrameCountUntilNextBlock, GetWaitFrameCountUntilNextBlockFn)(this);
    WAIT_FRAME_COUNT_UNTIL_NEXT_BLOCK.observe(&[value as i64]);

    value
}

type GetWaitFrameUntilNextBlockFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitFrameUntilNextBlock(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitFrameUntilNextBlock, GetWaitFrameUntilNextBlockFn)(this);
    WAIT_FRAME_UNTIL_NEXT_BLOCK.observe(&[value as i64]);

    value
}

type GetWaitingFrameCountFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn get_WaitingFrameCount(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(get_WaitingFrameCount, GetWaitingFrameCountFn)(this);
    WAITING_FRAME_COUNT.observe(&[value as i64]);

    value
}

type UpdateTimeScaleByHispeedTypeFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn UpdateTimeScaleByHispeedType(this: *mut Il2CppObject) {
    UPDATE_TIME_SCALE_BY_HIGHSPEED.observe(&[]);

    get_orig_fn!(UpdateTimeScaleByHispeedType, UpdateTimeScaleByHispeedTypeFn)(this);
}

type SkipMotionFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
extern "C" fn SkipMotionFrame(this: *mut Il2CppObject, frames: i32) {
    SKIP_MOTION_FRAME.observe(&[frames as i64]);

    get_orig_fn!(SkipMotionFrame, SkipMotionFrameFn)(this, frames);
}

type IsSkipToTextClipFn = extern "C" fn(this: *mut Il2CppObject, skip_text: bool, skip_block: bool) -> bool;
extern "C" fn IsSkipToTextClip(this: *mut Il2CppObject, skip_text: bool, skip_block: bool) -> bool {
    let value = get_orig_fn!(IsSkipToTextClip, IsSkipToTextClipFn)(this, skip_text, skip_block);
    IS_SKIP_TO_TEXT_CLIP.observe(&[skip_text as i64, skip_block as i64, value as i64]);

    value
}

// Dumped as `IsHighSpeedMode/0 -> static bool()`. A static method carries no hidden `this`, so a
// wrapper that declares no parameters matches it exactly.
type IsHighSpeedModeFn = extern "C" fn() -> bool;
extern "C" fn IsHighSpeedMode() -> bool {
    let value = get_orig_fn!(IsHighSpeedMode, IsHighSpeedModeFn)();
    IS_HIGH_SPEED_MODE.observe(&[value as i64]);

    value
}

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
    STORY_END_FRAME_SKIPPED.observe(&[frame as i64, next_frame as i64, value as i64]);

    value
}

type GetWaitFrameUntilNextBlockLocalizeFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn GetWaitFrameUntilNextBlockLocalize(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(GetWaitFrameUntilNextBlockLocalize, GetWaitFrameUntilNextBlockLocalizeFn)(this);
    TEXT_CLIP_WAIT_FRAME.observe(&[value as i64]);

    value
}

// Instance candidates go through the same signature check the scaling hooks use, so a detour can
// never land on an overload with different arguments or a different return type.
unsafe fn resolve_instance(
    class: *mut Il2CppClass,
    name: &str,
    params: &[Il2CppTypeEnum],
    ret: Il2CppTypeEnum,
) -> usize {
    AnimationSpeed::resolve_method(class, name, params, ret)
}

// Static candidates are matched by arity because their object parameters are dumped as class
// references, and the static flag is then confirmed instead of assumed.
unsafe fn resolve_static(class: *mut Il2CppClass, name: &std::ffi::CStr, args_count: i32) -> usize {
    match get_method(class, name, args_count) {
        Ok(method) => {
            if (*method).flags & METHOD_ATTRIBUTE_STATIC == 0 {
                debug!("Frame probe: {} is not static, probe skipped", name.to_string_lossy());
                0
            }
            else {
                (*method).methodPointer
            }
        }
        Err(_) => {
            debug!("Frame probe: {} has no overload with {} arguments", name.to_string_lossy(), args_count);
            0
        }
    }
}

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let Ok(controller) = get_class(umamusume, c"Gallop", c"StoryTimelineController") else {
        debug!("Frame probe: StoryTimelineController not found");
        return;
    };

    let mut installed = 0usize;

    let skip_frames_addr = unsafe { resolve_instance(
        controller, "SkipFrameCount",
        &[Il2CppTypeEnum_IL2CPP_TYPE_I4, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN],
        Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if skip_frames_addr != 0 { new_hook!(skip_frames_addr, SkipFrameCount); installed += 1; }

    let set_waiting_addr = unsafe { resolve_instance(
        controller, "SetFrameCountForWaiting",
        &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if set_waiting_addr != 0 { new_hook!(set_waiting_addr, SetFrameCountForWaiting); installed += 1; }

    let count_until_next_addr = unsafe { resolve_instance(
        controller, "get_WaitFrameCountUntilNextBlock", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
    ) };
    if count_until_next_addr != 0 { new_hook!(count_until_next_addr, get_WaitFrameCountUntilNextBlock); installed += 1; }

    let frame_until_next_addr = unsafe { resolve_instance(
        controller, "get_WaitFrameUntilNextBlock", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
    ) };
    if frame_until_next_addr != 0 { new_hook!(frame_until_next_addr, get_WaitFrameUntilNextBlock); installed += 1; }

    let waiting_addr = unsafe { resolve_instance(
        controller, "get_WaitingFrameCount", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
    ) };
    if waiting_addr != 0 { new_hook!(waiting_addr, get_WaitingFrameCount); installed += 1; }

    let time_scale_addr = unsafe { resolve_instance(
        controller, "UpdateTimeScaleByHispeedType", &[], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if time_scale_addr != 0 { new_hook!(time_scale_addr, UpdateTimeScaleByHispeedType); installed += 1; }

    let skip_motion_addr = unsafe { resolve_instance(
        controller, "SkipMotionFrame", &[Il2CppTypeEnum_IL2CPP_TYPE_I4], Il2CppTypeEnum_IL2CPP_TYPE_VOID,
    ) };
    if skip_motion_addr != 0 { new_hook!(skip_motion_addr, SkipMotionFrame); installed += 1; }

    let skip_clip_addr = unsafe { resolve_instance(
        controller, "IsSkipToTextClip",
        &[Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN, Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN,
    ) };
    if skip_clip_addr != 0 { new_hook!(skip_clip_addr, IsSkipToTextClip); installed += 1; }

    let high_speed_mode_addr = unsafe { resolve_static(controller, c"IsHighSpeedMode", 0) };
    if high_speed_mode_addr != 0 { new_hook!(high_speed_mode_addr, IsHighSpeedMode); installed += 1; }

    let story_end_addr = unsafe { resolve_static(controller, c"IsStoryEndFrameOrGrandLiveWaitFrameSkipped", 4) };
    if story_end_addr != 0 { new_hook!(story_end_addr, IsStoryEndFrameOrGrandLiveWaitFrameSkipped); installed += 1; }

    if let Ok(text_clip) = get_class(umamusume, c"Gallop", c"StoryTimelineTextClipData") {
        let wait_localize_addr = unsafe { resolve_instance(
            text_clip, "GetWaitFrameUntilNextBlockLocalize", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4,
        ) };
        if wait_localize_addr != 0 { new_hook!(wait_localize_addr, GetWaitFrameUntilNextBlockLocalize); installed += 1; }
    }
    else {
        debug!("Frame probe: StoryTimelineTextClipData not found");
    }

    let _ = START.set(Instant::now());
    info!("Frame probe: {installed} observe only probes installed, totals reported every {REPORT_INTERVAL_SECS} s");
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
        let _ = write!(line, " {}={}", probe.name, probe.calls());
    }

    info!("Frame probe totals at {now} s:{line}");
}
