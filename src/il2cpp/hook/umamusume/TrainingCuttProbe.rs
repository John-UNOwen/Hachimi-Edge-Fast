use std::ffi::CString;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicUsize};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use fnv::{FnvHashMap, FnvHashSet};
use once_cell::sync::Lazy;

use crate::{
    core::{hachimi::recover_lock, settings_preset, Hachimi},
    il2cpp::{
        api::{il2cpp_string_chars, il2cpp_string_length},
        hook::{
            hook_id, hook_key_match,
            umamusume::{AnimationSpeed, SceneDefine::ViewId, SceneManager},
        },
        symbols::{get_class, get_field_from_name},
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
// The run boundary is the door the game plays a cut through, measured rather than named. The first version
// chose `CutInTimelineController::ResetCurrentTime` from the class name and run 9 returned `cut runs 0`
// (C47). The second chose `set_IsPlayingCutt/1 -> void(bool)`, the game's own playing flag, and run 10
// installed it and it printed nothing across a 472 s career that contained 19 training cuts (C49). What that
// run did reach is `PlayTrainingCut` 19 times, `PlayTrainingCutEndAsync` 19 times and `CleanUpCutt` 35
// times, so a run opens at the cut start doors and closes at `CleanUpCutt`. `set_IsPlayingCutt` and
// `get_IsPlayingCutt` stay counted, and `ResetCurrentTime` stays counted so a non training cut-in is still
// visible.
//
// Alongside the counts the probe now reads the numbers the cut-in engine keeps for itself:
// `GetTotalTime`, `GetTotalFrameCeil`, `get_CurrentFrame` and `get_Speed`. The game already knows how long
// the animation is and how far it has got, so a length should come from it rather than be inferred from
// wall clock (A29). `IsValidTag` and the tag cut-in player separate a friendship cut from a regular one,
// which run 9 could not do at all (A28, A30).
//
// Installed only when debug_mode is on, like StoryFrameProbe, and installed after the scaling modules
// so both resolve the same class lookups.
pub(crate) const PROBE_DETAIL_LIMIT: usize = 8;
pub(crate) const PROBE_CHUNK: usize = 4096;
pub(crate) const REPORT_INTERVAL_SECS: i64 = 20;
// A timeline peak is summed as whole milliseconds so the total stays an integer.
const MS_PER_SECOND: f32 = 1000.0;

const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const I4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I4;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
// A `string<System.String>` parameter is its own type in the metadata, and the exact walk compares the enum the
// wrapper declared with the one the method carries. Run 43 reported `Play(label, action, int)` and
// `Play(label, action, float)` as "no matching overload" because this file asked for CLASS where the dump spells a
// string, so the two doors that carry the number were never armed (A2).
const STRING: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_STRING;
// `struct<Gallop.TrainingDefine.TrainingCommandId:4B>` and `TrainingResultType` are value types of
// four bytes, which is the size A5 proved travels in a general purpose register. A probe only hands
// them back untouched, and the resolver still measures the size before anything is installed.
const VALUETYPE: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VALUETYPE;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const ONE_FLOAT: &[Il2CppTypeEnum] = &[R4];
const ONE_FLAG: &[Il2CppTypeEnum] = &[BOOL];
const ONE_INFO: &[Il2CppTypeEnum] = &[CLASS];
const ONE_ID: &[Il2CppTypeEnum] = &[VALUETYPE];
const ONE_INT: &[Il2CppTypeEnum] = &[I4];
// `PlayIcon/2 -> void(int, class<Gallop.TrainingParamChangeUI.ChangeParameterInfo>)`.
const INT_AND_INFO: &[Il2CppTypeEnum] = &[I4, CLASS];
const ONE_ACTION: &[Il2CppTypeEnum] = &[CLASS];
const ID_AND_FLAGS: &[Il2CppTypeEnum] = &[VALUETYPE, BOOL, BOOL];
const RESULT_AND_LIST: &[Il2CppTypeEnum] = &[VALUETYPE, CLASS];
const FRAMES_AND_FLAG: &[Il2CppTypeEnum] = &[I4, BOOL];
// `PlayCutIn/2 -> void(generic<List<SupportCardData>>, class<System.Action>)`.
const LIST_AND_ACTION: &[Il2CppTypeEnum] = &[CLASS, CLASS];
// `PlayOut/2 -> void(bool, class<System.Action>)`.
const FLAG_AND_ACTION: &[Il2CppTypeEnum] = &[BOOL, CLASS];
// `CoroutinePlayIn/2 -> IEnumerator(float, int)`, the door run 26 named as where the status panel actually
// runs, and `PlayInRootMotion/1 -> void(float)` next door on the frame class.
const INTERVAL_AND_FRAME: &[Il2CppTypeEnum] = &[R4, I4];
// `SingleModeMainViewTrainingCutStatusFrame::PlayOut/3 -> void(bool, float, class<System.Action>)`.
const FLAG_TIME_AND_ACTION: &[Il2CppTypeEnum] = &[BOOL, R4, CLASS];
// `SingleModeMainViewTrainingCutStatus::PlayIn/4 -> void(float, int, bool, class<System.Action>)`.
const INTERVAL_FRAME_FLAG_AND_ACTION: &[Il2CppTypeEnum] = &[R4, I4, BOOL, CLASS];
// `SingleModeMainViewTrainingCutStatusFrame::PlayIn/9 -> void(int, int, int, int, int, int, float, int, bool)`
// and `Skip/5 -> void(int, int, int, int, int)`, nine and five plain arguments and no struct among them (A5).
const FRAME_PLAY_IN_ARGS: &[Il2CppTypeEnum] = &[I4, I4, I4, I4, I4, I4, R4, I4, BOOL];
// `TimelineTrainingCuttController::Init/9 -> void(class<TrainingCuttControllerDelegate>, class<Action>,
// class<Action>, class<TrainingCuttControllerDelegate>, class<Action>, class<Action>, class<Action>,
// generic<Action<float>>, generic<Func<bool>>)` (`introspect.log`, the class block): nine references and
// no struct among them, so every one of them travels in a general purpose register (A5) and the wrapper
// hands all nine back to the original untouched.
const NINE_REFERENCES: &[Il2CppTypeEnum] = &[CLASS, CLASS, CLASS, CLASS, CLASS, CLASS, CLASS, CLASS, CLASS];
const FIVE_INTS: &[Il2CppTypeEnum] = &[I4, I4, I4, I4, I4];
// `PlayMemberCutin/3 -> static class<Gallop.FlashPlayer>(generic<List<int>>, class<Transform>, class<Action>)`,
// `PlayAoharuSoulCutin/4 -> static class<FlashPlayer>(int, class<Transform>, class<Action>, class<Action>)` and
// `PlayOutMemberCutin/3 -> static void(class<FlashPlayer>, int, class<Action>)` (`introspect.log:25120` to
// `25123`): the doors that hand a `FlashPlayer` to a support member cut-in, which is what a friendship cut-in is.
// No struct travels through any of them that A5 has not already cleared (A5, C48).
const MEMBER_CUTIN_ARGS: &[Il2CppTypeEnum] = &[CLASS, CLASS, CLASS];
const SOUL_CUTIN_ARGS: &[Il2CppTypeEnum] = &[I4, CLASS, CLASS, CLASS];
const PLAYER_INDEX_AND_ACTION: &[Il2CppTypeEnum] = &[CLASS, I4, CLASS];
// `FlashPlayer::Play/3 -> void(string<System.String>, class<System.Action>, int)` and the same door spelled with a
// float (`introspect-run45.log:26462` to `26463`): a label, a callback, and the number this run has to identify as a
// speed or a duration. Two overloads of one name and one arity, which is what resolving by signature is for (A2).
const LABEL_AND_INT: &[Il2CppTypeEnum] = &[STRING, I4];
const LABEL_AND_FLOAT: &[Il2CppTypeEnum] = &[STRING, R4];
// `FlashActionPlayer::Play/3 -> void(string<System.String>, float, string<System.String>)` and
// `Play/2 -> void(string, string)` (`introspect-run45.log:26411` to `26412`): the wrapper that holds a flash player and
// has a float on its play door. Counted apart so a run says which of the two classes a cut-in effect plays on.
const LABEL_TIME_AND_LABEL: &[Il2CppTypeEnum] = &[STRING, R4, STRING];
const TWO_LABELS: &[Il2CppTypeEnum] = &[STRING, STRING];
// `InitializeFlash/4 -> void(generic<List<ChangeParameterInfo>>, float, bool, class<Canvas>)` and
// `PlayParameterChangeAsync/2 -> IEnumerator(generic<List<ChangeParameterInfo>>, float)`. Both are
// reference plus float doors, and the generic half only resolves through the generic matcher (C48).
const LIST_AND_INTERVAL: &[Il2CppTypeEnum] = &[CLASS, R4];
const LIST_INTERVAL_FLAG_AND_CANVAS: &[Il2CppTypeEnum] = &[CLASS, R4, BOOL, CLASS];
// `Initialize/15 -> void(class<RectTransform>, generic<List<ChangeParameterInfo>>, class<Canvas>,
// class<Action>, float, float, bool, bool, class<HpGauge>, class<MotivationButton>, int, class<Action>,
// bool, bool, bool)`: the door the caller sets the cascade up through, and the only place the two
// cascade intervals can be seen as the caller hands them. No struct travels here, which is what makes a
// fifteen argument wrapper safe to declare (A5).
const PLATE_INITIALIZE_ARGS: &[Il2CppTypeEnum] = &[CLASS, CLASS, CLASS, CLASS, R4, R4, BOOL, BOOL, CLASS, CLASS, I4, CLASS, BOOL, BOOL, BOOL];
// `CommonSendCommandAsync/2 -> IEnumerator(struct<SingleModeDefine.CommandType:4B>, struct<TrainingDefine.TrainingCommandId:4B>)`
// and `SendCommandAsync/6 -> static IEnumerator(same two structs, int, int, generic<Action<SingleModeCommandResult>>, generic<Action<Cute.Http.ErrorType, int>>)`.
// Both structs are four byte value types, which travel in a general purpose register (A5), and the two
// generic callbacks match CLASS through the generic matcher (C48).
const TWO_COMMAND_IDS: &[Il2CppTypeEnum] = &[VALUETYPE, VALUETYPE];
const COMMAND_SEND_ARGS: &[Il2CppTypeEnum] = &[VALUETYPE, VALUETYPE, I4, I4, CLASS, CLASS];

pub(crate) fn bit(flag: bool) -> f64 {
    if flag { 1.0 } else { 0.0 }
}

// A bool sampled into a peak answers "was this ever true" for the whole run without a log line per read.
pub(crate) fn flag_bit(flag: bool) -> f32 {
    if flag { 1.0 } else { 0.0 }
}

// The peak merge a frame hot getter needs: the largest value seen so far, kept as `f32` bits so one
// atomic is enough. A negative or non finite reading is ignored instead of becoming the peak, because
// a NaN would out rank every real value behind it, and a 0.0 answer from a class that is still being
// set up must not wipe a peak already measured.
pub(crate) fn peak_merge(current: u32, value: f32) -> u32 {
    if !value.is_finite() || value < 0.0 {
        return current;
    }

    let candidate = value.to_bits();

    if candidate > current { candidate } else { current }
}

pub(crate) fn peak_seconds(bits: u32) -> f32 {
    f32::from_bits(bits)
}

pub(crate) fn peak_milliseconds(bits: u32) -> i64 {
    (f32::from_bits(bits) * MS_PER_SECOND) as i64
}

// A report is due when the totals moved and the interval passed. A quiet path stays quiet, which is
// the rule StoryFrameProbe runs on.
pub(crate) fn report_due(now_sec: i64, last_sec: i64, totals: usize, last_totals: usize, interval: i64) -> bool {
    if totals == 0 || totals == last_totals {
        return false;
    }

    last_sec < 0 || now_sec - last_sec >= interval
}

// Shared by both cut probes so the counting rules, the chunked logging and the peak merge live in one
// place. A hook the game reaches every frame must not format anything.
pub(crate) struct CutProbe {
    name: &'static str,
    calls: AtomicUsize,
    // Set for the probes worth a peak: the largest value this path handed back all run.
    peaked: bool,
    peak: AtomicU32,
    // The clock time of the last crossing, from `ProbeSuite`, or -1 for a door never crossed or a suite that never
    // started. The watchdog reads it to say which doors went quiet during a stall.
    last_us: AtomicI64,
}

impl CutProbe {
    pub(crate) const fn counted(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0), peaked: false, peak: AtomicU32::new(0), last_us: AtomicI64::new(-1) }
    }

    pub(crate) const fn peaked(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0), peaked: true, peak: AtomicU32::new(0), last_us: AtomicI64::new(-1) }
    }

    // First hits print the values, later ones only the count, so a path the game polls every frame
    // cannot fill the log.
    pub(crate) fn observe(&self, values: &[f64]) {
        self.stamp();
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;
        note_hole_census(self.name);

        if calls <= PROBE_DETAIL_LIMIT {
            debug!("Cutt probe {} call {}: {:?}", self.name, calls, values);
        }
        else if calls % PROBE_CHUNK == 0 {
            debug!("Cutt probe {} {} calls", self.name, calls);
        }
    }

    pub(crate) fn count(&self) {
        self.observe(&[]);
    }

    // A door that reports its values and keeps a peak must increment once. The first version called
    // `observe` and then `sample`, and both incremented, so every peaked door that also printed its
    // values reported twice the calls the game actually made: run 13's `InitializePlateList(list,
    // interval)=12` was six calls, and its first hit lines numbered them 1, 3, 5 and 7 (C53).
    pub(crate) fn observe_peak(&self, values: &[f64], value: f32) {
        self.stamp();
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        // The census records the value for the same doors the peak is worth keeping for: those are the
        // doors that hand a time or a speed, which is what item 70 needs inside a hole window.
        if self.peaked {
            note_hole_census_value(self.name, value);
            let bits = peak_merge(self.peak.load(atomic::Ordering::Relaxed), value);
            self.peak.fetch_max(bits, atomic::Ordering::Relaxed);
        }
        else {
            note_hole_census(self.name);
        }

        if calls <= PROBE_DETAIL_LIMIT {
            debug!("Cutt probe {} call {}: {:?}", self.name, calls, values);
        }
        else if calls % PROBE_CHUNK == 0 {
            debug!("Cutt probe {} {} calls peak {}", self.name, calls, f32::from_bits(self.peak.load(atomic::Ordering::Relaxed)));
        }
    }

    // The frame hot shape: one increment, one max, no slice and no formatting until a chunk boundary.
    pub(crate) fn sample(&self, value: f32) {
        self.stamp();
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        if self.peaked {
            note_hole_census_value(self.name, value);
            let bits = peak_merge(self.peak.load(atomic::Ordering::Relaxed), value);
            self.peak.fetch_max(bits, atomic::Ordering::Relaxed);
        }
        else {
            note_hole_census(self.name);
        }

        if calls % PROBE_CHUNK == 0 {
            debug!("Cutt probe {} {} calls peak {}", self.name, calls, f32::from_bits(self.peak.load(atomic::Ordering::Relaxed)));
        }
    }

    // One clock read per crossing, and only while the suite is armed. Without debug_mode the suite never starts, the
    // read answers -1, and a crossing costs one relaxed store.
    fn stamp(&self) {
        self.last_us.store(super::ProbeSuite::now_us(), atomic::Ordering::Relaxed);
    }

    pub(crate) fn last_us(&self) -> i64 {
        self.last_us.load(atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn stamp_at(&self, us: i64) {
        self.last_us.store(us, atomic::Ordering::Relaxed);
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(atomic::Ordering::Relaxed)
    }

    pub(crate) fn peak_bits(&self) -> u32 {
        self.peak.load(atomic::Ordering::Relaxed)
    }

    pub(crate) fn is_peaked(&self) -> bool {
        self.peaked
    }

    // The label the totals line prints. It carries the argument list on purpose: `SkipRuntime(time)` and
    // `SkipRuntime(frames, keep)` are two different doors, and dropping the list made the log print them
    // under one name.
    pub(crate) fn label(&self) -> &str {
        self.name
    }
}

// The half items 66 and 67 left open. A training cut's hole is now named down to what it yields
// (`UnityEngine.WaitForFixedUpdate`, one poll every 33 ms, 198 of them in run 23's 6583 ms) and down to
// where it sits (one branch of `<PlayTrainingCut>d__70::MoveNext`), but not down to what the poll is
// testing: sampling the coroutine's fields and its owner's fields at both ends of a hole changed one slot
// out of nine, its own `<>1__state`, on every run that read it. A wait that holds nothing is a wait that
// *asks* something, and the only way to see what it asks from outside the method body is to watch which
// doors this fork already counts get crossed while the hole window is open. A door crossed as often as the
// polls is the door the poll re-tests. A door crossed once at the end is what released the cut.
//
// The cost sits with the hole, not on a door: one relaxed load per counted door call, and a lock with a
// hash entry only while a hole window stands open, which is a stretch of a few seconds a couple of times
// a career run, on doors that already do an atomic increment each.
// One door's half of a hole census. The crossings answer item 68 (which door the poll re-tests); the
// lowest and highest value answer what item 70 needs before it can hand the game's own skip door an argument,
// which is the time the cut-in clock is being fed during the wait rather than a guess at it.
#[derive(Default)]
struct HoleCensusEntry {
    crossings: usize,
    lowest: f32,
    highest: f32,
    valued: bool,
}

static HOLE_CENSUS: Lazy<Mutex<Option<FnvHashMap<&'static str, HoleCensusEntry>>>> = Lazy::new(|| Mutex::new(None));
static HOLE_CENSUS_OPEN: AtomicBool = AtomicBool::new(false);

// Printed most crossed first, and capped: the line has to name the poll's own door and whatever the poll
// asks, and a list of every door a 6 s window touched is not something a person can read off a log.
const HOLE_CENSUS_DOOR_LIMIT: usize = 12;

fn note_hole_census(name: &'static str) {
    if !HOLE_CENSUS_OPEN.load(atomic::Ordering::Relaxed) {
        return;
    }

    let mut slot = recover_lock(&HOLE_CENSUS);

    if let Some(table) = slot.as_mut() {
        table.entry(name).or_default().crossings += 1;
    }
}

// Shared with the wait doors outside this module: a hole that holds no animation may still be holding a
// `WaitForSeconds`, and the census line is where that shows up.
pub(crate) fn note_hole_census_value(name: &'static str, value: f32) {
    if !HOLE_CENSUS_OPEN.load(atomic::Ordering::Relaxed) {
        return;
    }

    let mut slot = recover_lock(&HOLE_CENSUS);

    let Some(table) = slot.as_mut() else {
        return;
    };

    let entry = table.entry(name).or_default();

    if entry.valued {
        entry.lowest = entry.lowest.min(value);
        entry.highest = entry.highest.max(value);
    } else {
        entry.lowest = value;
        entry.highest = value;
        entry.valued = true;
    }

    entry.crossings += 1;
}

// Installs the tally before raising the flag, so a door crossed between the two cannot fall between them.
fn open_hole_census() {
    *recover_lock(&HOLE_CENSUS) = Some(FnvHashMap::default());
    HOLE_CENSUS_OPEN.store(true, atomic::Ordering::Relaxed);
}

// Lowers the flag before taking the tally, so a door crossed after the hole is closed is not charged to it.
fn close_hole_census() -> Option<FnvHashMap<&'static str, HoleCensusEntry>> {
    HOLE_CENSUS_OPEN.store(false, atomic::Ordering::Relaxed);
    recover_lock(&HOLE_CENSUS).take()
}

fn hole_census_line(span_ms: i64, from: &str, to: &str, table: &FnvHashMap<&'static str, HoleCensusEntry>) -> String {
    let doors = table.len();
    let crossings: usize = table.values().map(|entry| entry.crossings).sum();

    let mut ranked: Vec<(&str, &HoleCensusEntry)> = table.iter().map(|(name, entry)| (*name, entry)).collect();
    ranked.sort_by(|left, right| right.1.crossings.cmp(&left.1.crossings).then_with(|| left.0.cmp(right.0)));
    ranked.truncate(HOLE_CENSUS_DOOR_LIMIT);

    let listed = ranked
        .iter()
        .map(|(name, entry)| {
            if entry.valued {
                format!("{name} = {} ({} to {} handed)", entry.crossings, entry.lowest, entry.highest)
            } else {
                format!("{name} = {}", entry.crossings)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");

    format!("Cutt probe cut hole census over {span_ms} ms from {from} to {to}: {doors} counted door(s) crossed {crossings} times while that stretch was held off: {listed}")
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
// Peaked rather than counted: an auto skip of a training cut (item 70) has to be asked for in the unit the
// game's own skip takes, and run 39 counted 12 of these calls without ever recording what time was passed.
static CUTT_SKIP_RUNTIME_TIME: CutProbe = CutProbe::peaked("CutInTimelineController::SkipRuntime(time)");
static CUTT_SKIP_RUNTIME_FRAMES: CutProbe = CutProbe::counted("CutInTimelineController::SkipRuntime(frames, keep)");
static CUTT_SKIP_TIME_DIRECT: CutProbe = CutProbe::peaked("CutInTimelineController::SkipTimeDirect(time)");

// The pause, resume and wait setters of the cut-in timeline. A stalled cut reads the census flags on every sample,
// but the door that set a hold is only visible as the time it was last crossed, so each one stamps that time.
static CUTT_PAUSE: CutProbe = CutProbe::counted("CutInTimelineController::Pause(pause)");
static CUTT_RESUME: CutProbe = CutProbe::counted("CutInTimelineController::Resume()");
static CUTT_SET_WAIT_TAP: CutProbe = CutProbe::counted("CutInTimelineController::SetWaitTap(event)");
static CUTT_SET_WAIT_VOICE_END: CutProbe = CutProbe::counted("CutInTimelineController::SetWaitVoiceEnd(event)");
static CUTT_SET_ENABLE_WAITING_UPDATE: CutProbe = CutProbe::counted("CutInTimelineController::SetEnableWaitingUpdate(enable)");
static CUT_STATUS_SKIP: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::Skip(skip)");
static CUTT_WAIT_TAP_ASYNC: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::WaitTapAsync()");
static CUTT_FADE_OUT_RESULT_FLASH: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::FadeOutResultFlash()");
static PLATE_INITIALIZE_LIST: CutProbe = CutProbe::peaked("TrainingParamChangeUI::InitializePlateList(list, interval)");
static MAIN_COROUTINE_DOTWEEN_SCALE: CutProbe = CutProbe::counted("SingleModeMainViewController::CoroutineDoTweenTimeScale()");
static MAIN_WAIT_TAP: CutProbe = CutProbe::counted("SingleModeMainViewController::WaitTap()");
// The training turn's own flow, on `Gallop.SingleModeMainViewController`: the click that opens a turn,
// the two coroutines that send the turn's command, the coroutine the view walks back through, and the
// door that plays the remaining turn change animation. `SendCommandAsync/6` is dumped with
// `Action<SingleModeCommandResult>` and `Action<Cute.Http.ErrorType, int>`, which is the fork's evidence
// that a training turn is driven by a request. Whether the silent wait inside a turn belongs to that
// request or to the client's own pacing decides whether anything in it may be shortened at all (C3, C6,
// C12, C31), so every door here is counted and its arguments and coroutine pointer handed back untouched.
static MAIN_ON_CLICK_TRAINING: CutProbe = CutProbe::counted("SingleModeMainViewController::OnClickTraining()");
static MAIN_COMMON_SEND_COMMAND_ASYNC: CutProbe = CutProbe::counted("SingleModeMainViewController::CommonSendCommandAsync(command, id)");
static MAIN_SEND_COMMAND_ASYNC: CutProbe = CutProbe::counted("SingleModeMainViewController::SendCommandAsync(command, id, int, int, on_result, on_error)");
static MAIN_BACK_FROM_TRAINING: CutProbe = CutProbe::counted("SingleModeMainViewController::BackFromTraining()");
static MAIN_TRY_REMAIN_TURN_CHANGE: CutProbe = CutProbe::counted("SingleModeMainViewController::TryPlayRemainTurnChangeAnimation()");

// The doors the run 9 dump turned from guesses into signatures (A28, A29). The cut in progress flag is
// the boundary v1 was missing, and the driver trio is what actually spends the frames.
static CUTT_SET_IS_PLAYING_CUTT: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::set_IsPlayingCutt(playing)");
static CUTT_GET_IS_PLAYING_CUTT: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::get_IsPlayingCutt()");
static CUTT_IS_AUTO_PLAY: CutProbe = CutProbe::peaked("SingleModeMainTrainingCuttController::IsAutoPlay()");
static CUTT_UPDATE_TRAINING_CUT_IN: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::UpdateTrainingCutIn()");
static CUTT_FIXED_UPDATE_TRAINING_CUT_IN: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::FixedUpdateTrainingCutIn()");
static CUTT_LATE_UPDATE_TRAINING_CUT_IN: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::LateUpdateTrainingCutIn()");
static CUTT_PLAY_TRAINING_CUT: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::PlayTrainingCut(info)");
static CUTT_PLAY_SCENARIO_TRAINING_CUT: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::PlayScenarioTrainingCut(info)");
static CUTT_PLAY_TRAINING_SABORI: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::PlayTrainingSaboriAsync(id)");
static CUTT_PLAY_TRAINING_CUT_END: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::PlayTrainingCutEndAsync(id, flag, flag)");
static CUTT_TRAINING_ASYNC: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::TrainingAsync(id)");
static CUTT_PLAY_IN_TRAINING_STATUS: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::PlayInTrainingStatus()");
static CUTT_PLAY_OUT_TRAINING_STATUS: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::PlayOutTrainingStatus()");
static CUTT_CLEAN_UP_CUTT: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::CleanUpCutt()");
// Item 88: the game's own skip for a training cutt, on the class that owns the stat change
// presentation. No hook file in this fork has ever named it, so before a lever is allowed to call it a
// run has to say whether the client reaches it by itself (A4: installed is not the same as called).
static STORY_SKIP_TRAINING_CUTT: CutProbe = CutProbe::counted("StoryViewController::SkipTrainingCutt()");
// Item 89: the duration door next to the scale door `AnimationSpeed` owns on the same class. The scale
// half is measured (run 48 handed the door its 30.0 ceiling while the reader door still peaked at the
// game's own 5.680, item 82), so what is left of the cut's length is a duration asked for elsewhere.
// Sampled, not counted: what matters is the number the game took.
static UTILS_GET_HIGH_SPEED_PLAY_DURATION: CutProbe = CutProbe::peaked("SingleModeUtils::GetHighSpeedPlayDuration()");
// Item 90: `Gallop.TimelineTrainingCuttController` keeps a cut clock in public float fields `StartTime`,
// `DelayTime`, `TotalTime` and in delegate slots wired by `Init`. Nothing counts that class today, so no
// run knows whether a training cut on this client runs through it. One counted door where the delegates
// get wired is the answer.
static TIMELINE_CUTT_INIT: CutProbe = CutProbe::counted("TimelineTrainingCuttController::Init(delegates)");
static CUTT_GET_TOTAL_TIME: CutProbe = CutProbe::peaked("CutInTimelineController::GetTotalTime()");
static CUTT_GET_SPEED: CutProbe = CutProbe::peaked("CutInTimelineController::get_Speed()");
static CUTT_SET_SKIP_FRAME: CutProbe = CutProbe::counted("CutInTimelineController::set_SkipFrame(frames)");
static CUTT_SET_IS_AUTO_PLAY: CutProbe = CutProbe::counted("CutInTimelineController::set_IsAutoPlay(playing)");
static CUTT_GET_IS_AUTO_PLAY: CutProbe = CutProbe::peaked("CutInTimelineController::get_IsAutoPlay()");
// `SingleModeMainViewTrainingCutStatus::PlayIn` and `SingleModeMainViewHpGauge::SetProgressbarBlendTime`
// are no longer counted here: AnimationSpeed installs scaling hooks on those two addresses (C51), and a
// probe cannot hook an address a second time to watch its own scaling hook. Their call counts reach this
// report through AnimationSpeed::TRAINING_HIT_SLOTS instead.
static STATUS_PLAY_OUT: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::PlayOut(flag, action)");
static STATUS_INTERVAL_OUT: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatus::GetIntervalOutBegine(time)");
static STATUS_RANK_UP_HIGH_SPEED: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatus::WillRankUpInHighSpeedMode()");
static STATUS_EXIST_PLAYING_FRAME: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::ExistPlayingFrame()");
// The doors the friendship cut actually spends its wall inside. Run 26 is why these are here and
// `PlayIn/4` alone was not enough: it read `PlayIn 2.4 -> 0.12` and the status panel still held 7866,
// 8182 and 7866 ms in the same session, because the animation runs on `CoroutinePlayIn/2 ->
// IEnumerator(float, int)` next door. A friendship cut (2.0 to 2.2 s of animation in every arm of
// `training_cut_speed`) plays through the tag cut-in player's own effect doors and this frame class, and
// none of them had ever been counted. Every one is observed only: the float is handed back as the game
// computed it, and the ledger needs a run to say which of these doors a lever belongs on (item 79).
static STATUS_PLAY_IN: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatus::PlayIn(interval, frame, flag, action)");
static STATUS_COROUTINE_PLAY_IN: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatus::CoroutinePlayIn(interval, frame)");
static STATUS_FRAME_PLAY_IN: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatusFrame::PlayIn(int, int, int, int, int, int, time, int, flag)");
static STATUS_FRAME_PLAY_IN_ROOT_MOTION: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatusFrame::PlayInRootMotion(time)");
static STATUS_FRAME_PLAY_OUT: CutProbe = CutProbe::peaked("SingleModeMainViewTrainingCutStatusFrame::PlayOut(flag, time, action)");
static STATUS_FRAME_PRE_IN: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatusFrame::PlayPreIn()");
static STATUS_FRAME_SKIP: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatusFrame::Skip(int, int, int, int, int)");
// `CreateLineEffect/1` and `PlayLineEffect/0` are not counted here any more: AnimationSpeed arms both addresses
// and reports the writes it makes on them through its own hit slots, because they are where the tag cut-in lever
// writes the Animator speed (item 79). `PlayButtonEffect/3` is not armed at all: its second parameter is a 12 byte
// struct and A5 admits only structs a run has proved travel in a general register.
static TAG_PLAYER_STOP_LINE_EFFECT: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::StopLineEffect()");
static TAG_PLAYER_DESTROY_BUTTON_EFFECT: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::DestroyButtonEffect()");
static TAG_PLAYER_PLAY_SUCCESS_TEXT: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::PlaySuccessTextEffect()");
static TAG_PLAYER_DESTROY_LINE_EFFECT: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::DestroyLineEffect()");
// The doors that hand a `FlashPlayer` to a support member cut-in. Run 41 proved the tag cut-in's `Animator` is not
// what paces its 1.2 s effect leg, and a friendship cut-in is built on the object these doors return.
static SUPPORT_MEMBER_PLAY_CUTIN: CutProbe = CutProbe::counted("TrainingParamChangeSupportMemberA2U::PlayMemberCutin(ids, parent, action)");
static SUPPORT_MEMBER_PLAY_SOUL_CUTIN: CutProbe = CutProbe::counted("TrainingParamChangeSupportMemberA2U::PlayAoharuSoulCutin(index, parent, action, action)");
static SUPPORT_MEMBER_PLAY_SP_SOUL_CUTIN: CutProbe = CutProbe::counted("TrainingParamChangeSupportMemberA2U::PlayAoharuSpSoulCutin(index, parent, action, action)");
static SUPPORT_MEMBER_PLAY_OUT_CUTIN: CutProbe = CutProbe::counted("TrainingParamChangeSupportMemberA2U::PlayOutMemberCutin(player, index, action)");
static TAG_PLAYER_SET_RESULT_FLASH: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::SetResultFlashPlayer(player)");
// The `FlashPlayer` the last member cut-in door handed back, and the one the tag cut-in player was set up with.
// The addresses are compared in `note_flash_player_pair`, which is bounded so this stays a census.
static SUPPORT_MEMBER_LAST_FLASH_PLAYER: AtomicUsize = AtomicUsize::new(0);
static TAG_PLAYER_RESULT_FLASH_PLAYER: AtomicUsize = AtomicUsize::new(0);
static FLASH_PLAYER_MATCH_LOGGED: AtomicUsize = AtomicUsize::new(0);
// `Gallop.FlashPlayer` is the class a training cut-in effect is played on (`introspect-run45.log:26446`): it wraps an
// `AnimateToUnity.AnMotion` and its `Play` doors are the only ones on that class that take a number. Run 42 handed
// the tag cut-in player 167 flash players through `SetResultFlashPlayer` and reached none of these doors, because
// no build had armed them yet, so the 1.2 s effect leg has never been measured from the object that runs it.
static FLASH_PLAY_ACTION: CutProbe = CutProbe::counted("FlashPlayer::Play(action)");
static MOTION_PLAY_LABEL_FLOAT: CutProbe = CutProbe::peaked("AnMotion::SetMotionPlay(label, float)");
static MOTION_PLAY_LABEL_INT: CutProbe = CutProbe::peaked("AnMotion::SetMotionPlay(label, int)");
static MOTION_PLAY_FLOAT: CutProbe = CutProbe::peaked("AnMotion::SetMotionPlay(float)");
static MOTION_PLAY: CutProbe = CutProbe::counted("AnMotion::SetMotionPlay");
static MOTION_STOP: CutProbe = CutProbe::counted("AnMotion::SetMotionStop");
static FLASH_PAUSE: CutProbe = CutProbe::counted("FlashPlayer::Pause(flag)");
static FLASH_RESUME: CutProbe = CutProbe::counted("FlashPlayer::Resume()");
static FLASH_STOP: CutProbe = CutProbe::counted("FlashPlayer::Stop()");
static FLASH_END: CutProbe = CutProbe::counted("FlashPlayer::End()");
static GRAPHIC_SETTINGS_EFFECT_TIME_SCALE: CutProbe = CutProbe::counted("GraphicSettings::SetEffectTimeScale(scale)");
static FLASH_ACTION_PLAY_WITH_TIME: CutProbe = CutProbe::counted("FlashActionPlayer::Play(label, time, label)");
static FLASH_ACTION_PLAY_TWO_LABELS: CutProbe = CutProbe::counted("FlashActionPlayer::Play(label, label)");
// `field _lastMotionSpeed [float]` on the flash player (`introspect-run45.log:26509`): the speed the wrapper last played
// its motion at, read with the same field accessor the plate UI's cascade floats use, so a run reports the number
// the game holds instead of inferring it from a wall. A client with no such field reads a zero, which is reported
// as a zero and not as a value this build never read.
def_field_value_accessors!(get flash_last_motion_speed, FLASH_LAST_MOTION_SPEED_FIELD, f32);
static FLASH_PLAY_LOGGED: AtomicUsize = AtomicUsize::new(0);
// The friendship split v1 could not make. `IsValidTag` answers whether the cards a training produced
// carry a friendship, and the tag cut-in player is the door a friendship cut-in is played through.
static TAG_IS_VALID_TAG: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::IsValidTag(result, cards)");
static TAG_PLAYER_IS_VALID_TAG: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::IsValidTag(cards)");
static TAG_PLAYER_PLAY_CUT_IN: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::PlayCutIn(cards, done)");
static TAG_PLAYER_PLAY_CUT_OUT: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::PlayCutInOut(done)");

pub(crate) static PROBES: [&CutProbe; 109] = [
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
    &CUTT_PAUSE,
    &CUTT_RESUME,
    &CUTT_SET_WAIT_TAP,
    &CUTT_SET_WAIT_VOICE_END,
    &CUTT_SET_ENABLE_WAITING_UPDATE,
    &CUT_STATUS_SKIP,
    &CUTT_WAIT_TAP_ASYNC,
    &CUTT_FADE_OUT_RESULT_FLASH,
    &PLATE_INITIALIZE_LIST,
    &MAIN_COROUTINE_DOTWEEN_SCALE,
    &MAIN_WAIT_TAP,
    &CUTT_SET_IS_PLAYING_CUTT,
    &CUTT_GET_IS_PLAYING_CUTT,
    &CUTT_IS_AUTO_PLAY,
    &CUTT_UPDATE_TRAINING_CUT_IN,
    &CUTT_FIXED_UPDATE_TRAINING_CUT_IN,
    &CUTT_LATE_UPDATE_TRAINING_CUT_IN,
    &CUTT_PLAY_TRAINING_CUT,
    &CUTT_PLAY_SCENARIO_TRAINING_CUT,
    &CUTT_PLAY_TRAINING_SABORI,
    &CUTT_PLAY_TRAINING_CUT_END,
    &CUTT_TRAINING_ASYNC,
    &CUTT_PLAY_IN_TRAINING_STATUS,
    &CUTT_PLAY_OUT_TRAINING_STATUS,
    &CUTT_CLEAN_UP_CUTT,
    &CUTT_GET_TOTAL_TIME,
    &CUTT_GET_SPEED,
    &CUTT_SET_SKIP_FRAME,
    &CUTT_SET_IS_AUTO_PLAY,
    &CUTT_GET_IS_AUTO_PLAY,
    &STATUS_PLAY_OUT,
    &STATUS_INTERVAL_OUT,
    &STATUS_RANK_UP_HIGH_SPEED,
    &STATUS_EXIST_PLAYING_FRAME,
    &STATUS_PLAY_IN,
    &STATUS_COROUTINE_PLAY_IN,
    &STATUS_FRAME_PLAY_IN,
    &STATUS_FRAME_PLAY_IN_ROOT_MOTION,
    &STATUS_FRAME_PLAY_OUT,
    &STATUS_FRAME_PRE_IN,
    &STATUS_FRAME_SKIP,
    &TAG_IS_VALID_TAG,
    &TAG_PLAYER_IS_VALID_TAG,
    &TAG_PLAYER_PLAY_CUT_IN,
    &TAG_PLAYER_PLAY_CUT_OUT,
    &TAG_PLAYER_STOP_LINE_EFFECT,
    &TAG_PLAYER_DESTROY_BUTTON_EFFECT,
    &TAG_PLAYER_PLAY_SUCCESS_TEXT,
    &TAG_PLAYER_DESTROY_LINE_EFFECT,
    &SUPPORT_MEMBER_PLAY_CUTIN,
    &SUPPORT_MEMBER_PLAY_SOUL_CUTIN,
    &SUPPORT_MEMBER_PLAY_SP_SOUL_CUTIN,
    &SUPPORT_MEMBER_PLAY_OUT_CUTIN,
    &TAG_PLAYER_SET_RESULT_FLASH,
    &FLASH_PLAY_ACTION,
    &MOTION_PLAY_LABEL_FLOAT,
    &MOTION_PLAY_LABEL_INT,
    &MOTION_PLAY_FLOAT,
    &MOTION_PLAY,
    &MOTION_STOP,
    &FLASH_PAUSE,
    &FLASH_RESUME,
    &FLASH_STOP,
    &FLASH_END,
    &GRAPHIC_SETTINGS_EFFECT_TIME_SCALE,
    &FLASH_ACTION_PLAY_WITH_TIME,
    &FLASH_ACTION_PLAY_TWO_LABELS,
    &HP_GAUGE_PLAY_IN,
    &HP_GAUGE_PLAY_VALUE,
    &HP_GAUGE_PLAY_OUT,
    &STATUS_PLAY_PRE_IN,
    &STATUS_PLAY_END,
    &PLATE_GET_IS_AUTO_PLAY,
    &PLATE_PLAY_ICON,
    &PLATE_IS_GROUP_PLAY,
    &PLATE_UPDATE,
    &PLATE_START_SEQUENCE,
    &PLATE_START_GROUP_TYPEWRITE,
    &PLATE_START_TYPEWRITE,
    &PLATE_ON_NEXT_TYPEWRITE,
    &PLATE_ON_END_TYPE_WRITE,
    &PLATE_ON_ALL_TYPEWRITE_END,
    &PLATE_ON_TAP_SCREEN,
    &PLATE_TAP_BUTTON_ORDER,
    &PLATE_INITIALIZE_FLASH,
    &PLATE_INITIALIZE,
    &STORY_PLAY_PARAMETER_CHANGE,
    &MAIN_ON_CLICK_TRAINING,
    &MAIN_COMMON_SEND_COMMAND_ASYNC,
    &MAIN_SEND_COMMAND_ASYNC,
    &MAIN_BACK_FROM_TRAINING,
    &MAIN_TRY_REMAIN_TURN_CHANGE,
    &STORY_SKIP_TRAINING_CUTT,
    &UTILS_GET_HIGH_SPEED_PLAY_DURATION,
    &TIMELINE_CUTT_INIT,
    &CUTT_LEVER_SKIP_STEPS,
];

// One cut-in run, opened by `ResetCurrentTime` and closed by the next one. The wall clock between the
// two is what the player waits, and the peak of `get_CurrentTime` reached inside it is how long the
// cut's own timeline ran, which is the number a rate hook has to shorten.
// Which part of the game a cut run started in. `ResetCurrentTime` fires for every cut-in the client
// plays, a race skill cut-in and a gacha reveal included, so an unattributed run count cannot tell a
// training animation from any other one. A regular training turn and a friendship training turn both
// run through the same training cut-in classes (A19), so separating those two needs the
// `TagTrainingCutInPlayer` signatures the next dump has to provide. What this can separate today is
// the screen, and that is what makes the number readable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ViewBucket { Training, Story, StoryEvent, Gacha, Race, Other }

pub(crate) const BUCKET_COUNT: usize = 6;
pub(crate) const BUCKET_NAMES: [&str; BUCKET_COUNT] = ["training screen", "story", "story event screen", "gacha", "race scene", "other"];

// Read from the game's own `ViewId` so a game update that moves a number shows up here as a compile
// error rather than as a silent mislabel.
const VIEW_TRAINING: [i32; 5] = [
    ViewId::SingleModeMonthStart as i32, ViewId::SingleModeMain as i32, ViewId::SingleModePaddock as i32,
    ViewId::SingleModeConfirmComplete as i32, ViewId::SingleModeResult as i32,
];
const VIEW_STORY: i32 = ViewId::Story as i32;
// The story event mission screen, and the two career end screens. `SingleModeSuccessionCut` and
// `SingleModeSuccessionEvent` play their inspiration cut-ins through the same timeline controller this
// probe hooks, and run 31 ended stuck on 1501 with every line about it reading `other`, which is how a
// career end stall stayed invisible in the census until the view id table named it after the fact (C62).
const VIEW_STORY_EVENT: [i32; 3] = [
    ViewId::StoryEventMission as i32,
    ViewId::SingleModeSuccessionCut as i32,
    ViewId::SingleModeSuccessionEvent as i32,
];
const VIEW_GACHA: i32 = ViewId::GachaMain as i32;

pub(crate) fn bucket_for(view_id: i32, in_race_scene: bool) -> ViewBucket {
    // The scene check first: a skill cut-in inside a race reaches the same timeline controller this
    // probe hooks, and it is not a training animation. It reads the scene id `SceneManager::AlterUpdate`
    // already caches, so it costs no game call.
    if in_race_scene {
        return ViewBucket::Race;
    }

    if VIEW_TRAINING.contains(&view_id) {
        ViewBucket::Training
    }
    else if view_id == VIEW_STORY {
        ViewBucket::Story
    }
    // The story event mission screen is its own view, and a cut-in played there is the thing the story
    // event probe is measuring. The career end screens are filed here for the same reason.
    else if VIEW_STORY_EVENT.contains(&view_id) {
        ViewBucket::StoryEvent
    }
    else if view_id == VIEW_GACHA {
        ViewBucket::Gacha
    }
    else {
        ViewBucket::Other
    }
}

// Asked for by a cut run and, since the frame clock, by every game tick. `GetCurrentViewId` is the
// method the mod already resolves for other features, and its wrapper refuses an unresolved address
// instead of jumping to 0 (C1). The per tick caller is why `SceneManager::instance` caches its method.
pub(crate) fn current_view_id() -> i32 {
    let scene_manager = SceneManager::instance();

    if scene_manager.is_null() {
        return 0;
    }

    SceneManager::GetCurrentViewId(scene_manager)
}

// Whether the game is standing on one of the career screens this probe already counts as a training
// session. `high_speed_settings` writes the game's own story and training HighSpeedType values, and run 16
// logged its raise at 09:00:22 and its restore at 09:03:24.9 with one of these screens up, inside a window
// where a training cut runtime never terminated. Both values live on the save loader and in StoryManager's
// saved setting, so the write the next turn reads is the one taken before the turn, not the one taken
// during it. The ids come from the game's own `ViewId`, so a client that moves one shows up here as a
// compile error rather than as a gate that quietly holds nothing.
//
// This is a proxy, and it is kept as one: it answers that a career screen is up, not that a cut is open,
// and run 57 shows what the difference costs - its pass held at 20:45:29.864 (`run log/hachimi-run57.log:1854`)
// while the cut census six seconds earlier read `cut runs 1 wall 8898 ms timeline 0 ms open 0 ms` (`:1827`).
// `529e4f5` dropped it from the write guard and left the guard with `cut_gate_engaged()` alone, which closed
// none of the passes this proxy refused; `HighSpeedSetting::plan_pass` and `StoryTimelineController::plan_pass`
// now refuse on this answer *or* the gate, so the boundary added by the gate is added to the coverage instead
// of replacing it.
pub(crate) fn on_a_career_screen() -> bool {
    VIEW_TRAINING.contains(&current_view_id())
}

// What the game last answered for "do these cards carry a friendship", kept so the cut that opens next
// can be labelled with the answer that decided it (A28).
const TAG_UNKNOWN: usize = 0;
const TAG_FRIENDSHIP: usize = 1;
const TAG_REGULAR: usize = 2;
const TAG_COUNT: usize = 3;
const TAG_NAMES: [&str; TAG_COUNT] = ["tag answer unknown", "friendship cut", "regular cut"];

static LAST_TAG_ANSWER: AtomicUsize = AtomicUsize::new(TAG_UNKNOWN);
static RUN_OPENED_MS: AtomicI64 = AtomicI64::new(-1);
static RUN_VIEW_ID: AtomicI32 = AtomicI32::new(0);
static RUN_BUCKET: AtomicUsize = AtomicUsize::new(BUCKET_COUNT - 1);
static RUN_TAG_PLAYER_SEEN: AtomicUsize = AtomicUsize::new(0);
static RUN_TAG_NAME: AtomicUsize = AtomicUsize::new(TAG_UNKNOWN);
static RUN_BUCKET_COUNT: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static RUN_BUCKET_VIEW: [AtomicI32; BUCKET_COUNT] = [const { AtomicI32::new(0) }; BUCKET_COUNT];
static RUN_BUCKET_WALL_MS: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static RUN_BUCKET_PEAK_MS: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static RUN_TAG_COUNT: [AtomicUsize; TAG_COUNT] = [const { AtomicUsize::new(0) }; TAG_COUNT];
static RUN_TAG_WALL_MS: [AtomicI64; TAG_COUNT] = [const { AtomicI64::new(0) }; TAG_COUNT];
static RUN_PEAK_BITS: AtomicU32 = AtomicU32::new(0);
static RUNS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static RUN_WALL_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static RUN_PEAK_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
// What the cut-in engine says about itself: the length of the animation in frames, the last frame it
// reached, and the frame rate it is playing against (A29).
static TIMELINE_TOTAL_FRAMES_PEAK: AtomicU32 = AtomicU32::new(0);
static TIMELINE_CURRENT_FRAME_PEAK: AtomicU32 = AtomicU32::new(0);
static TIMELINE_TARGET_FPS: AtomicU32 = AtomicU32::new(0);

// Run 11 measured 55 cut runs at 138,604 ms inside 1,210,471 ms of training screen frames, so about
// 1,015 s of that screen was neither a cut run nor a cut in, and nothing said what it was. These three
// clocks split what is left: how long a cut stayed open after it asked for a tap, how long the game sat
// between one cut closing and the next opening, and how the plate list interval the game hands
// `InitializePlateList` lines up with the wall clock between two of its calls.
static RUN_TAP_REQUESTED_MS: AtomicI64 = AtomicI64::new(-1);
static RUN_CLOSED_MS: AtomicI64 = AtomicI64::new(-1);
static TAP_WAIT_RUNS: AtomicUsize = AtomicUsize::new(0);
static TAP_WAIT_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static TAP_WAIT_BUCKET_RUNS: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static TAP_WAIT_BUCKET_MS: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static CUT_GAP_RUNS: AtomicUsize = AtomicUsize::new(0);
static CUT_GAP_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static CUT_GAP_WORST_MS: AtomicI64 = AtomicI64::new(0);
static CUT_GAP_BUCKET_MS: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static PLATE_LAST_MS: AtomicI64 = AtomicI64::new(-1);
static PLATE_STEP_RUNS: AtomicUsize = AtomicUsize::new(0);
static PLATE_STEP_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static PLATE_STEP_WORST_MS: AtomicI64 = AtomicI64::new(0);

// A gap is not one thing. Run 12 measured four of them at a 14,897.8 ms mean with a 31,867 ms worst and
// said nothing about where the wall went, and a wall that is not split cannot be aimed at. These legs cut
// each gap at the plate passes that ran inside it: the wait before the first pass, the passes themselves,
// the idle between one pass's completion and the next pass's entry, and the time from the last completion
// to the next cut opening. All four come from one clock and always add up to the gap, so whatever is left
// over shows as a leg instead of being guessed at. A gap with no plate pass between its ends is counted
// apart rather than given invented legs.
//
// The pass legs come from the cascade window itself - entry at `InitializePlateList` / `StartSequence` to
// completion at `OnAllTypewriteEnd` - because the marks this split used to read were plate *calls*: run 49b's
// `plate list steps 9 mean 14,421.2 ms` and `the pass itself mean 26,634.3 ms worst 79,903 ms` are intervals
// from one pass to the next, and a cascade the fork bounds at 0.25 s per plate cannot cost that (ledger
// item 79: "the probe needs the pass measured entry to completion rather than to the next pass").
static GAP_PLATE_FIRST_MS: AtomicI64 = AtomicI64::new(-1);
// The completion of the last pass that closed inside the open gap, and the sum of the spans of the passes
// that closed inside it. Both are written by `close_plate_pass` and read once when the next cut opens.
static GAP_PASS_LAST_END_MS: AtomicI64 = AtomicI64::new(-1);
static GAP_PASS_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_BEFORE_RUNS: AtomicUsize = AtomicUsize::new(0);
static GAP_LEG_BEFORE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_BEFORE_WORST_MS: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_PLATE_RUNS: AtomicUsize = AtomicUsize::new(0);
static GAP_LEG_PLATE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_PLATE_WORST_MS: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_BETWEEN_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_BETWEEN_WORST_MS: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_AFTER_RUNS: AtomicUsize = AtomicUsize::new(0);
static GAP_LEG_AFTER_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_AFTER_WORST_MS: AtomicI64 = AtomicI64::new(0);
static GAP_WITHOUT_PLATE_RUNS: AtomicUsize = AtomicUsize::new(0);

// The open wall splits at the tap request: what the cut played before it asked, and what it waited for
// after. Run 12 read cuts that stayed open between 2,500 and 10,600 ms while the tap wait inside them was
// 436.5 ms mean, so the wall is almost none tap.
static WALL_OPEN_TO_TAP_RUNS: AtomicUsize = AtomicUsize::new(0);
static WALL_OPEN_TO_TAP_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static WALL_OPEN_TO_TAP_WORST_MS: AtomicI64 = AtomicI64::new(0);

// The animation doors run 11 never looked at, on the classes that hold the gauge and the param plates.
// Counting only: an argument is recorded and handed back untouched.
static HP_GAUGE_PLAY_IN: CutProbe = CutProbe::counted("SingleModeMainViewHpGauge::PlayIn()");
static HP_GAUGE_PLAY_VALUE: CutProbe = CutProbe::counted("SingleModeMainViewHpGauge::PlayValue(value)");
static HP_GAUGE_PLAY_OUT: CutProbe = CutProbe::counted("SingleModeMainViewHpGauge::PlayOut()");
static STATUS_PLAY_PRE_IN: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::PlayPreIn()");
static STATUS_PLAY_END: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::PlayEnd()");
static PLATE_GET_IS_AUTO_PLAY: CutProbe = CutProbe::counted("TrainingParamChangeUI::get_IsAutoPlay()");

// The plate cascade as the class that builds it exposes it. Run 13 put 10.3 s inside a training cut with
// six `InitializePlateList(list, interval)` calls at 1.0 s and twelve gauge plays, and the cut timeline
// reporting 2.4 s of total length, so the wall is the cascade rather than the timeline. Run 14 then
// scaled that interval 20x, reached it 22 times with `1.0 -> 0.05`, and the wall did not move: cut run 1
// took 12,965 ms with 9,669 ms of it between the cut-in ending and the status playing out, while
// `PlayIcon` was never reached and `IsGroupPlay` was reached 104 times and `Update` 1,560. So the
// interval this fork scales is not what paces a cascade the game builds as a group (C54), and the doors
// below stand on the chain that does: `StartSequence`, `StartGroupTypewrite`, `StartTypewrite(index)`,
// `OnNextTypewrite`, `OnEndTypeWrite(index)`, `OnAllTypewriteEnd`, `OnTapScreen`, `SetTapButtonOrder`,
// plus the two entry doors that hand the cascade its timings, `InitializeFlash(list, interval, flag,
// canvas)` and the owner coroutine `StoryViewController::PlayParameterChangeAsync(list, delay)`. All of
// them are counted and their arguments handed back untouched: this round is about naming the number the
// cascade waits on, not shortening one.
static PLATE_PLAY_ICON: CutProbe = CutProbe::counted("TrainingParamChangeUI::PlayIcon(index, info)");
static PLATE_IS_GROUP_PLAY: CutProbe = CutProbe::counted("TrainingParamChangeUI::IsGroupPlay(info)");
static PLATE_UPDATE: CutProbe = CutProbe::counted("TrainingParamChangeUI::Update()");
static PLATE_START_SEQUENCE: CutProbe = CutProbe::counted("TrainingParamChangeUI::StartSequence()");
static PLATE_START_GROUP_TYPEWRITE: CutProbe = CutProbe::counted("TrainingParamChangeUI::StartGroupTypewrite()");
static PLATE_START_TYPEWRITE: CutProbe = CutProbe::counted("TrainingParamChangeUI::StartTypewrite(index)");
static PLATE_ON_NEXT_TYPEWRITE: CutProbe = CutProbe::counted("TrainingParamChangeUI::OnNextTypewrite()");
static PLATE_ON_END_TYPE_WRITE: CutProbe = CutProbe::counted("TrainingParamChangeUI::OnEndTypeWrite(index)");
static PLATE_ON_ALL_TYPEWRITE_END: CutProbe = CutProbe::counted("TrainingParamChangeUI::OnAllTypewriteEnd()");
static PLATE_ON_TAP_SCREEN: CutProbe = CutProbe::counted("TrainingParamChangeUI::OnTapScreen()");
static PLATE_TAP_BUTTON_ORDER: CutProbe = CutProbe::counted("TrainingParamChangeUI::SetTapButtonOrder(order)");
static PLATE_INITIALIZE_FLASH: CutProbe = CutProbe::peaked("TrainingParamChangeUI::InitializeFlash(list, interval, flag, canvas)");
static PLATE_INITIALIZE: CutProbe = CutProbe::peaked("TrainingParamChangeUI::Initialize(content, list, canvas, action, first, second, ...)");
static STORY_PLAY_PARAMETER_CHANGE: CutProbe = CutProbe::peaked("StoryViewController::PlayParameterChangeAsync(list, delay)");

def_field_value_accessors!(get plate_ui_delay, PLATE_UI_DELAY_FIELD, f32);
def_field_value_accessors!(get plate_ui_tap_wait, PLATE_UI_TAP_WAIT_FIELD, f32);
def_field_value_accessors!(get plate_ui_group_interval, PLATE_UI_GROUP_INTERVAL_FIELD, f32);
def_field_value_accessors!(get plate_ui_sequence_interval, PLATE_UI_SEQUENCE_INTERVAL_FIELD, f32);

// The spacing of the events that put a stat number on screen, measured only inside one window so that a
// spacing across two turns cannot be read as part of a cut. Run 14 showed the window cannot be the cut
// alone: it printed `gauge plays 0` and `plate icon plays 0` while the same run counted 21 gauge plays
// and 22 plate list calls, because the cascade runs mostly between cuts (C55). The cascade window below
// is the second window a spacing may count in.
static GAUGE_PLAY_LAST_MS: AtomicI64 = AtomicI64::new(-1);
static GAUGE_PLAY_RUNS: AtomicUsize = AtomicUsize::new(0);
static GAUGE_PLAY_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAUGE_PLAY_WORST_MS: AtomicI64 = AtomicI64::new(0);
static PLATE_PLAY_LAST_MS: AtomicI64 = AtomicI64::new(-1);
static PLATE_PLAY_RUNS: AtomicUsize = AtomicUsize::new(0);
static PLATE_PLAY_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static PLATE_PLAY_WORST_MS: AtomicI64 = AtomicI64::new(0);

// One cascade of stat plates: opened by the first `InitializePlateList` after the previous one ended,
// closed by `OnAllTypewriteEnd` or by the cut teardown. Run 14 measured the wall clock *between* plate
// list calls (21 steps, 5,129.3 ms mean) but never how long one cascade took, which is the difference
// between a cascade that runs for 5 s and a game that starts a new cascade every 5 s. The typewrite
// spacing is the same question one level down: how far apart the plates inside one cascade arrive.
static PLATE_PASS_OPEN_MS: AtomicI64 = AtomicI64::new(-1);
static PLATE_PASS_RUNS: AtomicUsize = AtomicUsize::new(0);
static PLATE_PASS_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static PLATE_PASS_WORST_MS: AtomicI64 = AtomicI64::new(0);
static PLATE_TYPEWRITE_LAST_MS: AtomicI64 = AtomicI64::new(-1);
static PLATE_TYPEWRITE_RUNS: AtomicUsize = AtomicUsize::new(0);
static PLATE_TYPEWRITE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static PLATE_TYPEWRITE_WORST_MS: AtomicI64 = AtomicI64::new(0);

// What one cascade holds while it types, and what its plates actually did, read at the doors the census
// already arms. C54 is the finding that the float a caller hands `InitializePlateList` is not the number the
// group typewrite path waits on; C62 is the finding that the interval is a timing beat above about 0.25 s.
// Both were answered per *run*, off peak values. This is the same question per cascade: the class's own
// pacing numbers (`_delay`, `_tapWait`, `_groupInterval`, `_sequenceInterval`) are instance floats the probe
// can read at `StartSequence`, `StartGroupTypewrite`, `StartTypewrite`, `OnNextTypewrite`, `OnEndTypeWrite`
// and `OnTapScreen`, and the two literals it would rather read are `static const` this fork cannot reach -
// `TrainingParamChangePlate.TYPEWRITE_DURATION / NEXT_WAIT_DURATION` print `is a compile-time constant` at
// `AnimationSpeed` and `A const has no slot in the static data area` at `introspect`, and every
// `TrainingParamChangeUI` constant on this path (`GROUP_TYPEWRITE_INTERVAL_HIGH`, `SEQUENCE_INTERVAL`,
// `SEQUENCE_INTERVAL_SUCCESSION`, `PLAY_SPEED_DEFAULT`, `PLAY_SPEED_FOR_SKIP`) is the same kind of literal.
// Their size is therefore measured off the doors that stand for them: `StartTypewrite` to `OnEndTypeWrite`
// is the plate's own type write, and `OnEndTypeWrite` to the next `OnNextTypewrite` is the wait before the
// next plate starts.
#[derive(Clone, Default)]
struct CascadeRead {
    index: usize,
    open_ms: i64,
    close_ms: i64,
    closed_by: &'static str,
    // The interval the caller handed `InitializePlateList` before this fork scaled it, and the numbers the
    // UI itself holds.
    interval: f32,
    delay: f32,
    tap_wait: f32,
    group_interval: f32,
    sequence_interval: f32,
    plates: usize,
    groups: usize,
    advances: usize,
    taps: usize,
    first_start_ms: i64,
    last_start_ms: i64,
    // The legs the two unreachable literals own.
    write_start_ms: i64,
    write_end_ms: i64,
    write_ms: i64,
    write_runs: usize,
    wait_ms: i64,
    wait_runs: usize,
    // Plate to plate spacings inside this cascade, bounded so a cascade with a thousand plates costs no
    // allocation. The count is the number seen, not the number kept.
    beats: [i64; CASCADE_BEAT_LIMIT],
    beats_seen: usize,
}

impl CascadeRead {
    // The wall clock between two typewrite starts of this cascade: one stat number landing after the next,
    // which is the beat a player sees and the number a pacing lever has to match. None when the cascade
    // typed one plate or never typed one.
    fn beat_ms(&self) -> Option<i64> {
        if self.plates < 2 || self.first_start_ms < 0 || self.last_start_ms <= self.first_start_ms {
            return None;
        }

        Some((self.last_start_ms - self.first_start_ms) / (self.plates as i64 - 1))
    }
}

// The numbers a cascade could be waiting on, in the order the class carries them, plus the answer that it
// waits on none of them.
const CASCADE_BEAT_LIMIT: usize = 16;
const PACE_CANDIDATES: usize = 5;
const PACE_NONE: usize = PACE_CANDIDATES;
const PACE_NAMES: [&str; PACE_NONE + 1] = ["the interval handed to InitializePlateList", "TrainingParamChangeUI._delay", "TrainingParamChangeUI._tapWait", "TrainingParamChangeUI._groupInterval", "TrainingParamChangeUI._sequenceInterval", "none of these numbers"];
static PACE_RUNS: [AtomicUsize; PACE_NONE + 1] = [const { AtomicUsize::new(0) }; PACE_NONE + 1];
static CASCADE_READ: Lazy<Mutex<Option<CascadeRead>>> = Lazy::new(|| Mutex::new(None));
static CASCADE_INDEX: AtomicUsize = AtomicUsize::new(0);
static CASCADE_REPORTED: AtomicUsize = AtomicUsize::new(0);

// The span of each cascade that closed, in close order, with the number of the cascade it belongs to.
// Kept apart from the running total and the max because those two cannot separate cascades: a total only
// names one cascade's span when exactly one cascade closed between two readings (C56). A fixed ring, no
// allocation, written once per cascade close on the plate door - which is once per cascade of stat plates,
// not once per frame - and read once per report. When a run closes more cascades between two readings than
// the ring holds, the report says which spans it is not showing rather than letting the replay assume it
// read them all.
const CASCADE_SPAN_RING: usize = 16;
static CASCADE_SPANS: [AtomicI64; CASCADE_SPAN_RING] = [const { AtomicI64::new(0) }; CASCADE_SPAN_RING];
static CASCADE_SPAN_INDEXES: [AtomicUsize; CASCADE_SPAN_RING] = [const { AtomicUsize::new(0) }; CASCADE_SPAN_RING];
static CASCADE_SPAN_WRITES: AtomicUsize = AtomicUsize::new(0);
static CASCADE_SPANS_PRINTED: AtomicUsize = AtomicUsize::new(0);

fn mean_of(total: i64, count: usize) -> f64 {
    match count {
        0 => 0.0,
        n => total as f64 / n as f64,
    }
}

fn pace_value(cascade: &CascadeRead, index: usize) -> f32 {
    match index {
        0 => cascade.interval,
        1 => cascade.delay,
        2 => cascade.tap_wait,
        3 => cascade.group_interval,
        _ => cascade.sequence_interval,
    }
}

// How many of a cascade's plate steps land on one candidate number, and how far the matched ones sit from
// it. The tolerance is a quarter of the candidate either side, never tighter than one frame: the beat is a
// wall clock reading off two door calls, the candidate is a float the UI holds, and claiming a match at a
// tighter tolerance than the frame quantisation between them would be a match invented from rounding.
fn beats_against(cascade: &CascadeRead, value_ms: f64) -> (usize, f64) {
    let slack = (value_ms * 0.25).max(20.0);
    let mut matched = 0usize;
    let mut total_delta = 0.0;

    for step_ms in cascade.beats.iter().copied().take(cascade.beats_seen) {
        let delta = (step_ms as f64 - value_ms).abs();

        if delta <= slack {
            matched += 1;
            total_delta += delta;
        }
    }

    (matched, total_delta)
}

/// Which of the numbers this cascade was handed or holds its own plate steps match, and how many of them
/// matched. `PACE_NONE` says the plates advance on a number the class does not carry, which is C54's claim
/// read per cascade instead of per run. A number the UI holds at 0 is never a candidate: it paces nothing.
/// Two candidates that hold the same number are decided by which one the matched steps sit closest to, and
/// a dead tie keeps the number the class carries first, because the probe cannot tell which of two equal
/// floats the game reached for.
fn cascade_pace(cascade: &CascadeRead) -> (usize, usize) {
    if cascade.beats_seen == 0 {
        return (PACE_NONE, 0);
    }

    let mut best = (PACE_NONE, 0usize, f64::MAX);

    for index in 0..PACE_CANDIDATES {
        let value_ms = f64::from(pace_value(cascade, index)) * f64::from(MS_PER_SECOND);

        if value_ms <= 0.0 {
            continue;
        }

        let (matched, delta) = beats_against(cascade, value_ms);

        if matched > best.1 || (matched > 0 && matched == best.1 && delta < best.2) {
            best = (index, matched, delta);
        }
    }

    (best.0, best.1)
}

/// One cascade, said once, at the moment it closes: what its pass cost entry to completion, how far apart
/// its plates landed, what the class held while it did it, and which of those numbers its own plate steps
/// land on. This is the line C54 and C62 were owed - the number the cascade waits on, per cascade - spelled
/// `Cutt probe plate cascade {index} closed {span_ms} ms from its plate list call to {closed_by}:`, followed
/// by the plate spacing, the group / advance / tap counts, the five numbers the class carries, the two legs
/// the unreachable literals own, and the verdict.
///
/// Two limits belong on this line, because a run is told to look for it. It is a `debug!`, so it exists only
/// in a `debug_mode` session (`core/log.rs:3-9` holds a played build at `LevelFilter::Info`), and
/// `close_plate_pass` prints it for the first `PROBE_DETAIL_LIMIT` cascades of a session only. The span on it
/// is the same pair of clock reads the report's span tail prints for every cascade, so a cascade past that
/// limit is not lost from the log: its span is still on the cascade line of the report, and only its pace
/// answer survives as one count on `Cutt probe plate cascade pacing:`.
fn cascade_line(cascade: &CascadeRead) -> String {
    let span_ms = cascade.close_ms - cascade.open_ms;
    let (pace, matched) = cascade_pace(cascade);
    let pace_text = if pace == PACE_NONE {
        format!("none of these numbers ({n} plate step(s) read)", n = cascade.beats_seen)
    }
    else {
        format!("{} of {:.4} s, in {matched} of {} plate step(s)", PACE_NAMES[pace], pace_value(cascade, pace), cascade.beats_seen)
    };
    let beat_text = match cascade.beat_ms() {
        Some(ms) => format!("{} plates typed mean {:.1} ms apart", cascade.plates, ms as f64),
        None => format!("{} plate start(s), no step between them", cascade.plates),
    };

    format!(
        "Cutt probe plate cascade {index} closed {span_ms} ms from its plate list call to {closed_by}: {beat_text}, {groups} group start(s), {advances} advance(s), {taps} tap screen(s); the UI held interval {interval:.4} s _delay {delay:.4} _tapWait {tap_wait:.4} _groupInterval {group_interval:.4} s _sequenceInterval {sequence_interval:.4} s; type write leg mean {write:.1} ms and wait to the next advance mean {wait:.1} ms; the plates advance on {pace_text}",
        index = cascade.index,
        closed_by = cascade.closed_by,
        groups = cascade.groups,
        advances = cascade.advances,
        taps = cascade.taps,
        interval = cascade.interval,
        delay = cascade.delay,
        tap_wait = cascade.tap_wait,
        group_interval = cascade.group_interval,
        sequence_interval = cascade.sequence_interval,
        write = mean_of(cascade.write_ms, cascade.write_runs),
        wait = mean_of(cascade.wait_ms, cascade.wait_runs)
    )
}

// The wall between a training cut-in finishing and the status panel playing out. Both training heavy runs
// put nearly all of a slow turn here: 9,740 ms in run 14 and 7,221 ms in run 15, with no door reached in
// between, while the frame clock drew the whole way through at about 175 frames a second.
//
// The pairing used to stand on one order: the story event probe's `SingleModeTrainingCutInHelper::OnEndCutIn`
// door opened the window and `SingleModeMainViewTrainingCutStatus::PlayOut` closed it, so a cut that reached
// those two doors the other way round reported nothing at all - and that is the order this client walks now.
// Run 57 (`v0.32.0-529e4f5`) played the status panel out at 20:44:22.847 and ended the cut-in at
// 20:44:24.247; run 53 did the same at 19:30:14.533 and 19:30:18.100. Both printed `cut holes: 0 holes
// mean 0.0 ms` in every 20 s report, run 57 beside `cut runs by screen: training screen(view 1101) runs 1
// wall 8898 ms`. Run 54 is the only recent run that read a hole (34 ms), because there the cut-in ended
// first at 19:34:10.661 and the panel played out 34 ms later. So the window stands on whichever end the game
// reached first and closes at the other, and which end opened it is reported: the two orders are different
// waits. A cut still reports at most one hole, because the mark is swapped out when it is read.
static CUT_HOLE_OPEN_MS: AtomicI64 = AtomicI64::new(-1);
static CUT_HOLE_OPEN_END: AtomicUsize = AtomicUsize::new(HOLE_END_CUT_IN);
static CUT_HOLE_RUNS: AtomicUsize = AtomicUsize::new(0);
static CUT_HOLE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static CUT_HOLE_WORST_MS: AtomicI64 = AtomicI64::new(0);
// How many holes each of the two orders produced, because which end came first is the reading this repair
// exists to keep: the cut-in ending first is the 7 to 8 s shape runs 14 to 26 measured, the status play out
// first is the shape runs 53 and 57 measured.
static CUT_HOLE_END_RUNS: [AtomicUsize; HOLE_END_COUNT] = [const { AtomicUsize::new(0) }; HOLE_END_COUNT];

/// Which of the two doors a hole window is standing on.
const HOLE_END_CUT_IN: usize = 0;
const HOLE_END_PLAY_OUT: usize = 1;
const HOLE_END_COUNT: usize = 2;
const HOLE_END_NAMES: [&str; HOLE_END_COUNT] = ["the training cut-in ending", "the status play out"];

// A span from a door the game reached to the point the run ended, guarded against a request that never
// happened or landed after the close. Kept apart from the callers so the guard is a test.
fn span_from(requested: i64, now: i64) -> Option<i64> {
    if requested < 0 || now < requested {
        return None;
    }

    Some(now - requested)
}

/// The two ends of a cut hole, said in the order this cut reached them.
fn hole_stretch(from_end: usize) -> (&'static str, &'static str) {
    (HOLE_END_NAMES[from_end], HOLE_END_NAMES[HOLE_END_COUNT - 1 - from_end])
}

/// The pairing of the two doors a cut hole is named for, in the order the game reached them. `open_ms` and
/// `open_end` are the window the probe is standing on (-1 when nothing is open), `this_end` and `now_ms` the
/// door that just arrived. `Some` is the hole this door closes, told from the end that opened it; `None` is
/// the window this door opens. A window already standing on the same end is a mark no door ever answered, and
/// this door replaces it rather than pairing with itself.
fn pair_hole_open(open_ms: i64, open_end: usize, this_end: usize, now_ms: i64) -> Option<(i64, usize)> {
    if open_ms < 0 || open_end == this_end {
        return None;
    }

    span_from(open_ms, now_ms).map(|span_ms| (span_ms, open_end))
}

/// One gap cut into its legs at the plate passes seen inside it. A pass is its entry (`InitializePlateList`
/// or the `StartSequence` that follows it) to its completion (`OnAllTypewriteEnd`), not to the next entry:
/// run 49b read `the pass itself mean 26,634.3 ms worst 79,903 ms` out of a door bounded at
/// `MIN_PLATE_INTERVAL_SEC` 0.25 s and `training_plate_speed` 20, so that leg was the idle from one pass to
/// the next, player included, charged to a pass (ledger item 79). What is left between the first entry and
/// the last completion is named apart as the idle between passes, so the four legs are still the whole gap.
/// Every mark is read from the same clock, so a mark out of order means the pairing is wrong and the gap is
/// left whole: a leg made from bad marks is worse than a gap nobody explained.
fn gap_legs(close_ms: i64, first_entry_ms: i64, last_end_ms: i64, passed_ms: i64, open_ms: i64) -> Option<(i64, i64, i64, i64)> {
    let before = span_from(close_ms, first_entry_ms)?;
    let whole = span_from(first_entry_ms, last_end_ms)?;
    let after = span_from(last_end_ms, open_ms)?;
    let passes = passed_ms.clamp(0, whole);

    Some((before, passes, whole - passes, after))
}

/// The spacing between two stat plate events, or none when they do not both sit inside one window. An
/// event from before the window opened has no partner inside it, and the first event after the open has
/// no partner yet; either one read as a spacing would charge the idle between turns to the cut.
fn play_cadence_ms(opened_ms: i64, last_ms: i64, now_ms: i64) -> Option<i64> {
    if opened_ms < 0 || last_ms < opened_ms {
        return None;
    }

    span_from(last_ms, now_ms)
}

/// Which window a plate spacing belongs to: the open cut, or the open plate cascade, whichever already
/// held the earlier event. Run 14 measured no spacing at all with the cut as the only window while the
/// run counted 21 gauge plays and 22 plate list calls, because the cascade is built in the idle between
/// cuts as often as inside one (C55). A window that holds neither event reports -1 and the spacing is
/// dropped rather than guessed at.
fn cadence_window_open_ms(cut_open_ms: i64, pass_open_ms: i64, last_ms: i64) -> i64 {
    if cut_open_ms >= 0 && last_ms >= cut_open_ms {
        return cut_open_ms;
    }

    if pass_open_ms >= 0 && last_ms >= pass_open_ms {
        return pass_open_ms;
    }

    -1
}

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

// A run is a friendship cut when the tag cut-in player was reached during it, which is the door the game
// plays one through. Otherwise the last answer the game gave its own `IsValidTag` stands, and an answer
// it never gave stays unknown instead of being guessed at.
fn tag_kind_for_run(tag_player_seen: bool, last_answer: usize) -> usize {
    if tag_player_seen { TAG_FRIENDSHIP } else { last_answer }
}

// What one arm window did to the cuts that ran under it. Run 29 settled that a game session cannot be
// held constant, because which cut the game plays and how long the server takes are its own business,
// so item 55 is measured the other way round: every sample is filed under the window the picker last
// opened, and each window is reported on its own line with its cuts split by kind. The door side cost
// is one atomic load, and the lock below sits on the same cold doors the hole census already takes one
// on: a training cut closes a couple of times a career, not a frame.
#[derive(Clone, Default)]
struct WindowAgg {
    runs: usize,
    wall_ms: i64,
    played_ms: i64,
    tap_ms: i64,
    gaps: usize,
    gap_ms: i64,
    holes: usize,
    hole_ms: i64,
    first_ms: i64,
    last_ms: i64,
    kind_runs: [usize; TAG_COUNT],
    kind_wall_ms: [i64; TAG_COUNT],
}

static WINDOW_AGG: Lazy<Mutex<FnvHashMap<usize, WindowAgg>>> = Lazy::new(|| Mutex::new(FnvHashMap::default()));
static FLUSHED_WINDOW: AtomicUsize = AtomicUsize::new(0);

fn record_in_window(record: impl FnOnce(&mut WindowAgg)) {
    let window = settings_preset::arm_window();
    let now = elapsed_ms();
    let mut table = recover_lock(&WINDOW_AGG);
    let agg = table.entry(window).or_default();

    if agg.first_ms < 0 {
        agg.first_ms = now;
    }

    agg.last_ms = now;
    record(agg);
}

// The line one arm window gets when the picker has moved off it. `played` is the cut running before it
// asked the player for a tap and `tap` is the wait after that ask, which is the pair a comparison needs
// because one of them is the animation the levers reach and the other is a person.
fn window_line(window: usize, name: &str, agg: &WindowAgg) -> String {
    let mut kinds = String::new();

    for index in 0..TAG_COUNT {
        if agg.kind_runs[index] == 0 {
            continue;
        }

        let _ = write!(kinds, " {} {} runs {} ms", TAG_NAMES[index], agg.kind_runs[index], agg.kind_wall_ms[index]);
    }

    format!(
        "Cutt probe arm window {name} #{window} span {} s: cuts {} wall {} ms played {} ms tap {} ms, gaps {} {} ms, holes {} {} ms:{}kinds",
        (agg.last_ms - agg.first_ms).max(0) / 1000,
        agg.runs,
        agg.wall_ms,
        agg.played_ms,
        agg.tap_ms,
        agg.gaps,
        agg.gap_ms,
        agg.holes,
        agg.hole_ms,
        kinds
    )
}

// The ui clock interlock (item 106). Runs 52, 53 and 56 each stalled a training turn with
// `ui_animation_scale 20.0` and runs 54 and 55 ran the same turn whole at 1.0, with `time_scale` and the
// two cut levers held the same inside each pair. A stalled cut is the coroutine `<PlayTrainingCut>d__70`
// parked in one branch yielding `UnityEngine.WaitForFixedUpdate` about 30 times a second - 1,766 steps in
// 34 s on run 56 - while the animation it is waiting on has already finished on the multiplied tween
// clock, so the completion arrived before the wait that was going to watch for it. This gate keeps the
// lever off the tween channel for the length of one training cut, which is where the stall lives, and
// leaves it on everywhere else.
//
// The same boundary is what C57's two high speed write passes ask, because it is the measured version of
// the thing they were meant to wait for: run 16 wrote `story high speed 1 -> 2` and `training high speed
// 1 -> 2` again at 09:04:51, inside the window where this cut run sat open for 90,492 ms with its cut-in
// stepped every frame, and its own ladder is the evidence that the proxies that guard used are not this
// boundary - the stamp on `SingleModeTrainingCutInHelper::OnTimelineUpdatePost` sat flat at 788 calls from
// 09:00:49 to 09:01:29, when nothing was playing, and the view id test answers that a screen is up.
//
// The close door is not trusted on its own: in every stalled session measured so far `CleanUpCutt` is
// never called, so a hold only that door could end would leave the lever off for the rest of the session.
// The hold therefore also expires on a wall clock bound. It was a count of game ticks (900) until run 59 read it at
// `target_fps 200`: a 5 ms frame made that 4.5 s, short of the 8.2 s longest closed cut, and the hold let go in the
// middle of a cut (C66). 30 s of wall clock is past the longest closed cut run any run has measured (8,232 ms on run
// 55) and past the 28 s one career spread over 19 cuts (run 10), and short enough that a cut the game never cleaned up
// costs one bounded window instead of the rest of the day.
const CUT_GATE_HOLD_US: i64 = 30_000_000;

// The hold's own clock. `ProbeSuite::now_us` answers -1 in a played build, because the suite only starts under
// `debug_mode`, and the bound has to hold in a played build too. The first read sets the base.
static GATE_BASE: OnceLock<Instant> = OnceLock::new();

fn gate_clock_us() -> i64 {
    GATE_BASE.get_or_init(Instant::now).elapsed().as_micros() as i64
}

#[derive(Debug)]
struct CutGate {
    open: AtomicBool,
    opened_us: AtomicI64,
    holds: AtomicUsize,
    expiries: AtomicUsize,
}

impl CutGate {
    const fn new() -> Self {
        Self {
            open: AtomicBool::new(false),
            opened_us: AtomicI64::new(0),
            holds: AtomicUsize::new(0),
            expiries: AtomicUsize::new(0),
        }
    }

    /// The cut start doors call this. It answers which hold of the session this is, so the caller can stop
    /// naming them after the first few. Opening a cut re-arms the clock, so a hold that expired while the
    /// game was waiting for a tap does not shorten the one that follows it.
    fn engage(&self) -> usize {
        self.engage_at(gate_clock_us())
    }

    fn engage_at(&self, now_us: i64) -> usize {
        self.opened_us.store(now_us, atomic::Ordering::Relaxed);
        self.open.store(true, atomic::Ordering::Release);

        self.holds.fetch_add(1, atomic::Ordering::Relaxed) + 1
    }

    /// `CleanUpCutt` calls this. It ends a hold long before its bound, which is the ordinary case: run 10
    /// measured 35 closes against 19 starts.
    fn release(&self) {
        self.open.store(false, atomic::Ordering::Release);
    }

    /// One game tick. Answers how long the hold had run, in microseconds, when this tick ended it by reaching the
    /// bound, and `None` otherwise, so the caller only ever logs an expiry.
    fn tick(&self) -> Option<i64> {
        if !self.open.load(atomic::Ordering::Acquire) {
            return None;
        }

        self.hold_expired_at(gate_clock_us())
    }

    fn hold_expired_at(&self, now_us: i64) -> Option<i64> {
        if !self.open.load(atomic::Ordering::Acquire) {
            return None;
        }

        let held_us = now_us - self.opened_us.load(atomic::Ordering::Relaxed);
        if held_us < CUT_GATE_HOLD_US {
            return None;
        }

        self.open.store(false, atomic::Ordering::Release);
        self.expiries.fetch_add(1, atomic::Ordering::Relaxed);

        Some(held_us)
    }

    fn engaged(&self) -> bool {
        self.open.load(atomic::Ordering::Acquire)
    }
}

static CUT_GATE: CutGate = CutGate::new();

// The gate publishes one thing to the rest of the fork: `cut_gate_engaged()`, the answer `529e4f5`
// shipped, which `AnimationSpeed::tween_clocks_holding`, `HighSpeedSetting::apply` and both writes
// `StoryTimelineController` makes to the story high speed static all read. That answer is false both
// when a cut has closed and when nothing was ever armed to open one, and this module is the only place
// that can tell those two apart, so `arm_cut_gate` reports which of them it is *at install* - in the
// line the run reads - rather than exporting a second state for its readers to ask about per call.
//
// It is the arming and not the resolving, and it is the start doors and not just any door. `new_hook!`
// creates no hook at all when the site's id - or the bare wrapper name that id ends with - is in
// `Config::disabled_hooks`: it takes the other branch, `declare_hook_target` and a
// `[DISABLED] new_hook!:` line, and no hook is created (`hook/mod.rs:310-316`). And a gate whose only
// armed door is `CleanUpCutt` has nothing that ever calls `CutGate::engage` (`:3219`, `:3242`). Both shapes read
// closed for the whole session, and both used to be counted as armed: the count came from
// `resolve_method` answering non-zero, so a build with the gate's doors named in `disabled_hooks`
// printed `armed on 3 of 3 doors` about a gate that could never hold anything (C57).

// The three doors `arm_cut_gate` arms, spelled as the census spells its counters (lines :433, :434,
// :440) so a door named as missing in the install line is the same door a totals line counts.
const GATE_DOOR_LABELS: [&str; 3] = [
    "SingleModeMainTrainingCuttController::PlayTrainingCut(info)",
    "SingleModeMainTrainingCuttController::PlayScenarioTrainingCut(info)",
    "SingleModeMainTrainingCuttController::CleanUpCutt()",
];

// One door of the gate, in `GATE_DOOR_LABELS` order: the label the census counts it under, the id
// `new_hook!` keys its arming site on, and whether it is the door that ends a hold rather than the one
// that starts one. `disabled_hooks` is keyed on the arming site and not on the game method (C27), so the
// door carries both halves: the name a run reads and the key a config has to hold to put it down.
struct GateDoor {
    label: &'static str,
    id: &'static str,
    closes: bool,
}

// The id one gate door's arming site answers to, spelled out exactly as `new_hook!` spells it
// (`hook/mod.rs:291`): this module's path, the separator, the wrapper name. Written out at each door
// rather than built from a runtime string, so the id a key is matched against is the id the build keys
// the site on and not an approximation of it.
fn gate_doors() -> [GateDoor; 3] {
    [
        GateDoor {
            label: GATE_DOOR_LABELS[0],
            id: hook_id(concat!(module_path!(), "::", stringify!(TrainingCutt_PlayTrainingCut))),
            closes: false,
        },
        GateDoor {
            label: GATE_DOOR_LABELS[1],
            id: hook_id(concat!(module_path!(), "::", stringify!(TrainingCutt_PlayScenarioTrainingCut))),
            closes: false,
        },
        GateDoor {
            label: GATE_DOOR_LABELS[2],
            id: hook_id(concat!(module_path!(), "::", stringify!(TrainingCutt_CleanUpCutt))),
            closes: true,
        },
    ]
}

/// What the gate's three doors did at install: how many resolved, how many armed, and which of the two
/// reasons every door that did not arm falls under. `armed` and `resolved` are the two counts both install
/// lines print and `CensusCounts::from_gate` seeds the census from; they are kept apart because they are not
/// the same count and a run has to be able to tell a client that has no such method from a player who
/// switched the door off.
#[derive(Clone, Debug, Default)]
struct GateArming {
    /// Doors whose game method resolved to an address.
    resolved: usize,
    /// Doors `new_hook!` created a hook on: resolved, and no `disabled_hooks` key reaches the site. How
    /// many is not which - the two halves below are what a hold depends on, because a gate holding only
    /// the close door has one armed door and no way to open a hold (C57).
    armed: usize,
    /// A door that starts a hold armed. With none of them armed nothing ever calls `CutGate::engage`,
    /// so this module can never report a cut and every reader of `cut_gate_engaged` is reading a gate
    /// that cannot open. The install line says so; no reader has to work it out for itself.
    open_armed: bool,
    /// The door that ends a hold armed. Without it a hold ends only on its tick bound.
    close_armed: bool,
    /// Doors that resolved to nothing, in `GATE_DOOR_LABELS` order.
    unresolved: Vec<&'static str>,
    /// Doors that resolved and were put down by the player's own `disabled_hooks`: the door and the key.
    disabled: Vec<(&'static str, String)>,
}

impl GateArming {
    fn complete(&self) -> bool {
        self.armed == GATE_DOOR_LABELS.len()
    }
}

/// The `disabled_hooks` key that puts one arming site down, or `None` when the player configured none
/// that reaches it. It is `hook_key_match` - the copy of the rule the key report runs (`hook/mod.rs:171`)
/// - asked for this door's id, so the door check and the arming decision cannot drift apart.
///
/// Deliberately *not* `hook_is_disabled`, which answers the same thing and records the id on the way:
/// `new_hook!` records this site's id when it makes the decision, and a second record would count the
/// same hook twice in `report_hook_keys`, inventing hooks a bare key put down. Cold either way - three
/// scans of a player's key list while hooking, never on a call path.
fn key_putting_door_down(disabled: &FnvHashSet<String>, id: &'static str) -> Option<String> {
    disabled.iter().find(|key| hook_key_match(key, id).is_some()).cloned()
}

/// Which doors armed, decided from what the resolver answered and what the player configured. A door
/// arms only when both halves hold: its method resolved, and no key puts that site down. This is the
/// decision `arm_cut_gate` counts from and `init` prints: an address is not a hook, and a count of hooks
/// is not a reading of a cut - `open_armed` and `close_armed` are the halves the install line reports
/// and the census counts, and `armed` is only the number a run reads.
///
/// The third reason a door arms nothing is not counted here: a backend that refuses to create the hook.
/// `new_hook!` prints that itself at error level (`hook/mod.rs:302`), which survives the `Info` file
/// filter, so it is the one miss a played build cannot hide from the log.
fn gate_arming(open: usize, scenario: usize, close: usize, disabled: &FnvHashSet<String>) -> GateArming {
    let addresses = [open, scenario, close];
    let mut arming = GateArming::default();

    for (index, door) in gate_doors().iter().enumerate() {
        if addresses[index] == 0 {
            arming.unresolved.push(door.label);
            continue;
        }

        arming.resolved += 1;

        if let Some(key) = key_putting_door_down(disabled, door.id) {
            arming.disabled.push((door.label, key));
            continue;
        }

        arming.armed += 1;

        if door.closes {
            arming.close_armed = true;
        }
        else {
            arming.open_armed = true;
        }
    }

    arming
}

// The census half of the two doors the gate arms is only asked for when the probe was installed, so a
// build played without `debug_mode` pays two atomic stores per cut and writes no census lines.
static CENSUS_ON: AtomicBool = AtomicBool::new(false);

// Armed whatever `debug_mode` says, for the same reason `AnimationSpeed` owns the plate interval door: a
// bound on a lever this fork owns has to stand where the game is actually played, not only where a probe
// happens to be installed. The pair is the boundary the census already measures - `PlayTrainingCut` opens
// a run and `CleanUpCutt` closes it - and both wrappers hand their arguments and the coroutine the
// original built to the game untouched.
//
// It answers which doors armed and which did not. Before this, a door that resolved nothing armed nothing
// and said nothing: the miss was only ever printed by the resolver at debug level
// (`AnimationSpeed.rs:1692` "has no overload with the expected signature", `:1709` "is static"), which the
// file filter drops whenever `debug_mode` is off (`core/log.rs:3-9` sets `LevelFilter::Info`), and the one
// line the install printed appeared only with at least one door armed and only on the non-debug path. A
// played build could therefore hold its ui clock off, refuse its own writes, or do neither, with nothing in
// `hachimi.log` saying which of those it was. The census half of this module has always recorded its misses
// (`missing.push($label)`, `:4069`) and printed them (`:4356-4357`); the gate is the half that three other
// decisions now lean on, so it does the same.
//
// And it answers that question with the door armed, not with the target found: a door the resolver
// resolved and `disabled_hooks` put down is a miss too, and it is named with the key that did it.
fn arm_cut_gate(cutt_controller: Option<*mut Il2CppClass>, disabled: &FnvHashSet<String>) -> GateArming {
    let Some(class) = cutt_controller else {
        // No class means no door: the gate a played build is now leaning on does not exist, and that is
        // the one fact the run has to have. `init` prints every door as missing from this answer.
        return GateArming { unresolved: GATE_DOOR_LABELS.to_vec(), ..GateArming::default() };
    };

    let open = unsafe { AnimationSpeed::resolve_method(class, "PlayTrainingCut", ONE_INFO, CLASS) };

    // The scenario sibling opens a cut run the same way and is closed by the same door, so it holds the ui
    // clock the same way. Run 56's stalled cut came through `PlayTrainingCut`; this door is covered so the
    // hold does not depend on which of the two the game played through.
    let scenario = unsafe { AnimationSpeed::resolve_method(class, "PlayScenarioTrainingCut", ONE_INFO, CLASS) };

    // The close door is the miss with a cost a player pays: with no `CleanUpCutt` wrapper armed nothing
    // calls `CutGate::release` (`:3174`), so a hold only ever ends on `tick`'s `CUT_GATE_HOLD_US` bound
    // bound, and every cut in the session costs the ~30 s window in which `Pass::Hold` refuses both a raise
    // and its owed restore. Named at install, that shape is readable before the first cut instead of after
    // one.
    let close = unsafe { AnimationSpeed::resolve_method(class, "CleanUpCutt", NO_PARAMS, VOID) };

    // The decision before the arming, both over the same three ids: `gate_arming` asks the key rule
    // `new_hook!` is about to run for the site under it, so the count reports what the macro created
    // rather than what the resolver found. The macro stays the one place a hook is created - the pair and
    // the wrappers `529e4f5` shipped are untouched - and a door it puts down still prints its own
    // `[DISABLED] new_hook!: <id>` line and still gets its target declared.
    let arming = gate_arming(open, scenario, close, disabled);

    if open != 0 {
        new_hook!(open, TrainingCutt_PlayTrainingCut);
    }

    if scenario != 0 {
        new_hook!(scenario, TrainingCutt_PlayScenarioTrainingCut);
    }

    if close != 0 {
        new_hook!(close, TrainingCutt_CleanUpCutt);
    }

    // Which half of the pair armed is what a run has to be able to read, and it is not how many doors
    // did. `CutGate::open` is set only by `PlayTrainingCut` and `PlayScenarioTrainingCut`, so a gate whose
    // one armed door is `CleanUpCutt` has `armed > 0` and nothing that can ever call `engage`: the answer
    // it gives for the whole session is the one that reads "no cut is open". `cut_gate_missing_line`
    // spells that shape out instead of leaving a reader to infer it from a count.
    arming
}

// The id one census door's arming site answers to, spelled exactly as `new_hook!` spells it
// (`hook/mod.rs:291`): this module's path, the separator, the wrapper name. The census counts a door under
// the id the macro keys its own decision on, so the count answers for the sites this build arms and not for
// a name it merely resembles (C27), and the string a run reads in a `new_hook!:` line is the string the
// census matched the key against.
macro_rules! census_door_id {
    ($hook:ident) => {
        $crate::il2cpp::hook::hook_id(concat!(module_path!(), "::", stringify!($hook)))
    };
}

/// The two census doors a test can arm without the game: `init`'s `probe!` sites for the cut end async door
/// and the status play in door, each with the label the census prints for it and the id its arming site
/// answers to. The spelling has to be written in this module, because `module_path!` answers for the module
/// it is written in, and a test compares what the census matched a key against with the string a
/// `new_hook!:` line prints.
#[cfg(test)]
fn census_fixture_doors() -> [(&'static str, &'static str); 2] {
    [
        ("SingleModeMainTrainingCuttController::PlayTrainingCutEndAsync", census_door_id!(TrainingCutt_PlayTrainingCutEndAsync)),
        ("SingleModeMainTrainingCuttController::PlayInTrainingStatus", census_door_id!(TrainingCutt_PlayInTrainingStatus)),
    ]
}

/// What the census counts for the doors this module arms, in the two names the install line prints.
/// `resolved` is a door whose game method answered an address, which is a fact about the client. `armed` is a
/// door `new_hook!` created a hook on, which is a fact about this build: the key rule that macro consults
/// (`hook/mod.rs:293`) can leave a door that resolved with no hook behind it at all. `asked` is every door
/// the module tried to arm, so the two numbers read against one total. They used to be one number - the
/// census was seeded off `armed` while its body still counted what the resolver answered - which is how the
/// census read `5 doors installed` about a build whose `new_hook!` sites created 4 hooks (C57; the fixture is
/// `the_census_counts_a_door_new_hook_never_arms_as_resolved_only`).
#[derive(Clone, Debug, Default)]
struct CensusCounts {
    /// Doors this census asked an address for, gate doors and probe doors alike.
    asked: usize,
    /// Doors that answered with an address.
    resolved: usize,
    /// Doors `new_hook!` created a hook on: resolved, and no `disabled_hooks` key reaches the site.
    armed: usize,
    /// Census doors that resolved and armed nothing because the player's own key named their arming site,
    /// each with the key that did it. The census half of the install line's `armed nothing` clause.
    disabled: Vec<(&'static str, String)>,
}

impl CensusCounts {
    /// The gate's three doors are armed above the debug gate, so the census takes `arm_cut_gate`'s answer
    /// rather than resolving them a second time: the same doors, the same key rule, the same two numbers.
    fn from_gate(arming: &GateArming) -> Self {
        CensusCounts {
            asked: GATE_DOOR_LABELS.len(),
            resolved: arming.resolved,
            armed: arming.armed,
            disabled: Vec::new(),
        }
    }

    /// One census door whose method resolved, recorded after the `new_hook!` that ran for the site under it.
    /// A resolved door always counts as resolved; it counts as armed only when the rule that macro consults
    /// leaves the site alone, so the census reports the hooks the build created, not the addresses the
    /// resolver found. Cold: one scan of a player's key list per door while hooking, never on a call path.
    fn record_resolved(&mut self, label: &'static str, id: &'static str, disabled: &FnvHashSet<String>) {
        self.asked += 1;
        self.resolved += 1;

        match key_putting_door_down(disabled, id) {
            Some(key) => self.disabled.push((label, key)),
            None => self.armed += 1,
        }
    }

    /// One census door the client has no such method for, or whose class was not found: neither fact.
    fn record_miss(&mut self) {
        self.asked += 1;
    }
}

/// The census line a run reads. It prints the same two facts the install line prints - how many doors
/// resolved and how many armed - over every door this module arms, so the census and the gate line cannot
/// hand one install two different readings.
fn census_line(counts: &CensusCounts, counted_kinds: usize) -> String {
    format!(
        "Cutt probe: armed on {} of {} doors, {} of {} resolved, {} doors carry a counted kind in the totals line, cut runs measured from the cut start doors to CleanUpCutt and attributed to the view they start on",
        counts.armed, counts.asked, counts.resolved, counts.asked, counted_kinds,
    )
}

/// The census half of the `armed nothing` clause: every door this census armed nothing on although the
/// client answered for it, named with the key that put the arming site down. The doors the resolver never
/// answered for stay on their own line, because they have a different cause and a different fix.
fn census_disabled_line(counts: &CensusCounts) -> String {
    let named: Vec<String> = counts.disabled.iter().map(|(label, key)| format!("{label} (key {key:?})")).collect();

    format!("Cutt probe: armed nothing on {} resolved door(s) this census arms: disabled_hooks names these arming sites: {}", counts.disabled.len(), named.join(", "))
}

// The install line for a gate that armed every door it could ask for. Printed in both modes now; it
// used to print only on the non-debug path, which is the mode no measurement run uses. It names both
// counts, because the census line below does and a run has to be able to read the two against each other.
fn cut_gate_armed_line(arming: &GateArming) -> String {
    let doors = GATE_DOOR_LABELS.len();

    format!("Cutt gate: ui clock held at 1.0 while a training cut plays, armed on {armed} of {doors} doors, {resolved} of {doors} resolved", armed = arming.armed, resolved = arming.resolved)
}

/// The install line for a gate that armed less than it asked for, naming every door that did not arm and
/// which of the two reasons it falls under: this client has no such method, or the player's own
/// `disabled_hooks` key put the arming site down. Built apart from the il2cpp resolve so a test drives the
/// same decision `init` prints from, and it carries what each shape costs the session: the 0-of-3 shape is
/// the one that used to be invisible in every mode, the shape with no *start* door armed is the one where
/// the gate can never open a hold at all, and the shape where only the close door is missing is the one
/// where every cut in the session costs the tick bound.
///
/// `resolved` is printed beside `armed` because the two counts answer different questions - the first
/// about the client, the second about this build - and before this the line could only print the one a
/// resolver produced.
fn cut_gate_missing_line(arming: &GateArming, class_found: bool) -> String {
    let doors = GATE_DOOR_LABELS.len();
    let mut line = format!("Cutt gate: armed on {} of {doors} doors, {} of {doors} resolved", arming.armed, arming.resolved);

    if !arming.unresolved.is_empty() {
        let reason = if class_found { "no matching overload" } else { "no class" };
        line.push_str(&format!("; not installed ({reason}): {}", arming.unresolved.join(", ")));
    }

    if !arming.disabled.is_empty() {
        let named: Vec<String> = arming.disabled.iter().map(|(label, key)| format!("{label} (key {key:?})")).collect();
        line.push_str(&format!("; armed nothing: disabled_hooks names these arming sites: {}", named.join(", ")));
    }

    if !arming.open_armed {
        line.push_str("; the cut gate is inert in this build, no door that opens a hold is armed, so the ui clock hold and the high speed write guard that read it are inert too");
    }
    else if !arming.close_armed {
        line.push_str(&format!(
            "; no close door armed, a training cut hold ends only on its {} s wall bound",
            CUT_GATE_HOLD_US / 1_000_000,
        ));
    }

    line
}

/// Once per game tick from `GameSystem_Update`, where a per frame door already exists. A tick with no
/// hold open is one atomic load.
pub fn tick_cut_gate() {
    let Some(held_us) = CUT_GATE.tick() else {
        return;
    };

    if CUT_GATE.expiries.load(atomic::Ordering::Relaxed) <= PROBE_DETAIL_LIMIT {
        info!(
            "Cutt gate: the training cut never reached its close door, the ui clock hold ends after {:.2} s of wall clock",
            held_us as f64 / 1_000_000.0
        );
    }
}

/// True while a training cut is open in the game: from `PlayTrainingCut` / `PlayScenarioTrainingCut`
/// to `CleanUpCutt`, ending at `CUT_GATE_HOLD_US` of wall clock when the close door never arrives.
/// Read once per tween tick by `AnimationSpeed::tween_clocks`, and once per pass by
/// `HighSpeedSetting::apply` and by both of `StoryTimelineController`'s writes to the story high speed
/// static, which are the writes C57 measured landing inside a live cut.
///
/// This is the whole public surface of the gate, exactly as `529e4f5` shipped it: a reader that needs to
/// know whether the doors behind this answer actually armed is told at install, by `cut_gate_armed_line`
/// or `cut_gate_missing_line`, rather than being handed a second state to consult per call.
pub(crate) fn cut_gate_engaged() -> bool {
    CUT_GATE.engaged()
}

fn log_cut_gate_hold(hold: usize) {
    if hold > PROBE_DETAIL_LIMIT {
        return;
    }

    let ui = AnimationSpeed::ui_animation_scale();

    // Nothing to say about a hold that is covering a lever nobody turned on.
    if ui <= 1.0 {
        return;
    }

    info!("Cutt gate: training cut {hold} playing, ui clock held at 1.0 instead of {ui}");
}

// Called from the door run 10 measured 19 times in a career that produced 35 `CleanUpCutt` calls:
// `PlayTrainingCut` is where the game starts a training cut. The run before is closed first, so a cut that
// was never cleaned up is still measured instead of silently merged with the next one. C47 and C49 are the
// two boundaries this probe tried before it, `ResetCurrentTime` and `set_IsPlayingCutt`, and a full career
// showed that neither door is ever called on this path.
fn open_cut_run() {
    let now = elapsed_ms();

    if now < 0 {
        return;
    }

    close_cut_run();
    note_cut_open_after_command(now);

    let view = current_view_id();
    let bucket = bucket_for(view, SceneManager::is_race_scene_family());

    RUN_OPENED_MS.store(now, atomic::Ordering::Relaxed);
    RUN_VIEW_ID.store(view, atomic::Ordering::Relaxed);
    RUN_BUCKET.store(bucket as usize, atomic::Ordering::Relaxed);
    RUN_TAG_PLAYER_SEEN.store(0, atomic::Ordering::Relaxed);
    RUN_TAG_NAME.store(tag_kind_for_run(false, LAST_TAG_ANSWER.load(atomic::Ordering::Relaxed)), atomic::Ordering::Relaxed);
    RUN_PEAK_BITS.store(0, atomic::Ordering::Relaxed);

    // The idle between one cut closing and the next opening is the part of a training turn the cut
    // clocks never covered: run 11 put 138,604 ms of cuts inside 1,210,471 ms of training frames.
    let closed_ms = RUN_CLOSED_MS.swap(-1, atomic::Ordering::Relaxed);

    if let Some(gap_ms) = span_from(closed_ms, now) {
        let gaps = CUT_GAP_RUNS.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        CUT_GAP_MS_TOTAL.fetch_add(gap_ms, atomic::Ordering::Relaxed);
        CUT_GAP_BUCKET_MS[bucket as usize].fetch_add(gap_ms, atomic::Ordering::Relaxed);
        CUT_GAP_WORST_MS.fetch_max(gap_ms, atomic::Ordering::Relaxed);

        if gaps <= PROBE_DETAIL_LIMIT {
            info!("Cutt probe: cut gap {gap_ms} ms from the previous close to this open on view {view} {}", BUCKET_NAMES[bucket as usize]);
        }

        // A gap belongs to the window the cut opened in, which is where its second half was spent.
        record_in_window(|window| {
            window.gaps += 1;
            window.gap_ms += gap_ms;
        });

        record_gap_legs(closed_ms, now);
    }
}

// The gap belongs to the plate doors the probe already stands on, so splitting it costs no new hook: a
// `InitializePlateList` call between a close and the next open says the game was building param plates
// during that idle, and the time on either side of those passes is the time it was not. Each pass is
// charged its own entry to completion, which `close_plate_pass` accumulated while the gap was open.
fn record_gap_legs(close_ms: i64, open_ms: i64) {
    let first_entry = GAP_PLATE_FIRST_MS.swap(-1, atomic::Ordering::Relaxed);
    let last_end = GAP_PASS_LAST_END_MS.swap(-1, atomic::Ordering::Relaxed);
    let passed_ms = GAP_PASS_MS_TOTAL.swap(0, atomic::Ordering::Relaxed);

    match gap_legs(close_ms, first_entry, last_end, passed_ms, open_ms) {
        Some((before, passes, between, after)) => {
            GAP_LEG_BEFORE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
            GAP_LEG_BEFORE_MS_TOTAL.fetch_add(before, atomic::Ordering::Relaxed);
            GAP_LEG_BEFORE_WORST_MS.fetch_max(before, atomic::Ordering::Relaxed);
            GAP_LEG_PLATE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
            GAP_LEG_PLATE_MS_TOTAL.fetch_add(passes, atomic::Ordering::Relaxed);
            GAP_LEG_PLATE_WORST_MS.fetch_max(passes, atomic::Ordering::Relaxed);
            GAP_LEG_BETWEEN_MS_TOTAL.fetch_add(between, atomic::Ordering::Relaxed);
            GAP_LEG_BETWEEN_WORST_MS.fetch_max(between, atomic::Ordering::Relaxed);
            GAP_LEG_AFTER_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
            GAP_LEG_AFTER_MS_TOTAL.fetch_add(after, atomic::Ordering::Relaxed);
            GAP_LEG_AFTER_WORST_MS.fetch_max(after, atomic::Ordering::Relaxed);
        }
        None => {
            GAP_WITHOUT_PLATE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        }
    }
}

// The two ends of a training turn no animation lever reaches, read off doors this probe already stands
// on so it costs no new hook. A run 29 gap averages 19,820.7 ms against a 1,905.7 ms cut, and these are
// the marks that say how much of that gap is the game waiting on its own request and how much is the
// player deciding, which is the difference between a wall to route round and a wall to leave alone.
static COMMAND_SEND_RUNS: AtomicUsize = AtomicUsize::new(0);
static COMMAND_SENT_MS: AtomicI64 = AtomicI64::new(-1);
static SEND_TO_OPEN_RUNS: AtomicUsize = AtomicUsize::new(0);
static SEND_TO_OPEN_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static SEND_TO_OPEN_WORST_MS: AtomicI64 = AtomicI64::new(0);
static CLOSE_TO_SEND_RUNS: AtomicUsize = AtomicUsize::new(0);
static CLOSE_TO_SEND_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static CLOSE_TO_SEND_WORST_MS: AtomicI64 = AtomicI64::new(0);

/// The legs as plain marks. A negative mark is a door this session never crossed, and a pair that runs
/// backwards is dropped rather than counted, because a leg made from out of order marks charges the
/// wrong half of a turn.
fn command_legs(sent_ms: i64, closed_ms: i64, open_ms: i64) -> (Option<i64>, Option<i64>) {
    (span_from(sent_ms, open_ms), span_from(closed_ms, sent_ms))
}

fn note_command_sent() {
    let now = elapsed_ms();

    if now < 0 {
        return;
    }

    COMMAND_SEND_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
    COMMAND_SENT_MS.store(now, atomic::Ordering::Relaxed);

    // The idle between the previous cut closing and this command going out.
    if let Some(span_ms) = command_legs(now, RUN_CLOSED_MS.load(atomic::Ordering::Relaxed), -1).1 {
        CLOSE_TO_SEND_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        CLOSE_TO_SEND_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
        CLOSE_TO_SEND_WORST_MS.fetch_max(span_ms, atomic::Ordering::Relaxed);
    }
}

fn note_cut_open_after_command(open_ms: i64) {
    // Swapped rather than read: one command answers at most one cut open, so a send the game never
    // answered is not charged to a later turn.
    if let Some(span_ms) = command_legs(COMMAND_SENT_MS.swap(-1, atomic::Ordering::Relaxed), -1, open_ms).0 {
        SEND_TO_OPEN_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        SEND_TO_OPEN_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
        SEND_TO_OPEN_WORST_MS.fetch_max(span_ms, atomic::Ordering::Relaxed);
    }
}

// Called from `CleanUpCutt`, which run 10 measured 35 times against 19 cut starts. A close with nothing open
// is the game cleaning a cutt it already cleaned, so it is not counted as a run.
fn close_cut_run() {
    // A cut the game cleaned without ever reaching the status play out has no hole to census. Closing the
    // window here, even when this close counts as nothing, keeps a leftover window from putting this
    // probe's lock on every door call for the rest of the session.
    close_hole_census();

    // A hole is only a hole inside one cut, so a mark either end left standing is dropped here rather than
    // paired with the next cut's. The reset is above the early return below on purpose: `CleanUpCutt` runs
    // 35 times against 19 cut starts (run 10) and `open_cut_run` calls this function before every cut, so
    // a close that counts as nothing is still the point after which a leftover mark must not pair with
    // anything - a mark that survives it measures the idle between two turns as a hole inside one.
    CUT_HOLE_OPEN_MS.store(-1, atomic::Ordering::Relaxed);
    CUT_HOLE_OPEN_END.store(HOLE_END_CUT_IN, atomic::Ordering::Relaxed);

    let opened = RUN_OPENED_MS.swap(-1, atomic::Ordering::Relaxed);

    if opened < 0 {
        return;
    }

    let now = elapsed_ms();
    let peak = RUN_PEAK_BITS.swap(0, atomic::Ordering::Relaxed);
    let closed_bucket = RUN_BUCKET.load(atomic::Ordering::Relaxed);
    let closed_view = RUN_VIEW_ID.load(atomic::Ordering::Relaxed);
    let tag_kind = tag_kind_for_run(RUN_TAG_PLAYER_SEEN.load(atomic::Ordering::Relaxed) != 0, RUN_TAG_NAME.load(atomic::Ordering::Relaxed));

    let run_ms = now - opened;
    let runs = RUNS_CLOSED.fetch_add(1, atomic::Ordering::Relaxed) + 1;
    let peak_ms = peak_milliseconds(peak);
    let tap_ms = span_from(RUN_TAP_REQUESTED_MS.swap(-1, atomic::Ordering::Relaxed), now);

    RUN_WALL_MS_TOTAL.fetch_add(run_ms, atomic::Ordering::Relaxed);
    RUN_PEAK_MS_TOTAL.fetch_add(peak_ms, atomic::Ordering::Relaxed);

    // The bucket is the one captured when this run opened, so a cut-in that started in training and
    // was closed by a cut-in on another screen is still attributed to training.
    RUN_BUCKET_COUNT[closed_bucket].fetch_add(1, atomic::Ordering::Relaxed);
    RUN_BUCKET_WALL_MS[closed_bucket].fetch_add(run_ms, atomic::Ordering::Relaxed);
    RUN_BUCKET_PEAK_MS[closed_bucket].fetch_add(peak_ms, atomic::Ordering::Relaxed);
    RUN_BUCKET_VIEW[closed_bucket].store(closed_view, atomic::Ordering::Relaxed);

    RUN_TAG_COUNT[tag_kind].fetch_add(1, atomic::Ordering::Relaxed);
    RUN_TAG_WALL_MS[tag_kind].fetch_add(run_ms, atomic::Ordering::Relaxed);

    if let Some(waited) = tap_ms {
        TAP_WAIT_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        TAP_WAIT_MS_TOTAL.fetch_add(waited, atomic::Ordering::Relaxed);
        TAP_WAIT_BUCKET_RUNS[closed_bucket].fetch_add(1, atomic::Ordering::Relaxed);
        TAP_WAIT_BUCKET_MS[closed_bucket].fetch_add(waited, atomic::Ordering::Relaxed);
    }

    // The same cut, filed under the arm that was in force while it ran. `played` is `run_ms` minus the
    // tap wait, which is the leg from this run's open to the moment it asked for a tap, the same stretch
    // the `cut wall legs` line measures across the whole session.
    record_in_window(|window| {
        window.runs += 1;
        window.wall_ms += run_ms;
        window.kind_runs[tag_kind] += 1;
        window.kind_wall_ms[tag_kind] += run_ms;

        if let Some(waited) = tap_ms {
            window.tap_ms += waited;
            window.played_ms += run_ms - waited;
        }
    });

    RUN_CLOSED_MS.store(now, atomic::Ordering::Relaxed);

    // A new gap begins at this close, so the plate marks of the previous one are dropped rather than
    // carried into it.
    GAP_PLATE_FIRST_MS.store(-1, atomic::Ordering::Relaxed);
    GAP_PASS_LAST_END_MS.store(-1, atomic::Ordering::Relaxed);
    GAP_PASS_MS_TOTAL.store(0, atomic::Ordering::Relaxed);

    // The teardown is also where a cascade that never reported its typewrite end stops being open. A
    // window left open would charge the next turn's first plate event to the cascade before it.
    close_plate_pass("the cut teardown");

    // The cut's own coroutine window closes on the same door, and reports what that coroutine did inside
    // the wall clock this line just measured.
    super::CutStateProbe::note_cut_run_closed();

    if runs <= PROBE_DETAIL_LIMIT {
        let tap = match tap_ms {
            Some(waited) => format!("tap waited {waited} ms"),
            None => "no tap asked for".to_string(),
        };

        info!("Cutt probe: cut run {} closed at {now} ms in view {closed_view} {} as {}, timeline peak {} s, wall {run_ms} ms, {tap}", runs, BUCKET_NAMES[closed_bucket], TAG_NAMES[tag_kind], peak_seconds(peak));
    }
}

// Dumped static: `GetTrainingCutTimeScale/1 -> static float(float)`. This probe no longer stands on that
// address: `AnimationSpeed` arms the scaling door there (`training_cut_speed`, ledger item 77), and the fork
// does not hook one address twice - the same reason `TrainingParamChangeUI::InitializePlateList` moved out of
// this file. The census keeps its pair reading because the scaling door hands this module the value the game
// produced before it scaled it, so the peak stays the game's own number and the hit line beside it says what
// the lever made of it.
pub(crate) fn note_cut_clock(scale: f32, value: f32) {
    if START.get().is_some() {
        GET_TRAINING_CUT_TIME_SCALE.observe_peak(&[scale as f64, value as f64], value);
    }
    else {
        GET_TRAINING_CUT_TIME_SCALE.sample(value);
    }
}

type CutInSkipRuntimeFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    TrainingCuttHelper_SkipRuntime(this: *mut Il2CppObject) {
            CUT_IN_SKIP_RUNTIME.count();

        get_orig_fn!(TrainingCuttHelper_SkipRuntime, CutInSkipRuntimeFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCuttHelper_SkipRuntime, CutInSkipRuntimeFn)(this)
    }
}

type CutInGetTargetSpeedFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
def_detour! {
    TrainingCuttHelper_GetTargetSpeed(this: *mut Il2CppObject) -> f32 {
            let value = get_orig_fn!(TrainingCuttHelper_GetTargetSpeed, CutInGetTargetSpeedFn)(this);
        CUT_IN_GET_TARGET_SPEED.observe_peak(&[value as f64], value);

        value
    }
}

// Dumped static: `IsHighSpeedMode/0 -> static bool()`. Counted only, because a training screen reads
// it every frame and a bool has no value worth printing per call.
type CutInIsHighSpeedModeFn = extern "C" fn() -> bool;
def_detour! {
    TrainingCuttHelper_IsHighSpeedMode() -> bool {
            CUT_IN_IS_HIGH_SPEED_MODE.count();

        get_orig_fn!(TrainingCuttHelper_IsHighSpeedMode, CutInIsHighSpeedModeFn)()
    }
    bail {
                get_orig_fn!(TrainingCuttHelper_IsHighSpeedMode, CutInIsHighSpeedModeFn)()
    }
}

type CuttResetCurrentTimeFn = extern "C" fn(this: *mut Il2CppObject);
// Kept as a count, not as the run boundary. Run 9 showed a training cut-in never reaches it, which is why
// `cut runs` read 0 while the timeline was reached 3194 times (C47). A cut-in that does reset the timeline
// is still visible here.
def_detour! {
    CuttTimeline_ResetCurrentTime(this: *mut Il2CppObject) {
            CUTT_RESET_CURRENT_TIME.count();

        get_orig_fn!(CuttTimeline_ResetCurrentTime, CuttResetCurrentTimeFn)(this);
    }
    bail {
                get_orig_fn!(CuttTimeline_ResetCurrentTime, CuttResetCurrentTimeFn)(this)
    }
}

type CuttGetCurrentTimeFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
def_detour! {
    CuttTimeline_GetCurrentTime(this: *mut Il2CppObject) -> f32 {
            let value = get_orig_fn!(CuttTimeline_GetCurrentTime, CuttGetCurrentTimeFn)(this);
        CUTT_GET_CURRENT_TIME.sample(value);

        let bits = peak_merge(RUN_PEAK_BITS.load(atomic::Ordering::Relaxed), value);
        RUN_PEAK_BITS.fetch_max(bits, atomic::Ordering::Relaxed);

        value
    }
}

type CuttGetCurrentTimeScaleFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
def_detour! {
    CuttTimeline_GetCurrentTimeScale(this: *mut Il2CppObject) -> f32 {
            let value = get_orig_fn!(CuttTimeline_GetCurrentTimeScale, CuttGetCurrentTimeScaleFn)(this);
        CUTT_GET_CURRENT_TIME_SCALE.sample(value);

        value
    }
}

type CuttGetWaitingTimeFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
def_detour! {
    CuttTimeline_GetWaitingTime(this: *mut Il2CppObject) -> f32 {
            let value = get_orig_fn!(CuttTimeline_GetWaitingTime, CuttGetWaitingTimeFn)(this);
        CUTT_GET_WAITING_TIME.sample(value);

        value
    }
}

type CuttSetSpeedFn = extern "C" fn(this: *mut Il2CppObject, speed: f32);
def_detour! {
    CuttTimeline_SetSpeed(this: *mut Il2CppObject, speed: f32) {
            CUTT_SET_SPEED.observe_peak(&[speed as f64], speed);

        get_orig_fn!(CuttTimeline_SetSpeed, CuttSetSpeedFn)(this, speed);
    }
    bail {
                get_orig_fn!(CuttTimeline_SetSpeed, CuttSetSpeedFn)(this, speed)
    }
}

// Stall census for the cut-in controller (C65). The 2026-10-12 07:43 session stalled on the inherit screen (view 1501) with
// `CutInTimelineController::UpdateSpeed` still called every frame, the story doors silent for minutes, and no
// reading of the controller's own clock, because `get_CurrentTime` was never called on that path. This reads
// the clock itself, once a second, from the `UpdateSpeed` detour: the controller was just handed back to the
// game, so the object is live, and every read is a pure getter. A stall reads as a `current time` that stops
// moving while the update count keeps rising. It logs only while the view is in the story event bucket, so
// the training cut-ins that run through the same door stay out of it. Observe only: no write.
const STALL_SAMPLE_INTERVAL_MS: i64 = 1000;

pub(crate) fn stall_sample_due(last_ms: i64, now_ms: i64) -> bool {
    last_ms < 0 || now_ms - last_ms >= STALL_SAMPLE_INTERVAL_MS
}

// One stall clock per cut-in controller. Two controllers taking turns on every frame would restart each other's
// clock with a single slot, so its "seen" figure would read 0 ms for the whole run. Four slots is a bound chosen for
// that case, not a count taken from a logged run.
const STALL_SLOTS: usize = 4;

struct StallSlot {
    controller: AtomicUsize,
    seen_since_ms: AtomicI64,
    last_sample_ms: AtomicI64,
    last_update_ms: AtomicI64,
    updates: AtomicUsize,
}

impl StallSlot {
    const fn new() -> Self {
        Self {
            controller: AtomicUsize::new(0),
            seen_since_ms: AtomicI64::new(-1),
            last_sample_ms: AtomicI64::new(-1),
            last_update_ms: AtomicI64::new(-1),
            updates: AtomicUsize::new(0),
        }
    }

    fn claim(&self, controller: usize, now: i64) {
        self.controller.store(controller, atomic::Ordering::Relaxed);
        self.seen_since_ms.store(now, atomic::Ordering::Relaxed);
        self.last_sample_ms.store(-1, atomic::Ordering::Relaxed);
        self.last_update_ms.store(-1, atomic::Ordering::Relaxed);
        self.updates.store(0, atomic::Ordering::Relaxed);
    }
}

static STALL_SLOTS_TABLE: [StallSlot; STALL_SLOTS] = [const { StallSlot::new() }; STALL_SLOTS];

// The slot a controller's clock lives in. A controller that already has a slot keeps it. A new one takes the
// first empty slot, or else the slot updated least recently, which belongs to a controller the game has stopped
// calling.
fn stall_slot_in(table: &[StallSlot], controller: usize, now: i64) -> &StallSlot {
    if let Some(slot) = table.iter().find(|slot| slot.controller.load(atomic::Ordering::Relaxed) == controller) {
        return slot;
    }

    let victim = table
        .iter()
        .min_by_key(|slot| slot.last_update_ms.load(atomic::Ordering::Relaxed))
        .unwrap_or(&table[0]);
    victim.claim(controller, now);

    victim
}

def_method_wrapper_fn!(stall_get_current_time, STALL_GET_CURRENT_TIME_ADDR, f32, this: *mut Il2CppObject);
def_method_wrapper_fn!(stall_get_total_time, STALL_GET_TOTAL_TIME_ADDR, f32, this: *mut Il2CppObject);
def_method_wrapper_fn!(stall_get_waiting_time, STALL_GET_WAITING_TIME_ADDR, f32, this: *mut Il2CppObject);
def_method_wrapper_fn!(stall_get_time_scale, STALL_GET_TIME_SCALE_ADDR, f32, this: *mut Il2CppObject);
def_method_wrapper_fn!(stall_get_cutt_index, STALL_GET_CUTT_INDEX_ADDR, i32, this: *mut Il2CppObject);
def_method_wrapper_fn!(stall_get_is_pause, STALL_GET_IS_PAUSE_ADDR, bool, this: *mut Il2CppObject);
def_method_wrapper_fn!(stall_get_is_waiting_update, STALL_GET_IS_WAITING_UPDATE_ADDR, bool, this: *mut Il2CppObject);

// Milliseconds since a door last crossed on the suite clock, or -1 while it has not crossed since the suite armed.
fn door_age_ms(door: &CutProbe, now_us: i64) -> i64 {
    let last = door.last_us();

    if last < 0 { -1 } else { (now_us - last) / 1000 }
}

fn resolve_stall_getters(timeline: Option<*mut Il2CppClass>) {
    let Some(class) = timeline else { return };
    unsafe {
        STALL_GET_CURRENT_TIME_ADDR = AnimationSpeed::resolve_method(class, "get_CurrentTime", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4);
        STALL_GET_TOTAL_TIME_ADDR = AnimationSpeed::resolve_method(class, "GetTotalTime", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4);
        STALL_GET_WAITING_TIME_ADDR = AnimationSpeed::resolve_method(class, "get_WaitingTime", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4);
        STALL_GET_TIME_SCALE_ADDR = AnimationSpeed::resolve_method(class, "get_CurrentTimeScale", &[], Il2CppTypeEnum_IL2CPP_TYPE_R4);
        STALL_GET_CUTT_INDEX_ADDR = AnimationSpeed::resolve_method(class, "get_CuttIndex", &[], Il2CppTypeEnum_IL2CPP_TYPE_I4);
        STALL_GET_IS_PAUSE_ADDR = AnimationSpeed::resolve_method(class, "get_IsPause", &[], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN);
        STALL_GET_IS_WAITING_UPDATE_ADDR = AnimationSpeed::resolve_method(class, "get_IsWaitingUpdate", &[], Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN);
    }
}

fn stall_census(this: *mut Il2CppObject, now: i64) {
    let controller = this as usize;
    let slot = stall_slot_in(&STALL_SLOTS_TABLE, controller, now);

    slot.last_update_ms.store(now, atomic::Ordering::Relaxed);
    slot.updates.fetch_add(1, atomic::Ordering::Relaxed);

    let last = slot.last_sample_ms.load(atomic::Ordering::Relaxed);
    if !stall_sample_due(last, now) {
        return;
    }
    slot.last_sample_ms.store(now, atomic::Ordering::Relaxed);

    let view = SceneManager::requested_view_id();
    if bucket_for(view, SceneManager::is_race_scene_family()) != ViewBucket::StoryEvent {
        return;
    }

    let seen = now - slot.seen_since_ms.load(atomic::Ordering::Relaxed);
    let now_us = super::ProbeSuite::now_us();
    info!(
        "Cut-in stall census: view {view} controller {controller:#x} seen {seen} ms, {} UpdateSpeed calls, current time {:.3}, total {:.3}, waiting {:.3}, scale {:.3}, cutt index {}, paused {}, wait flag {}, pause set {} ms ago, resume {} ms ago, tap wait set {} ms ago, voice wait set {} ms ago",
        slot.updates.load(atomic::Ordering::Relaxed),
        stall_get_current_time(this),
        stall_get_total_time(this),
        stall_get_waiting_time(this),
        stall_get_time_scale(this),
        stall_get_cutt_index(this),
        stall_get_is_pause(this),
        stall_get_is_waiting_update(this),
        door_age_ms(&CUTT_PAUSE, now_us),
        door_age_ms(&CUTT_RESUME, now_us),
        door_age_ms(&CUTT_SET_WAIT_TAP, now_us),
        door_age_ms(&CUTT_SET_WAIT_VOICE_END, now_us),
    );
}


type CuttUpdateSpeedFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    CuttTimeline_UpdateSpeed(this: *mut Il2CppObject) {
            CUTT_UPDATE_SPEED.count();
        let now = elapsed_ms();
        pump_training_cut_cutt(this, now);
        stall_census(this, now);

        get_orig_fn!(CuttTimeline_UpdateSpeed, CuttUpdateSpeedFn)(this);
    }
    bail {
                get_orig_fn!(CuttTimeline_UpdateSpeed, CuttUpdateSpeedFn)(this)
    }
}

type CuttSkipRuntimeTimeFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
def_detour! {
    CuttTimeline_SkipRuntimeTime(this: *mut Il2CppObject, time: f32) {
            CUTT_SKIP_RUNTIME_TIME.observe_peak(&[time as f64], time);

        get_orig_fn!(CuttTimeline_SkipRuntimeTime, CuttSkipRuntimeTimeFn)(this, time);
    }
    bail {
                get_orig_fn!(CuttTimeline_SkipRuntimeTime, CuttSkipRuntimeTimeFn)(this, time)
    }
}

// The call side of the cut-in skip lever (`skip_training_cut`): this build asks the client's own
// cut-in timeline to skip, through the door the client is measured using. `def_method_wrapper_fn!`
// keeps it inert while the address never arrived, which is the same case the census reports as a
// missing door (A4).
def_method_wrapper_fn!(skip_cutt_time, TRAINING_CUT_SKIP_ADDR, (), this: *mut Il2CppObject, time: f32);

// Which cut run this build has already asked for, stamped by that run's open marker, and whether it is
// inside the skip door right now: the skip advances the timeline, the timeline reaches `UpdateSpeed`,
// and the lever must not answer that. The gate that used to be here was `set_IsPlayingCutt(playing)`,
// and runs 43 to 48 install that hook while recording no call to it, so a lever gated on it cannot run
// at all (item 100).
static SKIP_RUN_STAMP: AtomicI64 = AtomicI64::new(-1);
static SKIP_INSIDE_DOOR: AtomicBool = AtomicBool::new(false);
static CUTT_LEVER_SKIP_STEPS: CutProbe = CutProbe::counted("CutInTimelineController::SkipRuntime(lever calls)");

/// The lever, taken at the door the client reaches every frame of a cut-in (`UpdateSpeed`, 62 to 1276
/// calls across runs 43 to 48) on the instance that door is already handing us, so no pointer outlives
/// the call that got it. It runs only when the switch is on, when the probe has a training-screen cut
/// run open, and when this build has not already asked for that run. What it asks for is
/// `AnimationSpeed::TRAINING_CUT_SKIP_TIME_SEC`, the value the client is measured handing this door, and
/// the ask is held until `MIN_CUTT_OPEN_MS_BEFORE_SKIP` after the run opened rather than landing in the
/// frame the timeline resets.
fn pump_training_cut_cutt(this: *mut Il2CppObject, now: i64) {
    if !AnimationSpeed::skip_training_cut() || this.is_null() {
        return;
    }

    let open = RUN_OPENED_MS.load(atomic::Ordering::Relaxed);
    if open < 0 || RUN_BUCKET.load(atomic::Ordering::Relaxed) != ViewBucket::Training as usize {
        return;
    }

    let asked = SKIP_RUN_STAMP.load(atomic::Ordering::Relaxed);
    if AnimationSpeed::training_cut_skip_time(open, now, asked == open).is_some() {
        if SKIP_INSIDE_DOOR.swap(true, atomic::Ordering::AcqRel) {
            return;
        }

        SKIP_RUN_STAMP.store(open, atomic::Ordering::Relaxed);
        CUTT_LEVER_SKIP_STEPS.count();
        skip_cutt_time(this, AnimationSpeed::TRAINING_CUT_SKIP_TIME_SEC);
        SKIP_INSIDE_DOOR.store(false, atomic::Ordering::Release);
    }
}

type CuttSkipRuntimeFramesFn = extern "C" fn(this: *mut Il2CppObject, frames: i32, keep: bool);
def_detour! {
    CuttTimeline_SkipRuntimeFrames(this: *mut Il2CppObject, frames: i32, keep: bool) {
            CUTT_SKIP_RUNTIME_FRAMES.observe(&[frames as f64, bit(keep)]);

        get_orig_fn!(CuttTimeline_SkipRuntimeFrames, CuttSkipRuntimeFramesFn)(this, frames, keep);
    }
    bail {
                get_orig_fn!(CuttTimeline_SkipRuntimeFrames, CuttSkipRuntimeFramesFn)(this, frames, keep)
    }
}

type CuttSkipTimeDirectFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
def_detour! {
    CuttTimeline_SkipTimeDirect(this: *mut Il2CppObject, time: f32) {
            CUTT_SKIP_TIME_DIRECT.observe_peak(&[time as f64], time);

        get_orig_fn!(CuttTimeline_SkipTimeDirect, CuttSkipTimeDirectFn)(this, time);
    }
    bail {
                get_orig_fn!(CuttTimeline_SkipTimeDirect, CuttSkipTimeDirectFn)(this, time)
    }
}

type TimelineResumeFn = extern "C" fn(this: *mut Il2CppObject);
type TimelineSetEventFn = extern "C" fn(this: *mut Il2CppObject, data: *mut Il2CppObject);
def_detour! {
    CuttTimeline_Pause(this: *mut Il2CppObject, pause: bool) {
            CUTT_PAUSE.observe(&[bit(pause)]);

        get_orig_fn!(CuttTimeline_Pause, TimelineSetFlagFn)(this, pause);
    }
    bail {
                get_orig_fn!(CuttTimeline_Pause, TimelineSetFlagFn)(this, pause)
    }
}

def_detour! {
    CuttTimeline_Resume(this: *mut Il2CppObject) {
            CUTT_RESUME.count();

        get_orig_fn!(CuttTimeline_Resume, TimelineResumeFn)(this);
    }
    bail {
                get_orig_fn!(CuttTimeline_Resume, TimelineResumeFn)(this)
    }
}

def_detour! {
    CuttTimeline_SetWaitTap(this: *mut Il2CppObject, data: *mut Il2CppObject) {
            CUTT_SET_WAIT_TAP.count();

        get_orig_fn!(CuttTimeline_SetWaitTap, TimelineSetEventFn)(this, data);
    }
    bail {
                get_orig_fn!(CuttTimeline_SetWaitTap, TimelineSetEventFn)(this, data)
    }
}

def_detour! {
    CuttTimeline_SetWaitVoiceEnd(this: *mut Il2CppObject, data: *mut Il2CppObject) {
            CUTT_SET_WAIT_VOICE_END.count();

        get_orig_fn!(CuttTimeline_SetWaitVoiceEnd, TimelineSetEventFn)(this, data);
    }
    bail {
                get_orig_fn!(CuttTimeline_SetWaitVoiceEnd, TimelineSetEventFn)(this, data)
    }
}

def_detour! {
    CuttTimeline_SetEnableWaitingUpdate(this: *mut Il2CppObject, enable: bool) {
            CUTT_SET_ENABLE_WAITING_UPDATE.observe(&[bit(enable)]);

        get_orig_fn!(CuttTimeline_SetEnableWaitingUpdate, TimelineSetFlagFn)(this, enable);
    }
    bail {
                get_orig_fn!(CuttTimeline_SetEnableWaitingUpdate, TimelineSetFlagFn)(this, enable)
    }
}

type CutStatusSkipFn = extern "C" fn(this: *mut Il2CppObject, skip: bool);
def_detour! {
    TrainingCutStatus_Skip(this: *mut Il2CppObject, skip: bool) {
            CUT_STATUS_SKIP.observe(&[bit(skip)]);

        get_orig_fn!(TrainingCutStatus_Skip, CutStatusSkipFn)(this, skip);
    }
    bail {
                get_orig_fn!(TrainingCutStatus_Skip, CutStatusSkipFn)(this, skip)
    }
}

// `WaitTapAsync/0 -> class<System.Collections.IEnumerator>()` and `FadeOutResultFlash/0 -> void()` are
// the two ends of the cut that already have a dumped signature. The coroutine object is handed back
// untouched.
type WaitTapAsyncFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    TrainingCutt_WaitTapAsync(this: *mut Il2CppObject) -> *mut Il2CppObject {
            CUTT_WAIT_TAP_ASYNC.count();

        // Where the tap wait clock starts. Only while a run is open, so a request outside a measured cut
        // cannot be charged to one, and the last request of a cut is still the one its wait is measured from.
        // The first request also splits that cut's wall: what it played before asking is measured here, and
        // what it waited after asking is the tap wait.
        if RUN_OPENED_MS.load(atomic::Ordering::Relaxed) >= 0 {
            let now = elapsed_ms();

            if now >= 0 && RUN_TAP_REQUESTED_MS.swap(now, atomic::Ordering::Relaxed) < 0 {
                if let Some(played_ms) = span_from(RUN_OPENED_MS.load(atomic::Ordering::Relaxed), now) {
                    WALL_OPEN_TO_TAP_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
                    WALL_OPEN_TO_TAP_MS_TOTAL.fetch_add(played_ms, atomic::Ordering::Relaxed);
                    WALL_OPEN_TO_TAP_WORST_MS.fetch_max(played_ms, atomic::Ordering::Relaxed);
                }
            }
        }

        get_orig_fn!(TrainingCutt_WaitTapAsync, WaitTapAsyncFn)(this)
    }
}

type FadeOutResultFlashFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    TrainingCutt_FadeOutResultFlash(this: *mut Il2CppObject) {
            CUTT_FADE_OUT_RESULT_FLASH.count();

        get_orig_fn!(TrainingCutt_FadeOutResultFlash, FadeOutResultFlashFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_FadeOutResultFlash, FadeOutResultFlashFn)(this)
    }
}

// The plate list door is hooked by AnimationSpeed, because a duration this fork scales has to stand
// where it also works with debug_mode off (the plate scaling points of runs 12 and 13 were only reachable
// because the probe happened to be installed). The clocks stay here, so that hook reports each call it
// scales to this function. The plate UI's own two floats are read next to the interval the caller handed,
// so one line says which number the cascade waits on; the field handles only exist when this module was
// installed, so they are only asked for then. A probe that was never installed still counts the calls and
// keeps the peak, and records no wall clock because it has no clock to read.
pub(crate) fn note_plate_call(this: *mut Il2CppObject, interval: f32) {
    if START.get().is_some() {
        PLATE_INITIALIZE_LIST.observe_peak(
            &[
                interval as f64,
                plate_ui_delay(this) as f64,
                plate_ui_tap_wait(this) as f64,
                plate_ui_group_interval(this) as f64,
                plate_ui_sequence_interval(this) as f64,
            ],
            interval,
        );
    }
    else {
        PLATE_INITIALIZE_LIST.sample(interval);
    }

    record_plate_step();
    open_plate_pass(this, Some(interval));
}

// `PlayIcon/2 -> void(int, class<Gallop.TrainingParamChangeUI.ChangeParameterInfo>)`: the per plate play
// the plate class itself opens. Both parameters travel untouched; the index is recorded because a cascade
// that plays plates out of order is a different problem from a slow one.
type PlatePlayIconFn = extern "C" fn(this: *mut Il2CppObject, index: i32, info: *mut Il2CppObject);
def_detour! {
    TrainingParamChangeUI_PlayIcon(this: *mut Il2CppObject, index: i32, info: *mut Il2CppObject) {
            PLATE_PLAY_ICON.observe(&[index as f64]);
        record_play_cadence(&PLATE_PLAY_LAST_MS, &PLATE_PLAY_RUNS, &PLATE_PLAY_MS_TOTAL, &PLATE_PLAY_WORST_MS);

        get_orig_fn!(TrainingParamChangeUI_PlayIcon, PlatePlayIconFn)(this, index, info);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_PlayIcon, PlatePlayIconFn)(this, index, info)
    }
}

// `IsGroupPlay/1 -> bool(class<Gallop.TrainingParamChangeUI.ChangeParameterInfo>)`: the client's own
// answer to whether a plate belongs to a group that is type written together. The answer is the game's
// and is handed back untouched.
type PlateIsGroupPlayFn = extern "C" fn(this: *mut Il2CppObject, info: *mut Il2CppObject) -> bool;
def_detour! {
    TrainingParamChangeUI_IsGroupPlay(this: *mut Il2CppObject, info: *mut Il2CppObject) -> bool {
            PLATE_IS_GROUP_PLAY.count();

        get_orig_fn!(TrainingParamChangeUI_IsGroupPlay, PlateIsGroupPlayFn)(this, info)
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_IsGroupPlay, PlateIsGroupPlayFn)(this, info)
    }
}

// `Update/0 -> void()`: counted only, because a per frame door must not format anything. Whether the
// plate UI has one at all decides if the cascade is driven by frames or by a tween sequence.
type PlateUpdateFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    TrainingParamChangeUI_Update(this: *mut Il2CppObject) {
            PLATE_UPDATE.count();

        get_orig_fn!(TrainingParamChangeUI_Update, PlateUpdateFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_Update, PlateUpdateFn)(this)
    }
}

// The chain the plate class walks once the list exists: `StartSequence/0`, `StartGroupTypewrite/0`,
// `StartTypewrite/1 -> void(int)`, `OnNextTypewrite/0`, `OnEndTypeWrite/1 -> void(int)`,
// `OnAllTypewriteEnd/0`, `OnTapScreen/0` and `SetTapButtonOrder/1 -> void(int)`. Run 14 reached
// `IsGroupPlay` 104 times and `Update` 1,560 while `PlayIcon` was never reached at all, so the group
// path is the one this client walks and these are the doors that pace it. Every argument is handed back
// untouched. `OnTapScreen` matters for a different reason: if the cascade advances on taps, the wall
// clock between two plates is partly the player's reaction and no duration hook owns it.
type PlateVoidFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    TrainingParamChangeUI_StartSequence(this: *mut Il2CppObject) {
            PLATE_START_SEQUENCE.count();
        open_plate_pass(this, None);

        get_orig_fn!(TrainingParamChangeUI_StartSequence, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_StartSequence, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_StartGroupTypewrite(this: *mut Il2CppObject) {
            PLATE_START_GROUP_TYPEWRITE.count();
        note_cascade_group_start(this);

        get_orig_fn!(TrainingParamChangeUI_StartGroupTypewrite, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_StartGroupTypewrite, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_StartTypewrite(this: *mut Il2CppObject, index: i32) {
            PLATE_START_TYPEWRITE.observe(&[index as f64]);
        record_typewrite_cadence();
        note_cascade_typewrite_start(this);

        get_orig_fn!(TrainingParamChangeUI_StartTypewrite, PlateIntFn)(this, index);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_StartTypewrite, PlateIntFn)(this, index)
    }
}

def_detour! {
    TrainingParamChangeUI_OnNextTypewrite(this: *mut Il2CppObject) {
            PLATE_ON_NEXT_TYPEWRITE.count();
        note_cascade_advance(this);

        get_orig_fn!(TrainingParamChangeUI_OnNextTypewrite, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnNextTypewrite, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_OnEndTypeWrite(this: *mut Il2CppObject, index: i32) {
            PLATE_ON_END_TYPE_WRITE.observe(&[index as f64]);
        note_cascade_typewrite_end(this);

        get_orig_fn!(TrainingParamChangeUI_OnEndTypeWrite, PlateIntFn)(this, index);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnEndTypeWrite, PlateIntFn)(this, index)
    }
}

def_detour! {
    TrainingParamChangeUI_OnAllTypewriteEnd(this: *mut Il2CppObject) {
            PLATE_ON_ALL_TYPEWRITE_END.count();
        close_plate_pass("its typewrite end");

        get_orig_fn!(TrainingParamChangeUI_OnAllTypewriteEnd, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnAllTypewriteEnd, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_OnTapScreen(this: *mut Il2CppObject) {
            PLATE_ON_TAP_SCREEN.count();
        note_cascade_tap_screen(this);

        get_orig_fn!(TrainingParamChangeUI_OnTapScreen, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnTapScreen, PlateVoidFn)(this)
    }
}

type PlateIntFn = extern "C" fn(this: *mut Il2CppObject, value: i32);
def_detour! {
    TrainingParamChangeUI_SetTapButtonOrder(this: *mut Il2CppObject, order: i32) {
            PLATE_TAP_BUTTON_ORDER.observe(&[order as f64]);

        get_orig_fn!(TrainingParamChangeUI_SetTapButtonOrder, PlateIntFn)(this, order);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_SetTapButtonOrder, PlateIntFn)(this, order)
    }
}

// `InitializeFlash/4 -> void(generic<List<ChangeParameterInfo>>, float, bool, class<Canvas>)`: the sibling
// cascade of flash objects the same UI builds, and the second door that takes a timing from its caller.
// The float is recorded and handed back untouched this round: `TrainingParamChangeA2U.ANIMATION_TIME_HIGH_
// SPEED` is a compile time constant, so this argument is the only route to the flash pacing that a hook
// can take, and a route has to be measured before it is shortened.
type PlateFlashFn = extern "C" fn(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32, flag: bool, canvas: *mut Il2CppObject);
def_detour! {
    TrainingParamChangeUI_InitializeFlash(this: *mut Il2CppObject, list: *mut Il2CppObject, interval: f32, flag: bool, canvas: *mut Il2CppObject) {
            PLATE_INITIALIZE_FLASH.observe_peak(&[interval as f64, bit(flag)], interval);

        get_orig_fn!(TrainingParamChangeUI_InitializeFlash, PlateFlashFn)(this, list, interval, flag, canvas);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_InitializeFlash, PlateFlashFn)(this, list, interval, flag, canvas)
    }
}

// `Initialize/15`, the setup door. Its two floats are recorded in the order the dump spells them and
// handed back untouched. Read together with the plate list door, which prints `_delay`, `_tapWait`,
// `_groupInterval` and `_sequenceInterval` as the cascade is built, this says which argument the game
// stores in which field. That mapping is the difference between scaling the number the cascade waits on
// and scaling the one it only borrows, which is what run 14 could not tell (C54).
type PlateInitializeFn = extern "C" fn(
    this: *mut Il2CppObject,
    content: *mut Il2CppObject,
    list: *mut Il2CppObject,
    canvas: *mut Il2CppObject,
    action: *mut Il2CppObject,
    first: f32,
    second: f32,
    flag_first: bool,
    flag_second: bool,
    hp_gauge: *mut Il2CppObject,
    motivation_button: *mut Il2CppObject,
    order: i32,
    on_end: *mut Il2CppObject,
    flag_third: bool,
    flag_fourth: bool,
    flag_fifth: bool,
);
def_detour! {
    TrainingParamChangeUI_Initialize(
    this: *mut Il2CppObject,
    content: *mut Il2CppObject,
    list: *mut Il2CppObject,
    canvas: *mut Il2CppObject,
    action: *mut Il2CppObject,
    first: f32,
    second: f32,
    flag_first: bool,
    flag_second: bool,
    hp_gauge: *mut Il2CppObject,
    motivation_button: *mut Il2CppObject,
    order: i32,
    on_end: *mut Il2CppObject,
    flag_third: bool,
    flag_fourth: bool,
    flag_fifth: bool,
) {
            PLATE_INITIALIZE.observe_peak(&[first as f64, second as f64], first);

        get_orig_fn!(TrainingParamChangeUI_Initialize, PlateInitializeFn)(
            this, content, list, canvas, action, first, second, flag_first, flag_second, hp_gauge, motivation_button, order, on_end, flag_third, flag_fourth, flag_fifth,
        );
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_Initialize, PlateInitializeFn)(
                this, content, list, canvas, action, first, second, flag_first, flag_second, hp_gauge, motivation_button, order, on_end, flag_third, flag_fourth, flag_fifth,
            )
    }
}

// `PlayParameterChangeAsync/2 -> IEnumerator(generic<List<ChangeParameterInfo>>, float)`, on
// `Gallop.StoryViewController`: the owner coroutine that runs the stat change presentation, and the dump
// spells its float as a `delay` inside the coroutine state machine. It is the level above the plate list,
// so its value is the one that says whether the wait between two cascades belongs to the plates or to the
// controller that queues them. The pointer the coroutine object comes back on is the game's and is handed
// back untouched.
type PlayParameterChangeFn = extern "C" fn(this: *mut Il2CppObject, list: *mut Il2CppObject, delay: f32) -> *mut Il2CppObject;
def_detour! {
    StoryViewController_PlayParameterChangeAsync(this: *mut Il2CppObject, list: *mut Il2CppObject, delay: f32) -> *mut Il2CppObject {
            STORY_PLAY_PARAMETER_CHANGE.observe_peak(&[delay as f64], delay);

        get_orig_fn!(StoryViewController_PlayParameterChangeAsync, PlayParameterChangeFn)(this, list, delay)
    }
    bail {
                get_orig_fn!(StoryViewController_PlayParameterChangeAsync, PlayParameterChangeFn)(this, list, delay)
    }
}

// The wall clock between two `InitializePlateList` calls is what the interval the game passed actually
// buys. Run 11 read the float as 1.5 and 1.0 across 370 calls and never said what those seconds cost,
// which is the difference between a duration this fork can scale and a number it must not touch.
fn record_plate_step() {
    let now = elapsed_ms();

    if now < 0 {
        return;
    }

    if let Some(step_ms) = span_from(PLATE_LAST_MS.swap(now, atomic::Ordering::Relaxed), now) {
        PLATE_STEP_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        PLATE_STEP_MS_TOTAL.fetch_add(step_ms, atomic::Ordering::Relaxed);
        PLATE_STEP_WORST_MS.fetch_max(step_ms, atomic::Ordering::Relaxed);
    }

    // The same door is the entry of a pass, and it is the split point of the gap when no cut is open. A
    // plate pass during a cut belongs to that cut's wall, so it is not charged to the idle between cuts.
    // Only the entry is marked here: the completion is the cascade window's own close, which
    // `close_plate_pass` charges to the gap it ran inside.
    if RUN_OPENED_MS.load(atomic::Ordering::Relaxed) < 0 && RUN_CLOSED_MS.load(atomic::Ordering::Relaxed) >= 0 && GAP_PLATE_FIRST_MS.load(atomic::Ordering::Relaxed) < 0 {
        GAP_PLATE_FIRST_MS.store(now, atomic::Ordering::Relaxed);
    }
}

// One stat plate event. The spacing to the previous one is recorded only when both sit inside one window,
// the open cut or the open cascade. The gauge play run 13 proved the game reaches and the plate class's
// own per plate play share one recorder so the two doors are compared on one report line rather than each
// inventing a window.
fn record_play_cadence(last_ms: &AtomicI64, runs: &AtomicUsize, total_ms: &AtomicI64, worst_ms: &AtomicI64) {
    let now = elapsed_ms();

    if now < 0 {
        return;
    }

    let previous_ms = last_ms.swap(now, atomic::Ordering::Relaxed);
    let opened_ms = cadence_window_open_ms(
        RUN_OPENED_MS.load(atomic::Ordering::Relaxed),
        PLATE_PASS_OPEN_MS.load(atomic::Ordering::Relaxed),
        previous_ms,
    );

    if let Some(step_ms) = play_cadence_ms(opened_ms, previous_ms, now) {
        runs.fetch_add(1, atomic::Ordering::Relaxed);
        total_ms.fetch_add(step_ms, atomic::Ordering::Relaxed);
        worst_ms.fetch_max(step_ms, atomic::Ordering::Relaxed);
    }
}

// A cascade of plates is open from its plate list call and the sequence start that follows it, to its
// last typewrite end. It is not the same window as a cut: run 14 built most of its plates in the idle
// between cuts, which is exactly why the cut window alone measured nothing. The group start does not open
// a window of its own. Run 15 called `StartGroupTypewrite` 58 times against 17 `StartSequence` calls, so
// opening per group reported 19 windows and a 15,141 ms worst cascade for a chain whose own
// Initialize to OnAllTypewriteEnd span was between 215 ms and 930 ms.
fn open_plate_pass(this: *mut Il2CppObject, interval: Option<f32>) {
    let now = elapsed_ms();

    if PLATE_PASS_OPEN_MS.load(atomic::Ordering::Relaxed) >= 0 {
        // A second list call inside a cascade the game has not finished is the caller handing the same
        // sequence a new number. The window stays one cascade; the numbers it holds are refreshed.
        with_cascade(|cascade| {
            if let Some(value) = interval {
                cascade.interval = value;
            }

            read_cascade_numbers(cascade, this);
        });

        return;
    }

    PLATE_PASS_OPEN_MS.store(now, atomic::Ordering::Relaxed);

    let mut slot = recover_lock(&CASCADE_READ);
    let index = CASCADE_INDEX.fetch_add(1, atomic::Ordering::Relaxed) + 1;
    let mut cascade = CascadeRead {
        index,
        open_ms: now,
        close_ms: now,
        first_start_ms: -1,
        write_start_ms: -1,
        write_end_ms: -1,
        ..CascadeRead::default()
    };

    if let Some(value) = interval {
        cascade.interval = value;
    }

    read_cascade_numbers(&mut cascade, this);
    *slot = Some(cascade);
}

fn with_cascade(record: impl FnOnce(&mut CascadeRead)) {
    let mut slot = recover_lock(&CASCADE_READ);

    if let Some(cascade) = slot.as_mut() {
        record(cascade);
    }
}

// The four instance floats `Gallop.TrainingParamChangeUI` paces its group typewrite with. Read at the
// doors already armed, which is what C54 asked for: the answer is in the class, not in the argument.
fn read_cascade_numbers(cascade: &mut CascadeRead, this: *mut Il2CppObject) {
    cascade.delay = plate_ui_delay(this);
    cascade.tap_wait = plate_ui_tap_wait(this);
    cascade.group_interval = plate_ui_group_interval(this);
    cascade.sequence_interval = plate_ui_sequence_interval(this);
}

/// The pass ended. The span is entry to completion - the plate list call or the sequence start that opened
/// the window, to the typewrite end, or to the teardown that stood in for an end that never arrived - and
/// never the distance to the next call, which is the reading ledger item 79 refused to accept.
fn close_plate_pass(closed_by: &'static str) {
    let opened = PLATE_PASS_OPEN_MS.swap(-1, atomic::Ordering::Relaxed);
    PLATE_TYPEWRITE_LAST_MS.store(-1, atomic::Ordering::Relaxed);
    let now = elapsed_ms();
    let span_ms = span_from(opened, now);

    if let Some(span_ms) = span_ms {
        PLATE_PASS_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        PLATE_PASS_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
        PLATE_PASS_WORST_MS.fetch_max(span_ms, atomic::Ordering::Relaxed);

        // A pass that ran in the idle between cuts is a leg of that gap, charged its own span rather than
        // the distance to the next pass. A pass that opened before this gap began - inside a cut, or before
        // the previous cut closed - belongs to that wall instead of the gap.
        let closed_ms = RUN_CLOSED_MS.load(atomic::Ordering::Relaxed);

        if RUN_OPENED_MS.load(atomic::Ordering::Relaxed) < 0 && closed_ms >= 0 && opened >= closed_ms {
            GAP_PASS_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
            GAP_PASS_LAST_END_MS.store(now, atomic::Ordering::Relaxed);

            if GAP_PLATE_FIRST_MS.load(atomic::Ordering::Relaxed) < 0 {
                GAP_PLATE_FIRST_MS.store(opened, atomic::Ordering::Relaxed);
            }
        }
    }

    let closed = recover_lock(&CASCADE_READ).take();

    if let Some(mut cascade) = closed {
        cascade.close_ms = now;
        cascade.closed_by = closed_by;

        // The same two clock reads the total above accumulated, kept as this cascade's own number. The
        // aggregate reads the total and the max over every cascade that has closed, and a total prices one
        // cascade's span only when nothing else closed since the last reading (C56), so the spans are
        // written down separately, in close order, while the close is here to say which one it is.
        if let Some(span_ms) = span_ms {
            record_cascade_span(cascade.index, span_ms);
        }

        let (pace, _) = cascade_pace(&cascade);
        PACE_RUNS[pace].fetch_add(1, atomic::Ordering::Relaxed);

        let reported = CASCADE_REPORTED.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        if reported <= PROBE_DETAIL_LIMIT {
            debug!("{}", cascade_line(&cascade));
        }
    }
}

/// One cascade closed: its span and the number its window opened on go into the ring, oldest slot
/// overwritten. Two relaxed stores and one counter on a door that runs once per cascade, never per frame.
fn record_cascade_span(index: usize, span_ms: i64) {
    let slot = CASCADE_SPAN_WRITES.fetch_add(1, atomic::Ordering::Relaxed) % CASCADE_SPAN_RING;
    CASCADE_SPANS[slot].store(span_ms, atomic::Ordering::Relaxed);
    CASCADE_SPAN_INDEXES[slot].store(index, atomic::Ordering::Relaxed);
}

/// The ring as a reading takes it: spans and cascade numbers in slot order, plus how many cascades the ring
/// has taken in over the session. Slot order is close order only until the ring wraps; the reading that
/// prints them works out the order from the count.
#[derive(Clone, Copy, Default)]
struct CascadeSpans {
    spans: [i64; CASCADE_SPAN_RING],
    indexes: [usize; CASCADE_SPAN_RING],
    writes: usize,
}

impl CascadeSpans {
    /// The spans that closed since the reading that last named any: oldest first, each one said as
    /// `cascade number=span ms`. `printed` is how many spans an earlier report already named, so the tail is
    /// the closes that landed between two readings, and it is in close order whether or not the ring has
    /// wrapped. A run that closed more cascades than the ring holds is told which spans the line is not
    /// showing, because a replay reading this line has to know whether it read every cascade or the last few
    /// of them. Nothing new says nothing at all: run 57 printed its cascade line 127 times and only four of
    /// those readings had a close no earlier reading had named, so naming spans costs nothing on the other
    /// 123 reports.
    fn span_tail(&self, printed: usize) -> String {
        let closed = self.writes.saturating_sub(printed);

        if closed == 0 {
            return String::new();
        }

        let shown = closed.min(CASCADE_SPAN_RING);
        let mut tail = String::new();

        for offset in 0..shown {
            let slot = (self.writes - shown + offset) % CASCADE_SPAN_RING;
            let _ = write!(tail, " {}={} ms", self.indexes[slot], self.spans[slot]);
        }

        if closed > shown {
            let _ = write!(tail, " (the last {shown} of {closed} closes, {} older spans no longer held)", closed - shown);
        }

        format!(" cascades closed since the last report:{tail}")
    }
}

/// The ring out of the statics, the way a report reads it.
fn cascade_span_ring() -> CascadeSpans {
    let mut ring = CascadeSpans::default();

    for slot in 0..CASCADE_SPAN_RING {
        ring.spans[slot] = CASCADE_SPANS[slot].load(atomic::Ordering::Relaxed);
        ring.indexes[slot] = CASCADE_SPAN_INDEXES[slot].load(atomic::Ordering::Relaxed);
    }

    ring.writes = CASCADE_SPAN_WRITES.load(atomic::Ordering::Relaxed);
    ring
}

// The plate doors feed the cascade record: the numbers the UI holds are read again at each one, because a
// number that changes mid cascade is the number the cascade is waiting on. These doors run per plate, not
// per frame - `Update` and `UpdateSpeed`, which the game polls every frame, stay counted only. The plate
// list door reads the same numbers through `open_plate_pass`.
fn note_cascade_group_start(this: *mut Il2CppObject) {
    with_cascade(|cascade| {
        cascade.groups += 1;
        read_cascade_numbers(cascade, this);
    });
}

fn note_cascade_typewrite_start(this: *mut Il2CppObject) {
    let now = elapsed_ms();

    with_cascade(|cascade| {
        cascade.plates += 1;
        cascade.write_start_ms = now;

        if cascade.first_start_ms < 0 {
            cascade.first_start_ms = now;
        }
        else if let Some(step_ms) = span_from(cascade.last_start_ms, now) {
            if cascade.beats_seen < CASCADE_BEAT_LIMIT {
                cascade.beats[cascade.beats_seen] = step_ms;
            }

            cascade.beats_seen += 1;
        }

        cascade.last_start_ms = now;
        read_cascade_numbers(cascade, this);
    });
}

fn note_cascade_typewrite_end(this: *mut Il2CppObject) {
    let now = elapsed_ms();

    with_cascade(|cascade| {
        // The plate's own type write: the leg `TrainingParamChangePlate.TYPEWRITE_DURATION` prices, and the
        // only way to its size when the literal itself has no slot to read.
        if let Some(leg_ms) = span_from(cascade.write_start_ms, now) {
            cascade.write_ms += leg_ms;
            cascade.write_runs += 1;
        }

        cascade.write_start_ms = -1;
        cascade.write_end_ms = now;
        read_cascade_numbers(cascade, this);
    });
}

fn note_cascade_advance(this: *mut Il2CppObject) {
    let now = elapsed_ms();

    with_cascade(|cascade| {
        cascade.advances += 1;

        // The wait between one plate finishing and the next being started: the leg
        // `TrainingParamChangePlate.NEXT_WAIT_DURATION` prices.
        if let Some(leg_ms) = span_from(cascade.write_end_ms, now) {
            cascade.wait_ms += leg_ms;
            cascade.wait_runs += 1;
        }

        cascade.write_end_ms = -1;
        read_cascade_numbers(cascade, this);
    });
}

fn note_cascade_tap_screen(this: *mut Il2CppObject) {
    with_cascade(|cascade| {
        cascade.taps += 1;
        read_cascade_numbers(cascade, this);
    });
}

// How far apart the plates of one cascade arrive. The window is the cascade itself, so a spacing between
// two cascades is charged to the passes clock instead of to this one.
fn record_typewrite_cadence() {
    let now = elapsed_ms();

    if now < 0 {
        return;
    }

    let previous_ms = PLATE_TYPEWRITE_LAST_MS.swap(now, atomic::Ordering::Relaxed);

    if let Some(step_ms) = play_cadence_ms(PLATE_PASS_OPEN_MS.load(atomic::Ordering::Relaxed), previous_ms, now) {
        PLATE_TYPEWRITE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        PLATE_TYPEWRITE_MS_TOTAL.fetch_add(step_ms, atomic::Ordering::Relaxed);
        PLATE_TYPEWRITE_WORST_MS.fetch_max(step_ms, atomic::Ordering::Relaxed);
    }
}

// Marks one end of the wall a training cut spends between its cut-in finishing and its status panel
// playing out. The door for the cut-in end lives in `StoryEventProbe` because that is where the cut-in
// helper doors are; the door for the play out is one this probe owns. Either of them opens the window and
// the other closes it, because run 57 and run 53 reached them the other way round from the runs this wall
// was first named in, and a clock that only counts one order reports nothing at all about the other.
fn pair_hole_end(end: usize, now_ms: i64) {
    if now_ms < 0 {
        return;
    }

    // Swapped rather than read: one window answers at most one far door.
    let open_ms = CUT_HOLE_OPEN_MS.swap(-1, atomic::Ordering::Relaxed);
    let open_end = CUT_HOLE_OPEN_END.swap(end, atomic::Ordering::Relaxed);

    if let Some((span_ms, from_end)) = pair_hole_open(open_ms, open_end, end, now_ms) {
        record_cut_hole(span_ms, from_end);

        return;
    }

    // A second door on the same end is a window no far door ever answered. Its tally is dropped rather than
    // carried into the new window.
    close_hole_census();
    CUT_HOLE_OPEN_MS.store(now_ms, atomic::Ordering::Relaxed);
    CUT_HOLE_OPEN_END.store(end, atomic::Ordering::Relaxed);
    open_hole_census();
    super::CutStateProbe::note_hole_open();
}

pub(crate) fn note_cut_in_end() {
    pair_hole_end(HOLE_END_CUT_IN, elapsed_ms());
}

// The stretch a hole closed, filed under the arm that was in force, with the doors it crossed and what the
// cut's own coroutine did inside it.
fn record_cut_hole(span_ms: i64, from_end: usize) {
    CUT_HOLE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
    CUT_HOLE_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
    CUT_HOLE_WORST_MS.fetch_max(span_ms, atomic::Ordering::Relaxed);
    CUT_HOLE_END_RUNS[from_end].fetch_add(1, atomic::Ordering::Relaxed);

    record_in_window(|window| {
        window.holes += 1;
        window.hole_ms += span_ms;
    });

    let (from, to) = hole_stretch(from_end);

    // Which doors the wait crossed, said once, at the moment the wait ends.
    if let Some(table) = close_hole_census() {
        info!("{}", hole_census_line(span_ms, from, to, &table));
    }

    // The same stretch read off the cut's own coroutine: whether the engine kept calling it while the
    // status panel was held off decides whether this is a wait the fork can reach at all.
    super::CutStateProbe::note_hole_closed(span_ms, from, to);
}

// The gauge and plate animation doors run 11 never opened. Every argument is handed to the original
// untouched. `SetProgressbarBlendTime` records its float because it is a duration, and a scaling hook
// only belongs on it once a run shows how often the game reaches it.
type HpGaugePlayInFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    HpGauge_PlayIn(this: *mut Il2CppObject) {
            HP_GAUGE_PLAY_IN.count();
        record_play_cadence(&GAUGE_PLAY_LAST_MS, &GAUGE_PLAY_RUNS, &GAUGE_PLAY_MS_TOTAL, &GAUGE_PLAY_WORST_MS);

        get_orig_fn!(HpGauge_PlayIn, HpGaugePlayInFn)(this);
    }
    bail {
                get_orig_fn!(HpGauge_PlayIn, HpGaugePlayInFn)(this)
    }
}

type HpGaugePlayValueFn = extern "C" fn(this: *mut Il2CppObject, value: i32);
def_detour! {
    HpGauge_PlayValue(this: *mut Il2CppObject, value: i32) {
            HP_GAUGE_PLAY_VALUE.observe(&[value as f64]);

        get_orig_fn!(HpGauge_PlayValue, HpGaugePlayValueFn)(this, value);
    }
    bail {
                get_orig_fn!(HpGauge_PlayValue, HpGaugePlayValueFn)(this, value)
    }
}

type HpGaugePlayOutFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    HpGauge_PlayOut(this: *mut Il2CppObject) {
            HP_GAUGE_PLAY_OUT.count();

        get_orig_fn!(HpGauge_PlayOut, HpGaugePlayOutFn)(this);
    }
    bail {
                get_orig_fn!(HpGauge_PlayOut, HpGaugePlayOutFn)(this)
    }
}

type StatusPlayPreInFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    TrainingCutStatus_PlayPreIn(this: *mut Il2CppObject) {
            STATUS_PLAY_PRE_IN.count();

        get_orig_fn!(TrainingCutStatus_PlayPreIn, StatusPlayPreInFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutStatus_PlayPreIn, StatusPlayPreInFn)(this)
    }
}

type StatusPlayEndFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    TrainingCutStatus_PlayEnd(this: *mut Il2CppObject) {
            STATUS_PLAY_END.count();

        get_orig_fn!(TrainingCutStatus_PlayEnd, StatusPlayEndFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutStatus_PlayEnd, StatusPlayEndFn)(this)
    }
}

type PlateGetIsAutoPlayFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
def_detour! {
    PlateUI_GetIsAutoPlay(this: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(PlateUI_GetIsAutoPlay, PlateGetIsAutoPlayFn)(this);
        PLATE_GET_IS_AUTO_PLAY.observe(&[bit(value)]);

        value
    }
}

type CoroutineReturnFn = extern "C" fn(this: *mut Il2CppObject) -> *mut Il2CppObject;
type CoroutineVoidFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    SingleModeMain_CoroutineDoTweenTimeScale(this: *mut Il2CppObject) -> *mut Il2CppObject {
            MAIN_COROUTINE_DOTWEEN_SCALE.count();

        get_orig_fn!(SingleModeMain_CoroutineDoTweenTimeScale, CoroutineReturnFn)(this)
    }
    bail {
                get_orig_fn!(SingleModeMain_CoroutineDoTweenTimeScale, CoroutineReturnFn)(this)
    }
}

def_detour! {
    SingleModeMain_WaitTap(this: *mut Il2CppObject) -> *mut Il2CppObject {
            MAIN_WAIT_TAP.count();

        get_orig_fn!(SingleModeMain_WaitTap, CoroutineReturnFn)(this)
    }
    bail {
                get_orig_fn!(SingleModeMain_WaitTap, CoroutineReturnFn)(this)
    }
}

// `OnClickTraining/0 -> void()`: the click that opens a training turn. Run 15 logged 17 plate cascades
// and 5 cuts, so the turn count and the reveal count are not the same thing, and the hole clock needs to
// know which side of the click it is on.
def_detour! {
    SingleModeMain_OnClickTraining(this: *mut Il2CppObject) {
            MAIN_ON_CLICK_TRAINING.count();

        get_orig_fn!(SingleModeMain_OnClickTraining, CoroutineVoidFn)(this);
    }
    bail {
                get_orig_fn!(SingleModeMain_OnClickTraining, CoroutineVoidFn)(this)
    }
}

def_detour! {
    SingleModeMain_BackFromTraining(this: *mut Il2CppObject) -> *mut Il2CppObject {
            MAIN_BACK_FROM_TRAINING.count();

        get_orig_fn!(SingleModeMain_BackFromTraining, CoroutineReturnFn)(this)
    }
    bail {
                get_orig_fn!(SingleModeMain_BackFromTraining, CoroutineReturnFn)(this)
    }
}

type RemainTurnChangeFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
def_detour! {
    SingleModeMain_TryRemainTurnChange(this: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(SingleModeMain_TryRemainTurnChange, RemainTurnChangeFn)(this);
        MAIN_TRY_REMAIN_TURN_CHANGE.sample(flag_bit(value));

        value
    }
}

// `CommonSendCommandAsync/2 -> IEnumerator(struct<SingleModeDefine.CommandType:4B>, struct<TrainingDefine.TrainingCommandId:4B>)`:
// the turn's command going out. Two four byte structs travel in general registers (A5) and the coroutine
// object the method returns is the game's, so both arguments and the pointer pass through unchanged. The
// two integers are recorded because the dump spells them as part of the command, and a run that shows a
// command send at the start of a hole is a run that says the hole is a request the fork must not shorten.
type CommonSendCommandFn = extern "C" fn(this: *mut Il2CppObject, command: i32, id: i32) -> *mut Il2CppObject;
def_detour! {
    SingleModeMain_CommonSendCommandAsync(this: *mut Il2CppObject, command: i32, id: i32) -> *mut Il2CppObject {
            MAIN_COMMON_SEND_COMMAND_ASYNC.observe(&[command as f64, id as f64]);
        note_command_sent();

        get_orig_fn!(SingleModeMain_CommonSendCommandAsync, CommonSendCommandFn)(this, command, id)
    }
    bail {
                get_orig_fn!(SingleModeMain_CommonSendCommandAsync, CommonSendCommandFn)(this, command, id)
    }
}

// `SendCommandAsync/6 -> static IEnumerator(CommandType, TrainingCommandId, int, int, Action<SingleModeCommandResult>, Action<Cute.Http.ErrorType, int>)`,
// the static sibling the view also has. It has no hidden `this` (A3), which is why its wrapper declares
// only the six dumped arguments.
type SendCommandFn = extern "C" fn(command: i32, id: i32, first: i32, second: i32, on_result: *mut Il2CppObject, on_error: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    SingleModeMain_SendCommandAsync(command: i32, id: i32, first: i32, second: i32, on_result: *mut Il2CppObject, on_error: *mut Il2CppObject) -> *mut Il2CppObject {
            MAIN_SEND_COMMAND_ASYNC.observe(&[command as f64, id as f64, first as f64, second as f64]);
        note_command_sent();

        get_orig_fn!(SingleModeMain_SendCommandAsync, SendCommandFn)(command, id, first, second, on_result, on_error)
    }
    bail {
                get_orig_fn!(SingleModeMain_SendCommandAsync, SendCommandFn)(command, id, first, second, on_result, on_error)
    }
}

type SetIsPlayingCuttFn = extern "C" fn(this: *mut Il2CppObject, playing: bool);
// Dumped: `set_IsPlayingCutt/1 -> void(bool)` on the training cutt controller. Run 10 installed it and it
// printed nothing for a whole career (C49), so it no longer drives the run, and runs 46 to 48 install it
// while carrying no totals line for it: a lever gated on this door cannot run (item 100). It stays counted
// because a client that does use the flag would show up here, and the value is handed back exactly as it arrived.
def_detour! {
    TrainingCutt_SetIsPlayingCutt(this: *mut Il2CppObject, playing: bool) {
            CUTT_SET_IS_PLAYING_CUTT.count();

        get_orig_fn!(TrainingCutt_SetIsPlayingCutt, SetIsPlayingCuttFn)(this, playing);
    }
    bail {
                get_orig_fn!(TrainingCutt_SetIsPlayingCutt, SetIsPlayingCuttFn)(this, playing)
    }
}

type IsPlayingCuttFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
def_detour! {
    TrainingCutt_GetIsPlayingCutt(this: *mut Il2CppObject) -> bool {
            CUTT_GET_IS_PLAYING_CUTT.count();

        get_orig_fn!(TrainingCutt_GetIsPlayingCutt, IsPlayingCuttFn)(this)
    }
    bail {
                get_orig_fn!(TrainingCutt_GetIsPlayingCutt, IsPlayingCuttFn)(this)
    }
}

type CuttBoolFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
// `IsAutoPlay/0 -> bool()`: whether the cut-in is already set to play without a tap. Sampled as a peak
// of 1.0 so the totals line answers "was it ever on" without a line per read.
def_detour! {
    TrainingCutt_IsAutoPlay(this: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(TrainingCutt_IsAutoPlay, CuttBoolFn)(this);
        CUTT_IS_AUTO_PLAY.sample(flag_bit(value));

        value
    }
}

// Item 88: `StoryViewController::SkipTrainingCutt/0 -> void()`, the game's own skip for a training
// cutt. Counted only and the wrapper changes nothing: a run has to learn whether the client reaches this
// on its own before this fork is allowed to call it, and a door that never prints is an answer too.
type SkipTrainingCuttFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    StoryViewController_SkipTrainingCutt(this: *mut Il2CppObject) {
            STORY_SKIP_TRAINING_CUTT.count();

        get_orig_fn!(StoryViewController_SkipTrainingCutt, SkipTrainingCuttFn)(this);
    }
    bail {
                get_orig_fn!(StoryViewController_SkipTrainingCutt, SkipTrainingCuttFn)(this)
    }
}

// Item 89: `SingleModeUtils::GetHighSpeedPlayDuration/0 -> static float()`, the duration door that sits
// next to the scale door `AnimationSpeed` arms on the same class. A static target takes no `this`, which
// is why its wrapper declares only the return (A3).
type GetHighSpeedPlayDurationFn = extern "C" fn() -> f32;
def_detour! {
    SingleModeUtils_GetHighSpeedPlayDuration() -> f32 {
            let value = get_orig_fn!(SingleModeUtils_GetHighSpeedPlayDuration, GetHighSpeedPlayDurationFn)();
        UTILS_GET_HIGH_SPEED_PLAY_DURATION.sample(value);

        value
    }
}

// Item 90: `TimelineTrainingCuttController::Init/9`, where a caller wires that controller's clock: the
// preload, start, stop and per frame delegates, the `Action<float>` that pushes current time, and the
// `Func<bool>` that says the cut is over. Every argument is a reference handed straight back (A5).
type TimelineCuttInitFn = extern "C" fn(
    this: *mut Il2CppObject,
    preload: *mut Il2CppObject,
    request_stop: *mut Il2CppObject,
    stop: *mut Il2CppObject,
    start: *mut Il2CppObject,
    fixed_update: *mut Il2CppObject,
    update: *mut Il2CppObject,
    late_update: *mut Il2CppObject,
    set_current_time: *mut Il2CppObject,
    is_cutt_end: *mut Il2CppObject,
);
def_detour! {
    TimelineTrainingCuttController_Init(this: *mut Il2CppObject, preload: *mut Il2CppObject, request_stop: *mut Il2CppObject, stop: *mut Il2CppObject, start: *mut Il2CppObject, fixed_update: *mut Il2CppObject, update: *mut Il2CppObject, late_update: *mut Il2CppObject, set_current_time: *mut Il2CppObject, is_cutt_end: *mut Il2CppObject) {
            TIMELINE_CUTT_INIT.count();

        get_orig_fn!(TimelineTrainingCuttController_Init, TimelineCuttInitFn)(this, preload, request_stop, stop, start, fixed_update, update, late_update, set_current_time, is_cutt_end);
    }
    bail {
                get_orig_fn!(TimelineTrainingCuttController_Init, TimelineCuttInitFn)(this, preload, request_stop, stop, start, fixed_update, update, late_update, set_current_time, is_cutt_end)
    }
}

type CuttVoidFn = extern "C" fn(this: *mut Il2CppObject);
// The three drivers the cut-in runs through. Counted rather than sampled: these are the frames the
// animation actually spends, and a run's cost is the count of them.
def_detour! {
    TrainingCutt_UpdateTrainingCutIn(this: *mut Il2CppObject) {
            CUTT_UPDATE_TRAINING_CUT_IN.count();

        get_orig_fn!(TrainingCutt_UpdateTrainingCutIn, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_UpdateTrainingCutIn, CuttVoidFn)(this)
    }
}

def_detour! {
    TrainingCutt_FixedUpdateTrainingCutIn(this: *mut Il2CppObject) {
            CUTT_FIXED_UPDATE_TRAINING_CUT_IN.count();

        get_orig_fn!(TrainingCutt_FixedUpdateTrainingCutIn, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_FixedUpdateTrainingCutIn, CuttVoidFn)(this)
    }
}

def_detour! {
    TrainingCutt_LateUpdateTrainingCutIn(this: *mut Il2CppObject) {
            CUTT_LATE_UPDATE_TRAINING_CUT_IN.count();

        get_orig_fn!(TrainingCutt_LateUpdateTrainingCutIn, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_LateUpdateTrainingCutIn, CuttVoidFn)(this)
    }
}

def_detour! {
    TrainingCutt_CleanUpCutt(this: *mut Il2CppObject) {
        // The gate belongs to the door, not to the census: this wrapper is armed whether or not the probe
        // was installed, and a cut the game cleaned is a cut that no longer holds the ui clock off.
        CUT_GATE.release();

        if CENSUS_ON.load(atomic::Ordering::Relaxed) {
            CUTT_CLEAN_UP_CUTT.count();
            close_cut_run();
        }

        get_orig_fn!(TrainingCutt_CleanUpCutt, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_CleanUpCutt, CuttVoidFn)(this)
    }
}

def_detour! {
    TrainingCutt_PlayInTrainingStatus(this: *mut Il2CppObject) {
            CUTT_PLAY_IN_TRAINING_STATUS.count();

        get_orig_fn!(TrainingCutt_PlayInTrainingStatus, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_PlayInTrainingStatus, CuttVoidFn)(this)
    }
}

def_detour! {
    TrainingCutt_PlayOutTrainingStatus(this: *mut Il2CppObject) {
            CUTT_PLAY_OUT_TRAINING_STATUS.count();

        get_orig_fn!(TrainingCutt_PlayOutTrainingStatus, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutt_PlayOutTrainingStatus, CuttVoidFn)(this)
    }
}

type PlayTrainingCutFn = extern "C" fn(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject;
// Dumped: `PlayTrainingCut/1 -> IEnumerator(class<Gallop.SingleModeMainTrainingCuttController.CuttPlayInfo>)`.
// The coroutine object is created by the original and handed straight back, so nothing here changes what
// the game ends up playing.
def_detour! {
    TrainingCutt_PlayTrainingCut(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject {
        // The hold opens here, before the cut starts playing, because the animations the coroutine later
        // waits on are started by the cut itself and a tween clock that is already multiplied is what
        // finishes them out from under that wait.
        let hold = CUT_GATE.engage();
        log_cut_gate_hold(hold);

        if CENSUS_ON.load(atomic::Ordering::Relaxed) {
            CUTT_PLAY_TRAINING_CUT.count();
            open_cut_run();
        }

        // The coroutine the game just built is the state machine of the whole cut, and the values it captured
        // are the ones the game computed for this cut. They are read after the original ran, and the object is
        // handed back exactly as it came (CutStateProbe).
        let coroutine = get_orig_fn!(TrainingCutt_PlayTrainingCut, PlayTrainingCutFn)(this, info);

        if CENSUS_ON.load(atomic::Ordering::Relaxed) {
            super::CutStateProbe::note_play_training_cut(coroutine);
        }

        coroutine
    }
}

def_detour! {
    TrainingCutt_PlayScenarioTrainingCut(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject {
        let hold = CUT_GATE.engage();
        log_cut_gate_hold(hold);

        if CENSUS_ON.load(atomic::Ordering::Relaxed) {
            CUTT_PLAY_SCENARIO_TRAINING_CUT.count();
            open_cut_run();
        }

        get_orig_fn!(TrainingCutt_PlayScenarioTrainingCut, PlayTrainingCutFn)(this, info)
    }
    bail {
                get_orig_fn!(TrainingCutt_PlayScenarioTrainingCut, PlayTrainingCutFn)(this, info)
    }
}

type TrainingIdCoroutineFn = extern "C" fn(this: *mut Il2CppObject, id: i32) -> *mut Il2CppObject;
// Dumped: `TrainingAsync/1 -> IEnumerator(struct<Gallop.TrainingDefine.TrainingCommandId:4B>)`. A four
// byte struct travels in a general purpose register (A5) and this wrapper only passes it through, so it
// never has to interpret what the id means.
def_detour! {
    TrainingCutt_TrainingAsync(this: *mut Il2CppObject, id: i32) -> *mut Il2CppObject {
            CUTT_TRAINING_ASYNC.count();

        get_orig_fn!(TrainingCutt_TrainingAsync, TrainingIdCoroutineFn)(this, id)
    }
    bail {
                get_orig_fn!(TrainingCutt_TrainingAsync, TrainingIdCoroutineFn)(this, id)
    }
}

def_detour! {
    TrainingCutt_PlayTrainingSaboriAsync(this: *mut Il2CppObject, id: i32) -> *mut Il2CppObject {
            CUTT_PLAY_TRAINING_SABORI.count();
        open_cut_run();

        get_orig_fn!(TrainingCutt_PlayTrainingSaboriAsync, TrainingIdCoroutineFn)(this, id)
    }
    bail {
                get_orig_fn!(TrainingCutt_PlayTrainingSaboriAsync, TrainingIdCoroutineFn)(this, id)
    }
}

type PlayTrainingCutEndFn = extern "C" fn(this: *mut Il2CppObject, id: i32, first: bool, second: bool) -> *mut Il2CppObject;
def_detour! {
    TrainingCutt_PlayTrainingCutEndAsync(this: *mut Il2CppObject, id: i32, first: bool, second: bool) -> *mut Il2CppObject {
            CUTT_PLAY_TRAINING_CUT_END.count();

        get_orig_fn!(TrainingCutt_PlayTrainingCutEndAsync, PlayTrainingCutEndFn)(this, id, first, second)
    }
    bail {
                get_orig_fn!(TrainingCutt_PlayTrainingCutEndAsync, PlayTrainingCutEndFn)(this, id, first, second)
    }
}

type IsValidTagFn = extern "C" fn(this: *mut Il2CppObject, result: i32, cards: *mut Il2CppObject) -> bool;
// Dumped: `IsValidTag/2 -> bool(struct<TrainingResultType:4B>, generic<List<SupportCardData>>)`. This is
// the game deciding whether the cards a training produced carry a friendship, which is the only place a
// probe can tell a friendship cut-in from a regular one (A28). The answer is remembered for the cut that
// is opened next, and both arguments are handed back untouched.
def_detour! {
    TrainingCutt_IsValidTag(this: *mut Il2CppObject, result: i32, cards: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(TrainingCutt_IsValidTag, IsValidTagFn)(this, result, cards);

        LAST_TAG_ANSWER.store(if value { TAG_FRIENDSHIP } else { TAG_REGULAR }, atomic::Ordering::Relaxed);
        TAG_IS_VALID_TAG.count();

        value
    }
}

type TagCutInPlayerPlayFn = extern "C" fn(this: *mut Il2CppObject, cards: *mut Il2CppObject, done: *mut Il2CppObject);
// Dumped: `PlayCutIn/2 -> void(generic<List<SupportCardData>>, class<System.Action>)`. Both parameters are
// references the wrapper holds as addresses and passes straight back. The call is also what settles the
// kind of the cut that is open, because this is the door a friendship cut-in is played through.
def_detour! {
    TagCutInPlayer_PlayCutIn(this: *mut Il2CppObject, cards: *mut Il2CppObject, done: *mut Il2CppObject) {
            TAG_PLAYER_PLAY_CUT_IN.count();
        RUN_TAG_PLAYER_SEEN.store(1, atomic::Ordering::Relaxed);

        get_orig_fn!(TagCutInPlayer_PlayCutIn, TagCutInPlayerPlayFn)(this, cards, done);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_PlayCutIn, TagCutInPlayerPlayFn)(this, cards, done)
    }
}

type TagCutInPlayerPlayOutFn = extern "C" fn(this: *mut Il2CppObject, done: *mut Il2CppObject);
def_detour! {
    TagCutInPlayer_PlayCutInOut(this: *mut Il2CppObject, done: *mut Il2CppObject) {
            TAG_PLAYER_PLAY_CUT_OUT.count();

        get_orig_fn!(TagCutInPlayer_PlayCutInOut, TagCutInPlayerPlayOutFn)(this, done);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_PlayCutInOut, TagCutInPlayerPlayOutFn)(this, done)
    }
}

// The friendship cut-in's own effect doors (`introspect.log:23801` to `23813`). A friendship cut has never
// answered to `training_cut_speed` in any arm, and these are the doors its animation is actually played
// through: an `Animator` line effect (`_topLineAnimator`, `_bottomLineAnimator`) and three `FlashPlayer`
// effects, none of which takes a duration. They are counted so a run can say whether the wall a friendship
// cut spends is inside this class at all before a lever is put anywhere near it (item 79).
def_detour! {
    TagCutInPlayer_StopLineEffect(this: *mut Il2CppObject) {
            TAG_PLAYER_STOP_LINE_EFFECT.count();

        get_orig_fn!(TagCutInPlayer_StopLineEffect, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_StopLineEffect, CuttVoidFn)(this)
    }
}

def_detour! {
    TagCutInPlayer_DestroyButtonEffect(this: *mut Il2CppObject) {
            TAG_PLAYER_DESTROY_BUTTON_EFFECT.count();

        get_orig_fn!(TagCutInPlayer_DestroyButtonEffect, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_DestroyButtonEffect, CuttVoidFn)(this)
    }
}

def_detour! {
    TagCutInPlayer_PlaySuccessTextEffect(this: *mut Il2CppObject) {
            TAG_PLAYER_PLAY_SUCCESS_TEXT.count();

        get_orig_fn!(TagCutInPlayer_PlaySuccessTextEffect, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_PlaySuccessTextEffect, CuttVoidFn)(this)
    }
}

def_detour! {
    TagCutInPlayer_DestroyLineEffect(this: *mut Il2CppObject) {
            TAG_PLAYER_DESTROY_LINE_EFFECT.count();

        get_orig_fn!(TagCutInPlayer_DestroyLineEffect, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TagCutInPlayer_DestroyLineEffect, CuttVoidFn)(this)
    }
}

type SetResultFlashPlayerFn = extern "C" fn(this: *mut Il2CppObject, player: *mut Il2CppObject);
// `SetResultFlashPlayer/1 -> void(class<Gallop.FlashPlayer>)` (`introspect.log:23812`): the tag cut-in player is
// handed the flash player it plays its cut-in on. The address is kept so a run can tell whether it is the same
// object the support member cut-in door returned, which is what the next lever has to be written on.
def_detour! {
    TagCutInPlayer_SetResultFlashPlayer(this: *mut Il2CppObject, player: *mut Il2CppObject) {
            get_orig_fn!(TagCutInPlayer_SetResultFlashPlayer, SetResultFlashPlayerFn)(this, player);

        TAG_PLAYER_SET_RESULT_FLASH.count();
        TAG_PLAYER_RESULT_FLASH_PLAYER.store(player as usize, atomic::Ordering::Relaxed);
        note_flash_player_pair("SetResultFlashPlayer");
    }
    bail {
                get_orig_fn!(TagCutInPlayer_SetResultFlashPlayer, SetResultFlashPlayerFn)(this, player)
    }
}

// Said a few times per run: the address the last member cut-in door handed back, and the address the tag cut-in
// player was set up with. Equal says the friendship cut-in's effect runs on the object `PlayMemberCutin` made;
// different says the two effects are separate objects and the lever has to reach the tag player's own.
fn note_flash_player_pair(door: &str) {
    let seen = FLASH_PLAYER_MATCH_LOGGED.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if seen <= PROBE_DETAIL_LIMIT {
        let member = SUPPORT_MEMBER_LAST_FLASH_PLAYER.load(atomic::Ordering::Relaxed);
        let tag = TAG_PLAYER_RESULT_FLASH_PLAYER.load(atomic::Ordering::Relaxed);
        let tail = if member != 0 && member == tag { " (the same object)" } else { "" };

        info!("Cutt probe: {door} flash player pair: member cutin {member:x}, tag cut-in {tag:x}{tail}");
    }
}

// The label a flash player was told to play, read out of the managed string and cut short. Only reached on the
// bounded print lines, so a door the game plays every frame never formats anything.
unsafe fn flash_label_text(label: *mut Il2CppString) -> String {
    if label.is_null() {
        return "none".to_string();
    }

    let length = il2cpp_string_length(label).clamp(0, 32) as usize;
    let chars = il2cpp_string_chars(label);

    if chars.is_null() || length == 0 {
        return "none".to_string();
    }

    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(chars as *const u16, length) })
}

fn note_flash_label(door: &str, label: *mut Il2CppString) {
    let seen = FLASH_PLAY_LOGGED.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if seen <= PROBE_DETAIL_LIMIT {
        let text = unsafe { flash_label_text(label) };

        info!("Cutt probe: {door} call {seen} plays label {text}");
    }
}

type FlashPlayActionFn = extern "C" fn(this: *mut Il2CppObject, on_end: *mut Il2CppObject);
// `Play/1 -> void(class<System.Action>)` (`introspect-run45.log:26461`), the play with no number on it.
def_detour! {
    FlashPlayer_PlayAction(this: *mut Il2CppObject, on_end: *mut Il2CppObject) {
            get_orig_fn!(FlashPlayer_PlayAction, FlashPlayActionFn)(this, on_end);

        FLASH_PLAY_ACTION.observe(&[flash_last_motion_speed(this) as f64]);
    }
    bail {
                get_orig_fn!(FlashPlayer_PlayAction, FlashPlayActionFn)(this, on_end)
    }
}

type MotionPlayLabelFloatFn = extern "C" fn(this: *mut Il2CppObject, label: *mut Il2CppString, value: f32);
// `AnimateToUnity.AnMotion::SetMotionPlay/2 -> void(string<System.String>, float)` (`introspect-run45.log:28805`): the play
// door on the motion object the cut-in lever writes its speed onto. This is the only place other than that lever a
// float enters the effect, so the census records the float and names the label and writes nothing (C47).
def_detour! {
    AnMotion_SetMotionPlayLabelFloat(this: *mut Il2CppObject, label: *mut Il2CppString, value: f32) {
            MOTION_PLAY_LABEL_FLOAT.observe(&[value as f64]);
        note_flash_label("AnMotion::SetMotionPlay(label, float)", label);

        get_orig_fn!(AnMotion_SetMotionPlayLabelFloat, MotionPlayLabelFloatFn)(this, label, value);
    }
    bail {
                get_orig_fn!(AnMotion_SetMotionPlayLabelFloat, MotionPlayLabelFloatFn)(this, label, value)
    }
}

type MotionPlayLabelIntFn = extern "C" fn(this: *mut Il2CppObject, label: *mut Il2CppString, value: i32);
// `SetMotionPlay/2 -> void(string<System.String>, int)` (`introspect-run45.log:28806`), the overload a
// `FlashPlayer::Play(label, action, int)` lands on, with the int the flash player was handed kept as the peak.
def_detour! {
    AnMotion_SetMotionPlayLabelInt(this: *mut Il2CppObject, label: *mut Il2CppString, value: i32) {
            MOTION_PLAY_LABEL_INT.observe(&[value as f64]);
        note_flash_label("AnMotion::SetMotionPlay(label, int)", label);

        get_orig_fn!(AnMotion_SetMotionPlayLabelInt, MotionPlayLabelIntFn)(this, label, value);
    }
    bail {
                get_orig_fn!(AnMotion_SetMotionPlayLabelInt, MotionPlayLabelIntFn)(this, label, value)
    }
}

type MotionPlayFloatFn = extern "C" fn(this: *mut Il2CppObject, value: f32);
// `SetMotionPlay/1 -> void(float)` (`introspect-run45.log:28807`), the play with a number and no label.
def_detour! {
    AnMotion_SetMotionPlayFloat(this: *mut Il2CppObject, value: f32) {
            MOTION_PLAY_FLOAT.observe(&[value as f64]);

        get_orig_fn!(AnMotion_SetMotionPlayFloat, MotionPlayFloatFn)(this, value);
    }
    bail {
                get_orig_fn!(AnMotion_SetMotionPlayFloat, MotionPlayFloatFn)(this, value)
    }
}

type MotionPlayFn = extern "C" fn(this: *mut Il2CppObject);
// `SetMotionPlay/0 -> void()` (`introspect-run45.log:28803`) and `SetMotionStop/0 -> void()` (`introspect-run45.log:28816`):
// counted, because whether the motion is played at all on the object the lever writes on is the fact a run needs
// before a lever on that object can be called working rather than installed.
def_detour! {
    AnMotion_SetMotionPlay(this: *mut Il2CppObject) {
            MOTION_PLAY.count();

        get_orig_fn!(AnMotion_SetMotionPlay, MotionPlayFn)(this);
    }
    bail {
                get_orig_fn!(AnMotion_SetMotionPlay, MotionPlayFn)(this)
    }
}

type MotionStopFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    AnMotion_SetMotionStop(this: *mut Il2CppObject) {
            MOTION_STOP.count();

        get_orig_fn!(AnMotion_SetMotionStop, MotionStopFn)(this);
    }
    bail {
                get_orig_fn!(AnMotion_SetMotionStop, MotionStopFn)(this)
    }
}

type FlashPauseFn = extern "C" fn(this: *mut Il2CppObject, pause: bool);
// `Pause/1 -> void(bool)`, `Resume/0`, `Stop/0` and `End/0` (`introspect-run45.log:26465` to `26468`): whether the game
// ever parks a cut-in effect instead of letting it run out. Counted, arguments untouched.
def_detour! {
    FlashPlayer_Pause(this: *mut Il2CppObject, pause: bool) {
            get_orig_fn!(FlashPlayer_Pause, FlashPauseFn)(this, pause);

        FLASH_PAUSE.count();
    }
    bail {
                get_orig_fn!(FlashPlayer_Pause, FlashPauseFn)(this, pause)
    }
}

def_detour! {
    FlashPlayer_Resume(this: *mut Il2CppObject) {
            FLASH_RESUME.count();

        get_orig_fn!(FlashPlayer_Resume, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(FlashPlayer_Resume, CuttVoidFn)(this)
    }
}

def_detour! {
    FlashPlayer_Stop(this: *mut Il2CppObject) {
            FLASH_STOP.count();

        get_orig_fn!(FlashPlayer_Stop, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(FlashPlayer_Stop, CuttVoidFn)(this)
    }
}

def_detour! {
    FlashPlayer_End(this: *mut Il2CppObject) {
            FLASH_END.count();

        get_orig_fn!(FlashPlayer_End, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(FlashPlayer_End, CuttVoidFn)(this)
    }
}

type SetEffectTimeScaleFn = extern "C" fn(scale: f32);
// `Gallop.GraphicSettings::SetEffectTimeScale/1 -> static void(float)` with `DefaultEffectTimeScale [public
// static const float]` next to it (`introspect.log:26517` to `26520`): the one game owned time scale knob that
// names cut-in effects at all. Static, so the wrapper declares only the float (A3), and it is written back
// untouched.
def_detour! {
    GraphicSettings_SetEffectTimeScale(scale: f32) {
            GRAPHIC_SETTINGS_EFFECT_TIME_SCALE.observe(&[scale as f64]);

        get_orig_fn!(GraphicSettings_SetEffectTimeScale, SetEffectTimeScaleFn)(scale);
    }
    bail {
                get_orig_fn!(GraphicSettings_SetEffectTimeScale, SetEffectTimeScaleFn)(scale)
    }
}

fn note_flash_labels(door: &str, first: *mut Il2CppString, second: *mut Il2CppString) {
    let seen = FLASH_PLAY_LOGGED.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    if seen <= PROBE_DETAIL_LIMIT {
        let (first, second) = unsafe { (flash_label_text(first), flash_label_text(second)) };

        info!("Cutt probe: {door} call {seen} plays labels {first} then {second}");
    }
}

type FlashActionPlayWithTimeFn = extern "C" fn(this: *mut Il2CppObject, first: *mut Il2CppString, time: f32, second: *mut Il2CppString);
// `FlashActionPlayer::Play/3 -> void(string<System.String>, float, string<System.String>)` and
// `Play/2 -> void(string, string)` (`introspect-run45.log:26411` to `26412`): the class that owns a flash player has a
// float on its play door, which is where a cut-in effect's length would be named apart from its speed. The float
// and both labels go back untouched, and the float is what the census records.
def_detour! {
    FlashActionPlayer_PlayWithTime(this: *mut Il2CppObject, first: *mut Il2CppString, time: f32, second: *mut Il2CppString) {
            FLASH_ACTION_PLAY_WITH_TIME.observe(&[time as f64]);
        note_flash_labels("FlashActionPlayer::Play(label, time, label)", first, second);

        get_orig_fn!(FlashActionPlayer_PlayWithTime, FlashActionPlayWithTimeFn)(this, first, time, second);
    }
    bail {
                get_orig_fn!(FlashActionPlayer_PlayWithTime, FlashActionPlayWithTimeFn)(this, first, time, second)
    }
}

type FlashActionPlayTwoLabelsFn = extern "C" fn(this: *mut Il2CppObject, first: *mut Il2CppString, second: *mut Il2CppString);
def_detour! {
    FlashActionPlayer_PlayTwoLabels(this: *mut Il2CppObject, first: *mut Il2CppString, second: *mut Il2CppString) {
            FLASH_ACTION_PLAY_TWO_LABELS.count();
        note_flash_labels("FlashActionPlayer::Play(label, label)", first, second);

        get_orig_fn!(FlashActionPlayer_PlayTwoLabels, FlashActionPlayTwoLabelsFn)(this, first, second);
    }
    bail {
                get_orig_fn!(FlashActionPlayer_PlayTwoLabels, FlashActionPlayTwoLabelsFn)(this, first, second)
    }
}

type MemberCutinFn = extern "C" fn(ids: *mut Il2CppObject, parent: *mut Il2CppObject, on_end: *mut Il2CppObject) -> *mut Il2CppObject;
// `PlayMemberCutin/3 -> static class<Gallop.FlashPlayer>(generic<List<int>:24B>, class<Transform>, class<Action>)`
// (`introspect.log:25120`). The generic half is a pointer this wrapper never reads through (C48) and the door is
// observed only: the ids list, the parent, the callback and the returned player all go back untouched.
def_detour! {
    SupportMemberA2U_PlayMemberCutin(ids: *mut Il2CppObject, parent: *mut Il2CppObject, on_end: *mut Il2CppObject) -> *mut Il2CppObject {
            let player = get_orig_fn!(SupportMemberA2U_PlayMemberCutin, MemberCutinFn)(ids, parent, on_end);

        SUPPORT_MEMBER_PLAY_CUTIN.count();
        SUPPORT_MEMBER_LAST_FLASH_PLAYER.store(player as usize, atomic::Ordering::Relaxed);

        player
    }
}

type SoulCutinFn = extern "C" fn(index: i32, parent: *mut Il2CppObject, first: *mut Il2CppObject, second: *mut Il2CppObject) -> *mut Il2CppObject;
// `PlayAoharuSoulCutin/4 -> static class<FlashPlayer>(int, class<Transform>, class<Action>, class<Action>)`
// (`introspect.log:25121`) and the same shape next door at `25122`: four plain int and reference arguments, which
// is what makes declaring the wrapper safe (A5).
def_detour! {
    SupportMemberA2U_PlayAoharuSoulCutin(index: i32, parent: *mut Il2CppObject, first: *mut Il2CppObject, second: *mut Il2CppObject) -> *mut Il2CppObject {
            let player = get_orig_fn!(SupportMemberA2U_PlayAoharuSoulCutin, SoulCutinFn)(index, parent, first, second);

        SUPPORT_MEMBER_PLAY_SOUL_CUTIN.count();
        SUPPORT_MEMBER_LAST_FLASH_PLAYER.store(player as usize, atomic::Ordering::Relaxed);

        player
    }
}

def_detour! {
    SupportMemberA2U_PlayAoharuSpSoulCutin(index: i32, parent: *mut Il2CppObject, first: *mut Il2CppObject, second: *mut Il2CppObject) -> *mut Il2CppObject {
            let player = get_orig_fn!(SupportMemberA2U_PlayAoharuSpSoulCutin, SoulCutinFn)(index, parent, first, second);

        SUPPORT_MEMBER_PLAY_SP_SOUL_CUTIN.count();
        SUPPORT_MEMBER_LAST_FLASH_PLAYER.store(player as usize, atomic::Ordering::Relaxed);

        player
    }
}

type OutMemberCutinFn = extern "C" fn(player: *mut Il2CppObject, index: i32, on_end: *mut Il2CppObject);
// `PlayOutMemberCutin/3 -> static void(class<Gallop.FlashPlayer>, int, class<Action>)` (`introspect.log:25123`),
// the cut-out half, which takes the player back as an argument so the address it is handed is counted too.
def_detour! {
    SupportMemberA2U_PlayOutMemberCutin(player: *mut Il2CppObject, index: i32, on_end: *mut Il2CppObject) {
            get_orig_fn!(SupportMemberA2U_PlayOutMemberCutin, OutMemberCutinFn)(player, index, on_end);

        SUPPORT_MEMBER_PLAY_OUT_CUTIN.count();
        SUPPORT_MEMBER_LAST_FLASH_PLAYER.store(player as usize, atomic::Ordering::Relaxed);
    }
    bail {
                get_orig_fn!(SupportMemberA2U_PlayOutMemberCutin, OutMemberCutinFn)(player, index, on_end)
    }
}

type StaticIsValidTagFn = extern "C" fn(cards: *mut Il2CppObject) -> bool;
// Dumped: `IsValidTag/1 -> static bool(generic<List<SupportCardData>>)`. A static target has no hidden
// `this`, so this wrapper declares only the dumped argument (A3).
def_detour! {
    TagCutInPlayer_IsValidTag(cards: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(TagCutInPlayer_IsValidTag, StaticIsValidTagFn)(cards);

        LAST_TAG_ANSWER.store(if value { TAG_FRIENDSHIP } else { TAG_REGULAR }, atomic::Ordering::Relaxed);
        TAG_PLAYER_IS_VALID_TAG.count();

        value
    }
}

type GetTotalTimeFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
// The cut-in engine's own answer to how long the animation is, read from inside its detours where `this`
// is a live timeline. A length measured this way does not have to be inferred from wall clock (A29).
def_detour! {
    CuttTimeline_GetTotalTime(this: *mut Il2CppObject) answer -> f32 {
            let value = get_orig_fn!(CuttTimeline_GetTotalTime, GetTotalTimeFn)(this);
        // The length is published before the sampling: a trip in the probe half answers the cut-in
        // engine with its own length, not with a 0 that means an animation of no length.
        answer.publish(value);
        CUTT_GET_TOTAL_TIME.sample(value);

        value
    }
}

type TimelineGetSpeedFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
def_detour! {
    CuttTimeline_GetSpeed(this: *mut Il2CppObject) answer -> f32 {
            let value = get_orig_fn!(CuttTimeline_GetSpeed, TimelineGetSpeedFn)(this);
        answer.publish(value);
        CUTT_GET_SPEED.sample(value);

        value
    }
}

type TimelineIntFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
def_detour! {
    CuttTimeline_GetTotalFrameCeil(this: *mut Il2CppObject) answer -> i32 {
            let value = get_orig_fn!(CuttTimeline_GetTotalFrameCeil, TimelineIntFn)(this);
        answer.publish(value);

        if value > 0 {
            TIMELINE_TOTAL_FRAMES_PEAK.fetch_max(value as u32, atomic::Ordering::Relaxed);
        }

        value
    }
}

def_detour! {
    CuttTimeline_GetCurrentFrame(this: *mut Il2CppObject) answer -> i32 {
            let value = get_orig_fn!(CuttTimeline_GetCurrentFrame, TimelineIntFn)(this);
        answer.publish(value);

        if value > 0 {
            TIMELINE_CURRENT_FRAME_PEAK.fetch_max(value as u32, atomic::Ordering::Relaxed);
        }

        value
    }
}

def_detour! {
    CuttTimeline_GetTargetFps(this: *mut Il2CppObject) answer -> i32 {
            let value = get_orig_fn!(CuttTimeline_GetTargetFps, TimelineIntFn)(this);
        // This one is a divisor in the game's own timeline math, so 0 is not an answer that can be
        // handed back when it is the probe that tripped.
        answer.publish(value);

        if value > 0 {
            TIMELINE_TARGET_FPS.fetch_max(value as u32, atomic::Ordering::Relaxed) as i32;
        }

        value
    }
}

type SetSkipFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
// `set_SkipFrame/1 -> void(int)`: whether the game itself uses the door an auto skip option would have to
// use. Observed only, and the frame count is written back untouched (A30).
def_detour! {
    CuttTimeline_SetSkipFrame(this: *mut Il2CppObject, frames: i32) {
            CUTT_SET_SKIP_FRAME.observe(&[frames as f64]);

        get_orig_fn!(CuttTimeline_SetSkipFrame, SetSkipFrameFn)(this, frames);
    }
    bail {
                get_orig_fn!(CuttTimeline_SetSkipFrame, SetSkipFrameFn)(this, frames)
    }
}

type TimelineSetFlagFn = extern "C" fn(this: *mut Il2CppObject, playing: bool);
def_detour! {
    CuttTimeline_SetIsAutoPlay(this: *mut Il2CppObject, playing: bool) {
            CUTT_SET_IS_AUTO_PLAY.observe(&[bit(playing)]);

        get_orig_fn!(CuttTimeline_SetIsAutoPlay, TimelineSetFlagFn)(this, playing);
    }
    bail {
                get_orig_fn!(CuttTimeline_SetIsAutoPlay, TimelineSetFlagFn)(this, playing)
    }
}

def_detour! {
    CuttTimeline_GetIsAutoPlay(this: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(CuttTimeline_GetIsAutoPlay, CuttBoolFn)(this);
        CUTT_GET_IS_AUTO_PLAY.sample(flag_bit(value));

        value
    }
}

type StatusPlayOutFn = extern "C" fn(this: *mut Il2CppObject, flag: bool, action: *mut Il2CppObject);
def_detour! {
    TrainingCutStatus_PlayOut(this: *mut Il2CppObject, flag: bool, action: *mut Il2CppObject) {
            STATUS_PLAY_OUT.observe(&[bit(flag)]);
        pair_hole_end(HOLE_END_PLAY_OUT, elapsed_ms());

        get_orig_fn!(TrainingCutStatus_PlayOut, StatusPlayOutFn)(this, flag, action);
    }
    bail {
                get_orig_fn!(TrainingCutStatus_PlayOut, StatusPlayOutFn)(this, flag, action)
    }
}

type StatusPlayInFn = extern "C" fn(this: *mut Il2CppObject, interval: f32, frame: i32, flag: bool, action: *mut Il2CppObject);
// `PlayIn/4 -> void(float, int, bool, class<System.Action>)`. Run 26 read 2.4 arrive on this float and the
// panel still held 7866, 8182 and 7866 ms in the same session, so the door is counted and its value kept and
// nothing is written back: what fills the wall is the coroutine below it, not this setup call.
def_detour! {
    TrainingCutStatus_PlayIn(this: *mut Il2CppObject, interval: f32, frame: i32, flag: bool, action: *mut Il2CppObject) {
            STATUS_PLAY_IN.observe_peak(&[interval as f64, frame as f64, bit(flag)], interval);

        get_orig_fn!(TrainingCutStatus_PlayIn, StatusPlayInFn)(this, interval, frame, flag, action);
    }
    bail {
                get_orig_fn!(TrainingCutStatus_PlayIn, StatusPlayInFn)(this, interval, frame, flag, action)
    }
}

type StatusCoroutinePlayInFn = extern "C" fn(this: *mut Il2CppObject, interval: f32, frame: i32) -> *mut Il2CppObject;
// `CoroutinePlayIn/2 -> IEnumerator(float, int)`, the door run 26 named as where the panel's animation
// actually runs. The interval and the coroutine pointer go back exactly as the game made them.
def_detour! {
    TrainingCutStatus_CoroutinePlayIn(this: *mut Il2CppObject, interval: f32, frame: i32) -> *mut Il2CppObject {
            STATUS_COROUTINE_PLAY_IN.observe_peak(&[interval as f64, frame as f64], interval);

        get_orig_fn!(TrainingCutStatus_CoroutinePlayIn, StatusCoroutinePlayInFn)(this, interval, frame)
    }
    bail {
                get_orig_fn!(TrainingCutStatus_CoroutinePlayIn, StatusCoroutinePlayInFn)(this, interval, frame)
    }
}

type StatusFramePlayInFn = extern "C" fn(this: *mut Il2CppObject, first: i32, second: i32, third: i32, fourth: i32, fifth: i32, sixth: i32, time: f32, index: i32, flag: bool);
// `PlayIn/9 -> void(int, int, int, int, int, int, float, int, bool)` on the frame class: nine arguments and
// no struct among them, which is what makes a wrapper of this shape safe to declare (A5).
def_detour! {
    TrainingCutStatusFrame_PlayIn(this: *mut Il2CppObject, first: i32, second: i32, third: i32, fourth: i32, fifth: i32, sixth: i32, time: f32, index: i32, flag: bool) {
            STATUS_FRAME_PLAY_IN.observe_peak(&[time as f64, index as f64, bit(flag)], time);

        get_orig_fn!(TrainingCutStatusFrame_PlayIn, StatusFramePlayInFn)(this, first, second, third, fourth, fifth, sixth, time, index, flag);
    }
    bail {
                get_orig_fn!(TrainingCutStatusFrame_PlayIn, StatusFramePlayInFn)(this, first, second, third, fourth, fifth, sixth, time, index, flag)
    }
}

type StatusFrameRootMotionFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
// `PlayInRootMotion/1 -> void(float)`, the float the frame's own root motion is set up with.
def_detour! {
    TrainingCutStatusFrame_PlayInRootMotion(this: *mut Il2CppObject, time: f32) {
            STATUS_FRAME_PLAY_IN_ROOT_MOTION.observe_peak(&[time as f64], time);

        get_orig_fn!(TrainingCutStatusFrame_PlayInRootMotion, StatusFrameRootMotionFn)(this, time);
    }
    bail {
                get_orig_fn!(TrainingCutStatusFrame_PlayInRootMotion, StatusFrameRootMotionFn)(this, time)
    }
}

type StatusFramePlayOutFn = extern "C" fn(this: *mut Il2CppObject, flag: bool, time: f32, action: *mut Il2CppObject);
// `PlayOut/3 -> void(bool, float, class<System.Action>)`, the frame's own out motion.
def_detour! {
    TrainingCutStatusFrame_PlayOut(this: *mut Il2CppObject, flag: bool, time: f32, action: *mut Il2CppObject) {
            STATUS_FRAME_PLAY_OUT.observe_peak(&[time as f64, bit(flag)], time);

        get_orig_fn!(TrainingCutStatusFrame_PlayOut, StatusFramePlayOutFn)(this, flag, time, action);
    }
    bail {
                get_orig_fn!(TrainingCutStatusFrame_PlayOut, StatusFramePlayOutFn)(this, flag, time, action)
    }
}

def_detour! {
    TrainingCutStatusFrame_PlayPreIn(this: *mut Il2CppObject) {
            STATUS_FRAME_PRE_IN.count();

        get_orig_fn!(TrainingCutStatusFrame_PlayPreIn, CuttVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingCutStatusFrame_PlayPreIn, CuttVoidFn)(this)
    }
}

type StatusFrameSkipFn = extern "C" fn(this: *mut Il2CppObject, first: i32, second: i32, third: i32, fourth: i32, fifth: i32);
// `Skip/5 -> void(int, int, int, int, int)`: the game's own way of putting a frame straight at its end
// state, counted here because a friendship cut skip has to be asked for in this door's shape (item 79).
def_detour! {
    TrainingCutStatusFrame_Skip(this: *mut Il2CppObject, first: i32, second: i32, third: i32, fourth: i32, fifth: i32) {
            STATUS_FRAME_SKIP.observe(&[first as f64, second as f64, third as f64, fourth as f64, fifth as f64]);

        get_orig_fn!(TrainingCutStatusFrame_Skip, StatusFrameSkipFn)(this, first, second, third, fourth, fifth);
    }
    bail {
                get_orig_fn!(TrainingCutStatusFrame_Skip, StatusFrameSkipFn)(this, first, second, third, fourth, fifth)
    }
}

type StatusIntervalFn = extern "C" fn(this: *mut Il2CppObject, time: f32) -> f32;
// `GetIntervalOutBegine/1 -> float(float)`, with no setter sibling in the dump. Sampled as a peak because
// it is the gap the status panel waits before it plays out.
def_detour! {
    TrainingCutStatus_GetIntervalOutBegine(this: *mut Il2CppObject, time: f32) -> f32 {
            let value = get_orig_fn!(TrainingCutStatus_GetIntervalOutBegine, StatusIntervalFn)(this, time);
        STATUS_INTERVAL_OUT.observe_peak(&[time as f64, value as f64], value);

        value
    }
}

type StatusBoolFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
// `WillRankUpInHighSpeedMode/0 -> bool()`: the game's own decision that the status panel may rank up in
// high speed mode. Sampled as a peak of 1.0 so the totals line answers whether it ever said yes.
def_detour! {
    TrainingCutStatus_WillRankUpInHighSpeedMode(this: *mut Il2CppObject) -> bool {
            let value = get_orig_fn!(TrainingCutStatus_WillRankUpInHighSpeedMode, StatusBoolFn)(this);
        STATUS_RANK_UP_HIGH_SPEED.sample(flag_bit(value));

        value
    }
}

def_detour! {
    TrainingCutStatus_ExistPlayingFrame(this: *mut Il2CppObject) -> bool {
            STATUS_EXIST_PLAYING_FRAME.count();

        get_orig_fn!(TrainingCutStatus_ExistPlayingFrame, StatusBoolFn)(this)
    }
    bail {
                get_orig_fn!(TrainingCutStatus_ExistPlayingFrame, StatusBoolFn)(this)
    }
}

// The dump prints a class as `namespace.name`, and this client has only ever looked classes up under
// `Gallop`. The cut-in engine sits under `Gallop.CutIn.Cutt` (A27), so a label is resolved by taking
// its last segment as the class name and the rest as the namespace.
pub(crate) fn class_for_label(image: *const Il2CppImage, label: &str) -> Option<*mut Il2CppClass> {
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
    // One config read for the gate's arming: `debug_mode` says whether the census half is installed, and
    // `disabled_hooks` says which of the gate's arming sites `new_hook!` is about to skip. Cold - hooking
    // is over before the first frame, and `new_hook!` reads the same config for the same decision.
    let hachimi = Hachimi::instance();
    let config = hachimi.config.load();
    let debug_mode = config.debug_mode;

    // The gate's pair is resolved and armed before the debug gate returns: the bound it keeps sits on a
    // lever this fork owns, and a player who has no probe installed still has that lever turned on. The
    // class lookup is the one the census below reuses, so the two halves still resolve the same class the
    // scaling modules resolved.
    let cutt_controller = class_for_label(umamusume, "Gallop.SingleModeMainTrainingCuttController");
    let arming = arm_cut_gate(cutt_controller, &config.disabled_hooks);

    // The gate's arming is told in both modes, and even when nothing armed. It used to be one line that
    // required at least one door and a non-debug build, so the shape where the class resolved to nothing
    // printed nothing in any mode, and the mode the measurement runs use printed nothing at all. Every
    // miss is named here the way the census names its own (`:4069`, `:4356-4357`), in the two numbers the
    // census counts, because the ui clock hold and the two high speed write guards now decide from this gate
    // - including the miss that is not the client's fault, a door of the gate named in the player's own
    // `disabled_hooks` (C57).
    if arming.complete() {
        info!("{}", cut_gate_armed_line(&arming));
    }
    else {
        warn!("{}", cut_gate_missing_line(&arming, cutt_controller.is_some()));
    }

    if !debug_mode {
        return;
    }

    CENSUS_ON.store(true, atomic::Ordering::Release);
    let _ = START.set(Instant::now());

    let cut_in_helper = class_for_label(umamusume, "Gallop.SingleModeTrainingCutInHelper");
    let timeline = class_for_label(umamusume, "Gallop.CutIn.Cutt.CutInTimelineController");
    let cut_status = class_for_label(umamusume, "Gallop.SingleModeMainViewTrainingCutStatus");
    let cut_status_frame = class_for_label(umamusume, "Gallop.SingleModeMainViewTrainingCutStatusFrame");
    let plate_ui = class_for_label(umamusume, "Gallop.TrainingParamChangeUI");
    let hp_gauge = class_for_label(umamusume, "Gallop.SingleModeMainViewHpGauge");
    let main_view = class_for_label(umamusume, "Gallop.SingleModeMainViewController");
    let tag_player = class_for_label(umamusume, "Gallop.SingleModeMainViewTagTrainingCutInPlayer");
    let support_member = class_for_label(umamusume, "Gallop.TrainingParamChangeSupportMemberA2U");
    let flash_player = class_for_label(umamusume, "Gallop.FlashPlayer");
    let graphic_settings = class_for_label(umamusume, "Gallop.GraphicSettings");
    let flash_action_player = class_for_label(umamusume, "Gallop.FlashActionPlayer");
    // The class that owns the stat change presentation for a training turn: its coroutine
    // `PlayParameterChangeAsync/2` is the level above the plate list door.
    let story_view = class_for_label(umamusume, "Gallop.StoryViewController");
    // Item 89 and item 90: the class the training cut's scale door already lives on, which also holds the
    // duration door nothing has asked yet, and the cutt controller whose clock fields no run has read.
    let single_mode_utils = class_for_label(umamusume, "Gallop.SingleModeUtils");
    let timeline_cutt_controller = class_for_label(umamusume, "Gallop.TimelineTrainingCuttController");

    // The plate UI keeps its cascade timing in four instance floats. They are resolved next to the door
    // that reads them, and nothing here writes them: a duration hook has to know which number the
    // cascade waits on before it is allowed to shorten one. Run 13 read `_delay` at 0.16666746139526367
    // and `_tapWait` at 0.0 next to a caller handed interval of 1.0, which is how the two cascade
    // intervals got added to the read: a wall clock between two plate list calls that a 20x shorter
    // interval did not move (C54) means the number to look at is not the one the caller passes. A client
    // that has none of these fields leaves the handles null, and the accessors report a zero rather than
    // a value they never read.
    unsafe {
        if let Some(class) = plate_ui {
            PLATE_UI_DELAY_FIELD = get_field_from_name(class, c"_delay");
            PLATE_UI_TAP_WAIT_FIELD = get_field_from_name(class, c"_tapWait");
            PLATE_UI_GROUP_INTERVAL_FIELD = get_field_from_name(class, c"_groupInterval");
            PLATE_UI_SEQUENCE_INTERVAL_FIELD = get_field_from_name(class, c"_sequenceInterval");
        }
    }

    let mut missing: Vec<&str> = Vec::new();
    // The census counts the same two facts the install line just printed, over every door it arms: how many
    // resolved to an address, and how many `new_hook!` created a hook on. The gate's three doors were armed
    // above the debug gate, so they enter here as `arm_cut_gate`'s two counts instead of a second
    // resolution; every door below is counted as its own `new_hook!` runs, under the id that macro keys the
    // site on.
    let mut census = CensusCounts::from_gate(&arming);

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
                        census.record_resolved($label, census_door_id!($hook), &config.disabled_hooks);
                    }
                    else {
                        missing.push($label);
                        census.record_miss();
                    }
                },
                None => {
                    missing.push($label);
                    census.record_miss();
                },
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
                        census.record_resolved($label, census_door_id!($hook), &config.disabled_hooks);
                    }
                    else {
                        missing.push($label);
                        census.record_miss();
                    }
                },
                None => {
                    missing.push($label);
                    census.record_miss();
                },
            }
        };
    }

    // A parameter the dump spells as `generic<...>` reports GENERICINST, so it only resolves through the
    // resolver whose wrapper declares a pointer there. Nothing in this file writes through such a
    // parameter: every one of them is handed back to the original untouched (C48).
    macro_rules! generic_probe {
        ($class:expr, $hook:ident, $method:literal, $params:expr, $ret:expr, $label:literal) => {
            match $class {
                Some(class) => {
                    let addr = unsafe { AnimationSpeed::resolve_generic_ref_method(class, $method, $params, $ret) };

                    if addr != 0 {
                        new_hook!(addr, $hook);
                        census.record_resolved($label, census_door_id!($hook), &config.disabled_hooks);
                    }
                    else {
                        missing.push($label);
                        census.record_miss();
                    }
                },
                None => {
                    missing.push($label);
                    census.record_miss();
                },
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
                        census.record_resolved($label, census_door_id!($hook), &config.disabled_hooks);
                    }
                    else {
                        missing.push($label);
                        census.record_miss();
                    }
                },
                None => {
                    missing.push($label);
                    census.record_miss();
                },
            }
        };
    }

    // `SingleModeUtils::GetTrainingCutTimeScale` is armed by `AnimationSpeed` now, which is where a lever can
    // sit on it without waiting for debug_mode; its census pair arrives through `note_cut_clock` (item 77).

    // Items 88 to 90, the three doors the introspect dump named and no hook file ever reached. All three are
    // census only in this build: they print in the totals line when the game reaches them and they are absent
    // when it does not, which is what a lever on any of them has to be priced against.
    probe!(story_view, StoryViewController_SkipTrainingCutt, "SkipTrainingCutt", NO_PARAMS, VOID, "StoryViewController::SkipTrainingCutt");
    static_probe!(single_mode_utils, SingleModeUtils_GetHighSpeedPlayDuration, "GetHighSpeedPlayDuration", NO_PARAMS, R4, "SingleModeUtils::GetHighSpeedPlayDuration");
    generic_probe!(timeline_cutt_controller, TimelineTrainingCuttController_Init, "Init", NINE_REFERENCES, VOID, "TimelineTrainingCuttController::Init");

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
    resolve_stall_getters(timeline);
    probe!(timeline, CuttTimeline_SkipRuntimeTime, "SkipRuntime", ONE_FLOAT, VOID, "CutInTimelineController::SkipRuntime(time)");
    probe!(timeline, CuttTimeline_SkipRuntimeFrames, "SkipRuntime", FRAMES_AND_FLAG, VOID, "CutInTimelineController::SkipRuntime(frames, keep)");

    // The call side, held apart from the census arm above: the same door resolved once more so the
    // lever has an address to call, and armed only once (A4). It is the door the client is measured
    // using, `SkipRuntime(time)` at 2.667 s in runs 43 to 48, and not `SkipRuntime(frames, keep)` or
    // `SkipTimeDirect(time)`, which carry no totals line in any of those runs. It stays 0, and the lever
    // stays inert, on a client that has no such method.
    if let Some(controller) = timeline {
        unsafe {
            TRAINING_CUT_SKIP_ADDR =
                AnimationSpeed::resolve_method(controller, "SkipRuntime", ONE_FLOAT, VOID);
        }
    }
    probe!(timeline, CuttTimeline_SkipTimeDirect, "SkipTimeDirect", ONE_FLOAT, VOID, "CutInTimelineController::SkipTimeDirect");

    // What the timeline says about its own length, and the two doors an auto skip option would have to
    // open (A29, A30). Observed only: no value here is written back.
    probe!(timeline, CuttTimeline_GetTotalTime, "GetTotalTime", NO_PARAMS, R4, "CutInTimelineController::GetTotalTime");
    probe!(timeline, CuttTimeline_GetTotalFrameCeil, "GetTotalFrameCeil", NO_PARAMS, I4, "CutInTimelineController::GetTotalFrameCeil");
    probe!(timeline, CuttTimeline_GetCurrentFrame, "get_CurrentFrame", NO_PARAMS, I4, "CutInTimelineController::get_CurrentFrame");
    probe!(timeline, CuttTimeline_GetTargetFps, "get_TargetFps", NO_PARAMS, I4, "CutInTimelineController::get_TargetFps");
    probe!(timeline, CuttTimeline_GetSpeed, "get_Speed", NO_PARAMS, R4, "CutInTimelineController::get_Speed");
    probe!(timeline, CuttTimeline_SetSkipFrame, "set_SkipFrame", ONE_INT, VOID, "CutInTimelineController::set_SkipFrame");
    probe!(timeline, CuttTimeline_SetIsAutoPlay, "set_IsAutoPlay", ONE_FLAG, VOID, "CutInTimelineController::set_IsAutoPlay");
    probe!(timeline, CuttTimeline_GetIsAutoPlay, "get_IsAutoPlay", NO_PARAMS, BOOL, "CutInTimelineController::get_IsAutoPlay");
    probe!(timeline, CuttTimeline_Pause, "Pause", ONE_FLAG, VOID, "CutInTimelineController::Pause(pause)");
    probe!(timeline, CuttTimeline_Resume, "Resume", NO_PARAMS, VOID, "CutInTimelineController::Resume()");
    probe!(timeline, CuttTimeline_SetWaitTap, "SetWaitTap", &[CLASS], VOID, "CutInTimelineController::SetWaitTap(event)");
    probe!(timeline, CuttTimeline_SetWaitVoiceEnd, "SetWaitVoiceEnd", &[CLASS], VOID, "CutInTimelineController::SetWaitVoiceEnd(event)");
    probe!(timeline, CuttTimeline_SetEnableWaitingUpdate, "SetEnableWaitingUpdate", ONE_FLAG, VOID, "CutInTimelineController::SetEnableWaitingUpdate(enable)");

    probe!(cut_status, TrainingCutStatus_Skip, "Skip", ONE_FLAG, VOID, "SingleModeMainViewTrainingCutStatus::Skip");
    // `PlayIn/4` was last armed as a scaling door and run 26 is why it is not: its float is the play in the
    // caller sets up, while the animation runs on `CoroutinePlayIn/2 -> IEnumerator(float, int)` next door.
    // Both are observed here now, the value kept and nothing written back, because a friendship cut's wall
    // (1943 to 2235 ms in every arm of `training_cut_speed`) is close to the 1792 and 2042 ms those two doors
    // were left with, and item 79 needs a run to say which of them a lever belongs on.
    probe!(cut_status, TrainingCutStatus_PlayIn, "PlayIn", INTERVAL_FRAME_FLAG_AND_ACTION, VOID, "SingleModeMainViewTrainingCutStatus::PlayIn");
    probe!(cut_status, TrainingCutStatus_CoroutinePlayIn, "CoroutinePlayIn", INTERVAL_AND_FRAME, CLASS, "SingleModeMainViewTrainingCutStatus::CoroutinePlayIn");
    probe!(cut_status, TrainingCutStatus_PlayOut, "PlayOut", FLAG_AND_ACTION, VOID, "SingleModeMainViewTrainingCutStatus::PlayOut");
    probe!(cut_status, TrainingCutStatus_GetIntervalOutBegine, "GetIntervalOutBegine", ONE_FLOAT, R4, "SingleModeMainViewTrainingCutStatus::GetIntervalOutBegine");
    probe!(cut_status, TrainingCutStatus_WillRankUpInHighSpeedMode, "WillRankUpInHighSpeedMode", NO_PARAMS, BOOL, "SingleModeMainViewTrainingCutStatus::WillRankUpInHighSpeedMode");
    probe!(cut_status, TrainingCutStatus_ExistPlayingFrame, "ExistPlayingFrame", NO_PARAMS, BOOL, "SingleModeMainViewTrainingCutStatus::ExistPlayingFrame");

    // The panel's own frame class (`introspect.log:23857`). Three of these doors take a float the panel is
    // set up with and one is the game's own way of putting a frame straight at its end state. All five are
    // observed only, with every argument handed back untouched.
    probe!(cut_status_frame, TrainingCutStatusFrame_PlayIn, "PlayIn", FRAME_PLAY_IN_ARGS, VOID, "SingleModeMainViewTrainingCutStatusFrame::PlayIn");
    probe!(cut_status_frame, TrainingCutStatusFrame_PlayInRootMotion, "PlayInRootMotion", ONE_FLOAT, VOID, "SingleModeMainViewTrainingCutStatusFrame::PlayInRootMotion");
    probe!(cut_status_frame, TrainingCutStatusFrame_PlayOut, "PlayOut", FLAG_TIME_AND_ACTION, VOID, "SingleModeMainViewTrainingCutStatusFrame::PlayOut");
    probe!(cut_status_frame, TrainingCutStatusFrame_PlayPreIn, "PlayPreIn", NO_PARAMS, VOID, "SingleModeMainViewTrainingCutStatusFrame::PlayPreIn");
    probe!(cut_status_frame, TrainingCutStatusFrame_Skip, "Skip", FIVE_INTS, VOID, "SingleModeMainViewTrainingCutStatusFrame::Skip");

    probe!(cutt_controller, TrainingCutt_WaitTapAsync, "WaitTapAsync", NO_PARAMS, CLASS, "SingleModeMainTrainingCuttController::WaitTapAsync");
    probe!(cutt_controller, TrainingCutt_FadeOutResultFlash, "FadeOutResultFlash", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::FadeOutResultFlash");

    // The boundary v1 had to guess at, and the doors that say which cut is playing.
    probe!(cutt_controller, TrainingCutt_SetIsPlayingCutt, "set_IsPlayingCutt", ONE_FLAG, VOID, "SingleModeMainTrainingCuttController::set_IsPlayingCutt");
    probe!(cutt_controller, TrainingCutt_GetIsPlayingCutt, "get_IsPlayingCutt", NO_PARAMS, BOOL, "SingleModeMainTrainingCuttController::get_IsPlayingCutt");
    probe!(cutt_controller, TrainingCutt_IsAutoPlay, "IsAutoPlay", NO_PARAMS, BOOL, "SingleModeMainTrainingCuttController::IsAutoPlay");
    probe!(cutt_controller, TrainingCutt_UpdateTrainingCutIn, "UpdateTrainingCutIn", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::UpdateTrainingCutIn");
    probe!(cutt_controller, TrainingCutt_FixedUpdateTrainingCutIn, "FixedUpdateTrainingCutIn", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::FixedUpdateTrainingCutIn");
    probe!(cutt_controller, TrainingCutt_LateUpdateTrainingCutIn, "LateUpdateTrainingCutIn", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::LateUpdateTrainingCutIn");
    // The three doors `arm_cut_gate` already armed above the debug gate are not armed a second time here:
    // `PlayTrainingCut` and `PlayScenarioTrainingCut` open a cut run and `CleanUpCutt` closes one, and the
    // ui clock hold they carry has to stand in a build with no probe installed. Their counting and the run
    // clocks still run in the same wrappers, guarded by `CENSUS_ON`.
    probe!(cutt_controller, TrainingCutt_PlayTrainingSaboriAsync, "PlayTrainingSaboriAsync", ONE_ID, CLASS, "SingleModeMainTrainingCuttController::PlayTrainingSaboriAsync");
    probe!(cutt_controller, TrainingCutt_PlayTrainingCutEndAsync, "PlayTrainingCutEndAsync", ID_AND_FLAGS, CLASS, "SingleModeMainTrainingCuttController::PlayTrainingCutEndAsync");
    probe!(cutt_controller, TrainingCutt_TrainingAsync, "TrainingAsync", ONE_ID, CLASS, "SingleModeMainTrainingCuttController::TrainingAsync");
    probe!(cutt_controller, TrainingCutt_PlayInTrainingStatus, "PlayInTrainingStatus", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::PlayInTrainingStatus");
    probe!(cutt_controller, TrainingCutt_PlayOutTrainingStatus, "PlayOutTrainingStatus", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::PlayOutTrainingStatus");
    generic_probe!(cutt_controller, TrainingCutt_IsValidTag, "IsValidTag", RESULT_AND_LIST, BOOL, "SingleModeMainTrainingCuttController::IsValidTag");

    // The friendship door (A28). `PlayCutIn` and the static `IsValidTag` are the two places a tag cut-in is
    // named by the game itself rather than by a class label.
    generic_probe!(tag_player, TagCutInPlayer_PlayCutIn, "PlayCutIn", LIST_AND_ACTION, VOID, "SingleModeMainViewTagTrainingCutInPlayer::PlayCutIn");
    probe!(tag_player, TagCutInPlayer_PlayCutInOut, "PlayCutInOut", ONE_ACTION, VOID, "SingleModeMainViewTagTrainingCutInPlayer::PlayCutInOut");
    static_generic_probe!(tag_player, TagCutInPlayer_IsValidTag, "IsValidTag", ONE_INFO, BOOL, "SingleModeMainViewTagTrainingCutInPlayer::IsValidTag");
    // The rest of the friendship cut-in's effect doors. `CreateLineEffect` and `PlayLineEffect` are armed by
    // AnimationSpeed, which reports the writes it makes there, and `PlayButtonEffect/3` is not armed at all: its
    // second parameter is a 12 byte struct and A5 only allows structs a run has proved travel in a general
    // register, so the doors around it are counted instead of guessing at that one.
    probe!(tag_player, TagCutInPlayer_StopLineEffect, "StopLineEffect", NO_PARAMS, VOID, "SingleModeMainViewTagTrainingCutInPlayer::StopLineEffect");
    probe!(tag_player, TagCutInPlayer_DestroyButtonEffect, "DestroyButtonEffect", NO_PARAMS, VOID, "SingleModeMainViewTagTrainingCutInPlayer::DestroyButtonEffect");
    probe!(tag_player, TagCutInPlayer_PlaySuccessTextEffect, "PlaySuccessTextEffect", NO_PARAMS, VOID, "SingleModeMainViewTagTrainingCutInPlayer::PlaySuccessTextEffect");
    probe!(tag_player, TagCutInPlayer_DestroyLineEffect, "DestroyLineEffect", NO_PARAMS, VOID, "SingleModeMainViewTagTrainingCutInPlayer::DestroyLineEffect");
    probe!(tag_player, TagCutInPlayer_SetResultFlashPlayer, "SetResultFlashPlayer", ONE_INFO, VOID, "SingleModeMainViewTagTrainingCutInPlayer::SetResultFlashPlayer");

    // The doors that hand a `FlashPlayer` to a support member cut-in (`introspect.log:25120` to `25123`). Run 41
    // proved the tag cut-in's `Animator` is not what paces its 1.2 s effect leg, and a friendship cut-in is built
    // on the object these doors return. Observed only: every argument and the returned player go back untouched.
    static_generic_probe!(support_member, SupportMemberA2U_PlayMemberCutin, "PlayMemberCutin", MEMBER_CUTIN_ARGS, CLASS, "TrainingParamChangeSupportMemberA2U::PlayMemberCutin");
    static_probe!(support_member, SupportMemberA2U_PlayAoharuSoulCutin, "PlayAoharuSoulCutin", SOUL_CUTIN_ARGS, CLASS, "TrainingParamChangeSupportMemberA2U::PlayAoharuSoulCutin");
    static_probe!(support_member, SupportMemberA2U_PlayAoharuSpSoulCutin, "PlayAoharuSpSoulCutin", SOUL_CUTIN_ARGS, CLASS, "TrainingParamChangeSupportMemberA2U::PlayAoharuSpSoulCutin");
    static_probe!(support_member, SupportMemberA2U_PlayOutMemberCutin, "PlayOutMemberCutin", PLAYER_INDEX_AND_ACTION, VOID, "TrainingParamChangeSupportMemberA2U::PlayOutMemberCutin");

    // The class a training cut-in effect is played on (`introspect-run45.log:26446`): it wraps an
    // `AnimateToUnity.AnMotion`, and its `Play` doors are the only ones on it that take a number. Run 42 handed the
    // tag cut-in player 167 flash players through `SetResultFlashPlayer` and reached none of these doors because no
    // build had armed them, so this is the first measurement taken on the object the 1.2 s effect leg runs on.
    unsafe {
        if let Some(class) = flash_player {
            FLASH_LAST_MOTION_SPEED_FIELD = get_field_from_name(class, c"_lastMotionSpeed");

            // The accessor answers a missing field with a zero, and a run that reads 0.0 cannot tell that from a
            // field the game holds at 0.0. Saying which one it is costs one line at init.
            if FLASH_LAST_MOTION_SPEED_FIELD.is_null() {
                info!("Cutt probe: FlashPlayer has no _lastMotionSpeed field, so every speed read on that door is a zero this build never read");
            }
        }
    }

    probe!(flash_player, FlashPlayer_PlayAction, "Play", ONE_ACTION, VOID, "FlashPlayer::Play(action)");
    // `AnimateToUnity.AnMotion` is in `Plugins.dll`, an image the dump scope does not walk (A8), and run 45 named it
    // through `Gallop.FlashPlayer._motion`. `AnimationSpeed` holds the speed door on this class, so only the play
    // family is censused here and no door is armed on an address the lever owns (A4).
    let motion_class = crate::il2cpp::symbols::get_assembly_image(c"Plugins.dll").ok().and_then(|image| {
        if image.is_null() {
            return None;
        }

        let class = crate::il2cpp::api::il2cpp_class_from_name(image, c"AnimateToUnity".as_ptr(), c"AnMotion".as_ptr());

        if class.is_null() { None } else { Some(class) }
    });

    probe!(motion_class, AnMotion_SetMotionPlayLabelFloat, "SetMotionPlay", LABEL_AND_FLOAT, VOID, "AnMotion::SetMotionPlay(label, float)");
    probe!(motion_class, AnMotion_SetMotionPlayLabelInt, "SetMotionPlay", LABEL_AND_INT, VOID, "AnMotion::SetMotionPlay(label, int)");
    probe!(motion_class, AnMotion_SetMotionPlayFloat, "SetMotionPlay", ONE_FLOAT, VOID, "AnMotion::SetMotionPlay(float)");
    probe!(motion_class, AnMotion_SetMotionPlay, "SetMotionPlay", NO_PARAMS, VOID, "AnMotion::SetMotionPlay");
    probe!(motion_class, AnMotion_SetMotionStop, "SetMotionStop", NO_PARAMS, VOID, "AnMotion::SetMotionStop");
    probe!(flash_player, FlashPlayer_Pause, "Pause", ONE_FLAG, VOID, "FlashPlayer::Pause(flag)");
    probe!(flash_player, FlashPlayer_Resume, "Resume", NO_PARAMS, VOID, "FlashPlayer::Resume");
    probe!(flash_player, FlashPlayer_Stop, "Stop", NO_PARAMS, VOID, "FlashPlayer::Stop");
    probe!(flash_player, FlashPlayer_End, "End", NO_PARAMS, VOID, "FlashPlayer::End");
    static_probe!(graphic_settings, GraphicSettings_SetEffectTimeScale, "SetEffectTimeScale", ONE_FLOAT, VOID, "GraphicSettings::SetEffectTimeScale");
    probe!(flash_action_player, FlashActionPlayer_PlayWithTime, "Play", LABEL_TIME_AND_LABEL, VOID, "FlashActionPlayer::Play(label, time, label)");
    probe!(flash_action_player, FlashActionPlayer_PlayTwoLabels, "Play", TWO_LABELS, VOID, "FlashActionPlayer::Play(label, label)");

    // Run 9's one install failure: the parameter is a generic instantiation, which the exact walk has never
    // answered (C48). The wrapper declares a pointer for it and never reads through it.
    // `InitializePlateList` is not probed here any more: AnimationSpeed hooks that address to scale the
    // interval, and a probe cannot hook an address a second time. Its clocks are fed by
    // `note_plate_call`, which the scaling hook reports to.
    probe!(plate_ui, PlateUI_GetIsAutoPlay, "get_IsAutoPlay", NO_PARAMS, BOOL, "TrainingParamChangeUI::get_IsAutoPlay");
    probe!(plate_ui, TrainingParamChangeUI_PlayIcon, "PlayIcon", INT_AND_INFO, VOID, "TrainingParamChangeUI::PlayIcon");
    probe!(plate_ui, TrainingParamChangeUI_IsGroupPlay, "IsGroupPlay", ONE_INFO, BOOL, "TrainingParamChangeUI::IsGroupPlay");
    probe!(plate_ui, TrainingParamChangeUI_Update, "Update", NO_PARAMS, VOID, "TrainingParamChangeUI::Update");
    probe!(plate_ui, TrainingParamChangeUI_StartSequence, "StartSequence", NO_PARAMS, VOID, "TrainingParamChangeUI::StartSequence");
    probe!(plate_ui, TrainingParamChangeUI_StartGroupTypewrite, "StartGroupTypewrite", NO_PARAMS, VOID, "TrainingParamChangeUI::StartGroupTypewrite");
    probe!(plate_ui, TrainingParamChangeUI_StartTypewrite, "StartTypewrite", ONE_INT, VOID, "TrainingParamChangeUI::StartTypewrite");
    probe!(plate_ui, TrainingParamChangeUI_OnNextTypewrite, "OnNextTypewrite", NO_PARAMS, VOID, "TrainingParamChangeUI::OnNextTypewrite");
    probe!(plate_ui, TrainingParamChangeUI_OnEndTypeWrite, "OnEndTypeWrite", ONE_INT, VOID, "TrainingParamChangeUI::OnEndTypeWrite");
    probe!(plate_ui, TrainingParamChangeUI_OnAllTypewriteEnd, "OnAllTypewriteEnd", NO_PARAMS, VOID, "TrainingParamChangeUI::OnAllTypewriteEnd");
    probe!(plate_ui, TrainingParamChangeUI_OnTapScreen, "OnTapScreen", NO_PARAMS, VOID, "TrainingParamChangeUI::OnTapScreen");
    probe!(plate_ui, TrainingParamChangeUI_SetTapButtonOrder, "SetTapButtonOrder", ONE_INT, VOID, "TrainingParamChangeUI::SetTapButtonOrder");
    generic_probe!(plate_ui, TrainingParamChangeUI_InitializeFlash, "InitializeFlash", LIST_INTERVAL_FLAG_AND_CANVAS, VOID, "TrainingParamChangeUI::InitializeFlash");
    generic_probe!(plate_ui, TrainingParamChangeUI_Initialize, "Initialize", PLATE_INITIALIZE_ARGS, VOID, "TrainingParamChangeUI::Initialize");
    generic_probe!(story_view, StoryViewController_PlayParameterChangeAsync, "PlayParameterChangeAsync", LIST_AND_INTERVAL, CLASS, "StoryViewController::PlayParameterChangeAsync");

    // The gauge animation is the largest piece of the training screen that run 11 measured no part of.
    probe!(hp_gauge, HpGauge_PlayIn, "PlayIn", NO_PARAMS, VOID, "SingleModeMainViewHpGauge::PlayIn");
    probe!(hp_gauge, HpGauge_PlayValue, "PlayValue", ONE_INT, VOID, "SingleModeMainViewHpGauge::PlayValue");
    probe!(hp_gauge, HpGauge_PlayOut, "PlayOut", NO_PARAMS, VOID, "SingleModeMainViewHpGauge::PlayOut");
    probe!(cut_status, TrainingCutStatus_PlayPreIn, "PlayPreIn", NO_PARAMS, VOID, "SingleModeMainViewTrainingCutStatus::PlayPreIn");
    probe!(cut_status, TrainingCutStatus_PlayEnd, "PlayEnd", NO_PARAMS, VOID, "SingleModeMainViewTrainingCutStatus::PlayEnd");
    probe!(main_view, SingleModeMain_CoroutineDoTweenTimeScale, "CoroutineDoTweenTimeScale", NO_PARAMS, CLASS, "SingleModeMainViewController::CoroutineDoTweenTimeScale");
    probe!(main_view, SingleModeMain_WaitTap, "WaitTap", NO_PARAMS, CLASS, "SingleModeMainViewController::WaitTap");
    probe!(main_view, SingleModeMain_OnClickTraining, "OnClickTraining", NO_PARAMS, VOID, "SingleModeMainViewController::OnClickTraining");
    probe!(main_view, SingleModeMain_BackFromTraining, "BackFromTraining", NO_PARAMS, CLASS, "SingleModeMainViewController::BackFromTraining");
    probe!(main_view, SingleModeMain_TryRemainTurnChange, "TryPlayRemainTurnChangeAnimation", NO_PARAMS, BOOL, "SingleModeMainViewController::TryPlayRemainTurnChangeAnimation");
    probe!(main_view, SingleModeMain_CommonSendCommandAsync, "CommonSendCommandAsync", TWO_COMMAND_IDS, CLASS, "SingleModeMainViewController::CommonSendCommandAsync");
    // `SendCommandAsync/6` is static and carries two generic callbacks, so it needs the static generic
    // matcher (A3 for the missing `this`, C48 for the generic arguments).
    static_generic_probe!(main_view, SingleModeMain_SendCommandAsync, "SendCommandAsync", COMMAND_SEND_ARGS, CLASS, "SingleModeMainViewController::SendCommandAsync");

    info!("{}", census_line(&census, PROBES.len()));

    if !census.disabled.is_empty() {
        info!("{}", census_disabled_line(&census));
    }

    if !missing.is_empty() {
        info!("Cutt probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the frame probe report.
/// How long the training doors may stay quiet before the census says so out loud. The stall this line
/// exists for held a live cut-in timeline for over five minutes with every other training door frozen,
/// and a player who walked away from a menu never reaches it: the census only runs when a door was
/// called, so an idle screen with no timeline alive produces no census at all.
const SILENCE_WARN_SECS: i64 = 120;

/// The counters allowed to keep growing inside a silence window. `CutInTimelineController::UpdateSpeed`
/// is called every frame a timeline object exists, advancing or not, and that is what makes "this one
/// grew and nothing else did" the reading of a frozen playhead rather than an idle screen. The 2026-10-10
/// stall ran it 60 times a second for five minutes on a screen where every cut, plate and high speed
/// stepping door had stopped.
const SILENCE_EXEMPT_LABELS: [&str; 1] = ["CutInTimelineController::UpdateSpeed()"];

fn is_exempt(label: &str) -> bool {
    SILENCE_EXEMPT_LABELS.contains(&label)
}

/// The calls on the doors that move the flow forward, from the `(label, calls)` pairs the census reads.
/// Held apart from the census so the split - which counter counts as progress - is a rule the tests run.
fn progress_total(entries: &[(&str, usize)]) -> usize {
    entries.iter().filter(|(label, _)| !is_exempt(label)).map(|(_, calls)| *calls).sum()
}

fn exempt_total(entries: &[(&str, usize)]) -> usize {
    entries.iter().filter(|(label, _)| is_exempt(label)).map(|(_, calls)| *calls).sum()
}

fn silence_due(silence_sec: i64) -> bool {
    silence_sec >= SILENCE_WARN_SECS
}

/// The line itself, apart from the log call, because it has to tell two readings apart that look the
/// same in frozen counters: a timeline nothing is advancing, and a game waiting for a human to tap. The
/// open cut run is what separates them, and `Time.timeScale` is what separates a frozen playhead from a
/// paused one, which the write hook cannot say on its own: it logs its first six calls and then one per
/// 4096, so a value the game wrote five minutes into a session never appears there.
fn silence_line(silence_sec: i64, exempt_since_last: usize, exempt_total: usize, time_scale: f32, open_for_ms: i64) -> String {
    let scale = if time_scale.is_finite() { time_scale.to_string() } else { "unread".to_string() };
    let tail = if open_for_ms > 0 {
        format!("a training cut run has been open for {open_for_ms} ms, which is what a wait for a tap looks like")
    } else {
        "no training cut run open".to_string()
    };

    format!(
        "Cutt probe flow silence {silence_sec} s: {} ran {exempt_since_last} more times ({exempt_total} total) with no call on any other training door; Time.timeScale reads {scale}; {tail}",
        SILENCE_EXEMPT_LABELS[0]
    )
}

/// The cascade line C54 and C62 are owed, spelled as the shipped report prints it. A run cannot close an item
/// on a wording its build does not print, so the whole line is here: label `Cutt probe plate cascade:`, then
/// the aggregate half in this order - `{n} cascades closed mean {mean} ms worst {worst} ms` - then `typewrite
/// starts {m} inside a cascade mean {mean} ms worst {worst} ms`, then the span tail.
///
/// What each half can be read for, stated as far as it actually goes (C56). The aggregate prices a running
/// total over every cascade that has closed and the largest span among them, and it names one cascade's span
/// only where exactly one cascade closed between two readings, as the difference of the two totals its mean
/// prices. The tail is where a span is read rather than solved: the span of every cascade that closed since
/// the previous reading, in close order, each said as `cascade number=span ms`, written by `close_plate_pass`
/// at the close, where the probe still knows which cascade it is standing on. The tail holds
/// `CASCADE_SPAN_RING` spans per reading and names the older spans it dropped instead of letting a replay
/// assume it read them all.
///
/// Run 57 is the run the aggregate half cannot read: it printed this line 127 times, at closed counts 0, 0,
/// 2, 3, 3, 4 and 5 x121 and never once at 1, and its build had no span tail and no per cascade line at all,
/// so that log prices four running totals and leaves the run's first two cascades as one 2,552 ms sum. The
/// spans a replay names for those two are the tail this change set prints for the same cascade boundaries,
/// not a reading run 57 left behind.
fn plate_cascade_line(closed: (usize, i64, i64), steps: (usize, i64, i64), ring: &CascadeSpans, printed: usize) -> String {
    format!(
        "Cutt probe plate cascade: {} cascades closed mean {:.1} ms worst {} ms, typewrite starts {} inside a cascade mean {:.1} ms worst {} ms{}",
        closed.0,
        mean_of(closed.1, closed.0),
        closed.2,
        steps.0,
        mean_of(steps.1, steps.0),
        steps.2,
        ring.span_tail(printed)
    )
}

/// The hole line C56 is owed, spelled as the shipped report prints it: label `Cutt probe cut holes:`, then
/// `{n} holes mean {mean} ms worst {worst} ms`, then the pairing `between the training cut-in ending and the
/// status play out`, then how many holes each of the two door orders produced - `{a} from the cut-in ending to
/// the play out, {b} from the play out to the cut-in ending`.
///
/// The two orders are on the line because they are two different waits, and run 57 is the run that showed the
/// pairing only reads when the line says which end opened the window: it printed `0 holes mean 0.0 ms` beside
/// `cut runs 1 wall 8898 ms` because the panel played out first, 20:44:22.847, and the cut-in ended 1,400 ms
/// later, 20:44:24.247. A line that only ever names the cut-in end reads that shape as a hole that never
/// happened. This wording replaced the one that ran 16 to run 57 printed, `... worst {worst} ms from the
/// training cut-in ending to the status play out`, and any owed line that still quotes that ending is a line
/// no build in this fork prints any more.
fn hole_line(runs: usize, total_ms: i64, worst_ms: i64, end_runs: &[usize; HOLE_END_COUNT]) -> String {
    format!(
        "Cutt probe cut holes: {runs} holes mean {:.1} ms worst {worst_ms} ms between the training cut-in ending and the status play out, {} from the cut-in ending to the play out, {} from the play out to the cut-in ending",
        mean_of(total_ms, runs),
        end_runs[HOLE_END_CUT_IN],
        end_runs[HOLE_END_PLAY_OUT]
    )
}

/// The line C54's closing pair names next to the cascade line: label `Cutt probe cut kinds:`, then the
/// friendship split as `{kind} runs {n} wall {ms} ms` per kind reached, then the cut-in engine's own self
/// report - `timeline self report total frames peak {frames} last frame peak {frame} target fps {fps}`. It is
/// the pair that says whether a raised cut scale moved the cascade or only moved a number: the cascade line
/// prices the cascades, this one says whether the cut-in timeline was stepping beside them.
fn cut_kinds_line(kinds: &str, total_frames: u32, last_frame: u32, target_fps: u32) -> String {
    format!("Cutt probe cut kinds:{kinds} timeline self report total frames peak {total_frames} last frame peak {last_frame} target fps {target_fps}")
}

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
            let _ = write!(line, " {}={} peak {:.3}", probe.label(), calls, peak_seconds(probe.peak_bits()));
        }
        else {
            let _ = write!(line, " {}={}", probe.label(), calls);
        }
    }

    let runs = RUNS_CLOSED.load(atomic::Ordering::Relaxed);
    let wall_ms = RUN_WALL_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let peak_ms = RUN_PEAK_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let opened = RUN_OPENED_MS.load(atomic::Ordering::Relaxed);
    let open_for = if opened < 0 { 0 } else { elapsed_ms() - opened };

    // Runs grouped by the screen they started on, which is the difference between "the training cut-in
    // costs 3 s" and "some cut-ins cost 3 s".
    let mut buckets = String::new();

    for index in 0..BUCKET_COUNT {
        let count = RUN_BUCKET_COUNT[index].load(atomic::Ordering::Relaxed);

        if count == 0 {
            continue;
        }

        let _ = write!(
            buckets,
            " {}(view {}) runs {count} wall {} ms timeline {} ms, tap waited {} ms in {} cuts, gap between cuts {} ms",
            BUCKET_NAMES[index],
            RUN_BUCKET_VIEW[index].load(atomic::Ordering::Relaxed),
            RUN_BUCKET_WALL_MS[index].load(atomic::Ordering::Relaxed),
            RUN_BUCKET_PEAK_MS[index].load(atomic::Ordering::Relaxed),
            TAP_WAIT_BUCKET_MS[index].load(atomic::Ordering::Relaxed),
            TAP_WAIT_BUCKET_RUNS[index].load(atomic::Ordering::Relaxed),
            CUT_GAP_BUCKET_MS[index].load(atomic::Ordering::Relaxed)
        );
    }

    // The two doors AnimationSpeed arms on the training screen. They printed install lines in run 8 and no
    // call line, and this probe cannot hook those addresses a second time, so their own call counts ride
    // along on this line (A21). Since item 59 they are counts only: AnimationSpeed hands both gates the
    // value the game passed, so `InitializePlateList=36` says the game reached the door 36 times, not that
    // this fork shortened it 36 times (C58). The label stays as the arm recipes in LEDGER name it.
    let mut doors = String::new();

    for (slot, name) in AnimationSpeed::TRAINING_HIT_SLOTS.iter() {
        let _ = write!(doors, " {name}={}", AnimationSpeed::hit_calls(*slot));
    }

    // The friendship split, and what the cut-in engine reported about itself. On their own line so the
    // totals above stay readable (A28, A29).
    let mut kinds = String::new();

    for index in 0..TAG_COUNT {
        let count = RUN_TAG_COUNT[index].load(atomic::Ordering::Relaxed);

        if count == 0 {
            continue;
        }

        let _ = write!(
            kinds,
            " {} runs {count} wall {} ms",
            TAG_NAMES[index],
            RUN_TAG_WALL_MS[index].load(atomic::Ordering::Relaxed)
        );
    }

    let total_frames = TIMELINE_TOTAL_FRAMES_PEAK.load(atomic::Ordering::Relaxed);
    let last_frame = TIMELINE_CURRENT_FRAME_PEAK.load(atomic::Ordering::Relaxed);
    let target_fps = TIMELINE_TARGET_FPS.load(atomic::Ordering::Relaxed);

    // The window still open gets a tail on the totals line rather than a line of its own, so naming the
    // arm a run is on costs no log volume.
    let window = settings_preset::arm_window();
    let arm = match recover_lock(&WINDOW_AGG).get(&window).cloned() {
        Some(agg) if agg.runs > 0 => format!(" arm {} #{} cuts {} wall {} ms", settings_preset::arm_window_name(window), window, agg.runs, agg.wall_ms),
        _ => String::new(),
    };

    info!("Cutt probe totals at {now_sec} s:{line} cut runs {runs} wall {wall_ms} ms timeline {peak_ms} ms open {open_for} ms{arm}");
    info!("Cutt probe cut runs by screen:{buckets}");

    // The three clocks run 11 left open, on their own line so the totals above stay readable. The tap
    // wait is what a cut costs after it asked the player for a tap, the gap is what the game spends
    // between cuts, and the plate step is the wall clock between two plate list intervals.
    let tap_runs = TAP_WAIT_RUNS.load(atomic::Ordering::Relaxed);
    let tap_ms = TAP_WAIT_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let gap_runs = CUT_GAP_RUNS.load(atomic::Ordering::Relaxed);
    let gap_ms = CUT_GAP_MS_TOTAL.load(atomic::Ordering::Relaxed);
    let plate_runs = PLATE_STEP_RUNS.load(atomic::Ordering::Relaxed);
    let plate_ms = PLATE_STEP_MS_TOTAL.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe cut clocks: tap waits {tap_runs} of {runs} closes mean {:.1} ms, gaps between cuts {gap_runs} mean {:.1} ms worst {} ms, plate list steps {plate_runs} mean {:.1} ms worst {} ms",
        mean_of(tap_ms, tap_runs),
        mean_of(gap_ms, gap_runs),
        CUT_GAP_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_of(plate_ms, plate_runs),
        PLATE_STEP_WORST_MS.load(atomic::Ordering::Relaxed)
    );

    // The gap and the wall above, split at the doors inside them. This is the line that says whether the
    // next lever should aim at the plate list, at what the game does after the plate list, or at the cut
    // itself, and it costs no hook because all three marks come from doors the probe already stands on.
    let leg_runs = GAP_LEG_BEFORE_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe gap legs: {leg_runs} of {gap_runs} gaps held a plate pass, before it mean {:.1} ms worst {} ms, the passes themselves mean {:.1} ms worst {} ms, between passes mean {:.1} ms worst {} ms, after it mean {:.1} ms worst {} ms, {} gaps held none",
        mean_of(GAP_LEG_BEFORE_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
        GAP_LEG_BEFORE_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_of(GAP_LEG_PLATE_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
        GAP_LEG_PLATE_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_of(GAP_LEG_BETWEEN_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
        GAP_LEG_BETWEEN_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_of(GAP_LEG_AFTER_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
        GAP_LEG_AFTER_WORST_MS.load(atomic::Ordering::Relaxed),
        GAP_WITHOUT_PLATE_RUNS.load(atomic::Ordering::Relaxed)
    );

    // The same gap read at its command doors: what the game spent waiting on the request it sent, and
    // what it spent before it sent one at all. Neither is a wall a duration lever reaches, and the run
    // cannot say how much of the turn is left for the levers until both are counted.
    let send_runs = COMMAND_SEND_RUNS.load(atomic::Ordering::Relaxed);
    let send_to_open_runs = SEND_TO_OPEN_RUNS.load(atomic::Ordering::Relaxed);
    let close_to_send_runs = CLOSE_TO_SEND_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe command legs: {send_runs} sends, {send_to_open_runs} cut opens after a send mean {:.1} ms worst {} ms, {close_to_send_runs} sends after a cut close mean {:.1} ms worst {} ms",
        mean_of(SEND_TO_OPEN_MS_TOTAL.load(atomic::Ordering::Relaxed), send_to_open_runs),
        SEND_TO_OPEN_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_of(CLOSE_TO_SEND_MS_TOTAL.load(atomic::Ordering::Relaxed), close_to_send_runs),
        CLOSE_TO_SEND_WORST_MS.load(atomic::Ordering::Relaxed)
    );

    let played_runs = WALL_OPEN_TO_TAP_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe cut wall legs: {played_runs} of {runs} closes asked for a tap, the cut played mean {:.1} ms worst {} ms before it asked",
        mean_of(WALL_OPEN_TO_TAP_MS_TOTAL.load(atomic::Ordering::Relaxed), played_runs),
        WALL_OPEN_TO_TAP_WORST_MS.load(atomic::Ordering::Relaxed)
    );
    let gauge_runs = GAUGE_PLAY_RUNS.load(atomic::Ordering::Relaxed);
    let icon_runs = PLATE_PLAY_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe plate cadence: gauge plays {gauge_runs} inside a cut or a cascade mean {:.1} ms worst {} ms, plate icon plays {icon_runs} inside a cut or a cascade mean {:.1} ms worst {} ms",
        mean_of(GAUGE_PLAY_MS_TOTAL.load(atomic::Ordering::Relaxed), gauge_runs),
        GAUGE_PLAY_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_of(PLATE_PLAY_MS_TOTAL.load(atomic::Ordering::Relaxed), icon_runs),
        PLATE_PLAY_WORST_MS.load(atomic::Ordering::Relaxed)
    );

    // How long one cascade of stat plates takes, and how far apart its plates arrive. Run 14 answered
    // neither: it counted 22 plate list calls 5,129.3 ms apart and said nothing about the time inside one
    // cascade, which is the difference between a cascade that runs for seconds and a controller that
    // starts a new one every few seconds. Those two need different doors (C54).
    //
    // What this line can be read for, stated as far as it actually goes (C56). The mean prices the total of
    // every cascade closed so far - to within half a tenth of a millisecond per closed cascade, the rounding
    // of a mean printed to one decimal - and the worst prices the largest of them. Those two give one
    // cascade's span only when exactly one cascade closed between two readings, as the difference of the two
    // totals they price. Run 57 is the run that shows the limit, and it is the run this comment used to cite
    // as its proof: it printed this line 127 times, at closed counts 0 (`run log/hachimi-run57.log:1309`,
    // `:1483`), 2 (`:1687`), 3 (`:1771`, `:1793`), 4 (`:1834`) and 5 (121 times from `:1879` on, `:3669`
    // among them), and never once at 1 closed. Its first two cascades therefore left the log as the single
    // 2,552 ms sum they add to. Reading them as 1,314 ms and 1,238 ms is not a reading off this line: it
    // borrows the printed worst, which says only that the larger of the two is 1,314 ms and not which
    // cascade it was, and with three or more cascades closing between readings - and the worst belonging to
    // a cascade that closed before them - the line splits nothing at all.
    // `run_57s_cascade_readings_do_not_name_which_of_its_first_two_cascades_was_the_worst` prints both
    // orders, and each reproduces every reading that run logged.
    //
    // The rule has a positive case too. Run 31 printed this line at closed counts 0 (`run
    // log/hachimi-run31.log:958`), 2 (`:1139`), 6 (`:1317`), 8 (`:1353`, `:1376`) and 9 (81 printings to
    // `:2505`), so exactly one of its boundaries closed a single cascade: the totals its means price are 737,
    // 2,196, 2,771 and 3,044 ms, and 3,044 less 2,771 is cascade 9's 273 ms.
    // `run_31s_cascade_line_names_the_one_span_its_readings_allow_and_leaves_the_rest_as_sums` holds that
    // arithmetic beside the run 57 half above: a run whose readings name nothing individually, and a run whose
    // readings name exactly one thing.
    //
    // So each cascade's own span is printed here too, in close order, named with the number its window
    // opened on. That tail is what a replay reads, and it is written at the close, where the probe still
    // knows which cascade it is standing on. The mean and the worst stay what they always priced - a running
    // total and a max - and a span is never recovered out of them when several cascades closed between two
    // readings.
    //
    // What the tail holds, and where it stops: `CASCADE_SPAN_RING` spans per reading, oldest first in close
    // order across the wrap, and a run that closed more than that between two readings is told how many
    // spans the line is not showing (`the_span_ring_holds_the_last_closes_and_says_which_spans_it_dropped`).
    // Run 57 read 5 cascades over 127 reports, so its four boundaries would each have fitted. Run 15 is the
    // one to watch: 17 cascades, which is 16 plus one, and a report that saw all 17 close between two
    // readings would name 16 and say one was gone.
    //
    // The per cascade reading is the narrower half, and it is the debug half. `close_plate_pass` prints
    // `cascade_line` for the first `PROBE_DETAIL_LIMIT` cascades of a session and only under `debug_mode`:
    // `core/log.rs:3-9` holds a played build with `debug_mode` off at `LevelFilter::Info`, so a census run
    // that closes C54 or C62 is a `debug_mode` run by definition, and past cascade 8 the number a cascade
    // waited on survives only as a count on the pacing line below, never as a line about that cascade. The
    // spans are the part every cascade gets: the tail is an `info!` and covers every close since the last
    // reading whether the detail line printed or not.
    //
    // A difference between the stamps of two door lines is still not a span either: the stamp is written by
    // the logger inside the wrapper, the span is a pair of `Instant` reads taken around it, and run 57's
    // stamp differences (1314.6 / 1237.7 / 1204.7 / 1828.4 / 5105.4 ms) bracket the measured spans within a
    // millisecond without reproducing the first or the last. `run_57s_five_cascade_replay_prices_the_spans_its_readings_allow_and_its_first_two_as_one_sum`
    // and `a_cascade_span_is_the_probes_clock_not_the_difference_of_two_log_stamps` hold that arithmetic.
    let pass_runs = PLATE_PASS_RUNS.load(atomic::Ordering::Relaxed);
    let typewrite_runs = PLATE_TYPEWRITE_RUNS.load(atomic::Ordering::Relaxed);
    let closed_cascades = (
        pass_runs,
        PLATE_PASS_MS_TOTAL.load(atomic::Ordering::Relaxed),
        PLATE_PASS_WORST_MS.load(atomic::Ordering::Relaxed),
    );
    let plate_steps = (
        typewrite_runs,
        PLATE_TYPEWRITE_MS_TOTAL.load(atomic::Ordering::Relaxed),
        PLATE_TYPEWRITE_WORST_MS.load(atomic::Ordering::Relaxed),
    );
    let ring = cascade_span_ring();
    let already_printed = CASCADE_SPANS_PRINTED.load(atomic::Ordering::Relaxed);
    let spans_line = plate_cascade_line(closed_cascades, plate_steps, &ring, already_printed);

    // Every span the ring has taken has now been through a reading, whether the tail could name all of them
    // or said it could not: a span older than the ring is no longer held, and the line says so rather than
    // letting a replay assume it read them all.
    CASCADE_SPANS_PRINTED.store(ring.writes, atomic::Ordering::Relaxed);

    info!("{spans_line}");
    // What each cascade that closed waits on, counted by the answer rather than by the peak: C54 found the
    // interval is not the pacing number and C62 found it is a beat above about 0.25 s, and both answers are
    // now per cascade. A cascade that advanced on none of these numbers is the run saying the wait sits on a
    // constant this class carries as a literal, or on a door nobody has counted yet.
    let pace_reported = CASCADE_REPORTED.load(atomic::Ordering::Relaxed);
    let mut pace_names = String::new();

    for index in 0..=PACE_NONE {
        let _ = write!(pace_names, " {} {}", PACE_NAMES[index], PACE_RUNS[index].load(atomic::Ordering::Relaxed));
    }

    info!("Cutt probe plate cascade pacing: {pace_reported} cascades read what they hold, advanced on:{pace_names}");
    let hole_runs = CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed);
    let hole_end_runs = [CUT_HOLE_END_RUNS[HOLE_END_CUT_IN].load(atomic::Ordering::Relaxed), CUT_HOLE_END_RUNS[HOLE_END_PLAY_OUT].load(atomic::Ordering::Relaxed)];

    info!("{}", hole_line(hole_runs, CUT_HOLE_MS_TOTAL.load(atomic::Ordering::Relaxed), CUT_HOLE_WORST_MS.load(atomic::Ordering::Relaxed), &hole_end_runs));
    info!("{}", cut_kinds_line(&kinds, total_frames, last_frame, target_fps));
    info!("Cutt probe training scaling points reached:{doors}");

    // A window is printed once, after the picker has moved off it, so the arm a run was on and the cuts
    // that ran under it are in the log even though the session never repeats the same content. A window
    // that saw no cut is left out: an arm with no sample has nothing to say.
    let closed_from = FLUSHED_WINDOW.load(atomic::Ordering::Relaxed) + 1;
    let current_window = settings_preset::arm_window();

    if closed_from < current_window {
        let mut closed: Vec<(usize, String, WindowAgg)> = Vec::new();

        for index in closed_from..current_window {
            if let Some(agg) = recover_lock(&WINDOW_AGG).get(&index).cloned() {
                closed.push((index, settings_preset::arm_window_name(index), agg));
            }
        }

        for (index, name, agg) in closed {
            if agg.runs == 0 {
                continue;
            }

            info!("{}", window_line(index, &name, &agg));
        }
    }

    FLUSHED_WINDOW.store(current_window.saturating_sub(1).max(FLUSHED_WINDOW.load(atomic::Ordering::Relaxed)), atomic::Ordering::Relaxed);

    // The stall reading. Reaching here with the advancing doors unchanged means the only counter that
    // moved was the per-frame one, and the census has been printing all along because of it. What the
    // frozen counters cannot say by themselves is how long that has held and what the game's clock reads,
    // so the line states both. The first crossing is a warn and every later one a debug: a stall that
    // lasts ten minutes should be visible once and clocked after that, not repeated at warn level every
    // interval.
    let entries: Vec<(&str, usize)> = PROBES.iter().map(|probe| (probe.label(), probe.calls())).collect();
    let progress = progress_total(&entries);
    let exempt = exempt_total(&entries);

    if progress != LAST_PROGRESS_TOTAL.load(atomic::Ordering::Relaxed) {
        LAST_PROGRESS_TOTAL.store(progress, atomic::Ordering::Relaxed);
        LAST_EXEMPT_TOTAL.store(exempt, atomic::Ordering::Relaxed);
        LAST_PROGRESS_SEC.store(now_sec, atomic::Ordering::Relaxed);
        SILENCE_WARNED.store(false, atomic::Ordering::Relaxed);
    }
    else {
        let silence = now_sec - LAST_PROGRESS_SEC.load(atomic::Ordering::Relaxed);

        if silence_due(silence) {
            let since_last = exempt.saturating_sub(LAST_EXEMPT_TOTAL.load(atomic::Ordering::Relaxed));
            let line = silence_line(silence, since_last, exempt, crate::il2cpp::hook::UnityEngine_CoreModule::Time::time_scale_now(), open_for);

            if SILENCE_WARNED.swap(true, atomic::Ordering::AcqRel) {
                debug!("{line}");
            }
            else {
                warn!("{line}");
            }
        }
    }
}

static LAST_PROGRESS_TOTAL: AtomicUsize = AtomicUsize::new(0);
static LAST_EXEMPT_TOTAL: AtomicUsize = AtomicUsize::new(0);
static LAST_PROGRESS_SEC: AtomicI64 = AtomicI64::new(0);
static SILENCE_WARNED: AtomicBool = AtomicBool::new(false);

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    // The window line is what a comparison is read off, so its numbers are checked as text: an arm that
    // cannot be told apart from the arm next to it in the log is the same as not measuring it at all.
    #[test]
    fn the_window_line_splits_an_arms_cuts_into_the_animation_and_the_tap() {
        let mut agg = WindowAgg::default();

        agg.first_ms = 10_000;
        agg.last_ms = 610_000;
        agg.runs = 3;
        agg.wall_ms = 6_000;
        agg.played_ms = 4_500;
        agg.tap_ms = 1_500;
        agg.gaps = 2;
        agg.gap_ms = 40_000;
        agg.holes = 1;
        agg.hole_ms = 3_000;
        agg.kind_runs[TAG_FRIENDSHIP] = 2;
        agg.kind_wall_ms[TAG_FRIENDSHIP] = 4_000;
        agg.kind_runs[TAG_UNKNOWN] = 1;
        agg.kind_wall_ms[TAG_UNKNOWN] = 2_000;

        let line = window_line(2, "All levers", &agg);

        assert!(line.contains("arm window All levers #2 span 600 s"), "{line}");
        assert!(line.contains("cuts 3 wall 6000 ms played 4500 ms tap 1500 ms"), "{line}");
        assert!(line.contains("gaps 2 40000 ms, holes 1 3000 ms"), "{line}");
        assert!(line.contains("friendship cut 2 runs 4000 ms"), "{line}");
        assert!(line.contains("tag answer unknown 1 runs 2000 ms"), "{line}");
    }

    #[test]
    fn a_window_line_leaves_out_the_cut_kind_that_arm_never_saw() {
        let mut agg = WindowAgg::default();

        agg.last_ms = 15_000;
        agg.first_ms = 10_000;
        agg.runs = 1;
        agg.wall_ms = 1_900;
        agg.kind_runs[TAG_REGULAR] = 1;
        agg.kind_wall_ms[TAG_REGULAR] = 1_900;

        let line = window_line(1, "Neutral", &agg);

        assert!(line.contains("regular cut 1 runs 1900 ms"), "{line}");
        assert!(!line.contains("friendship"), "a kind the arm never reached must not show up as a zero");
        assert!(!line.contains("tag answer"), "{line}");
    }

    // The census window is one process wide static, and cargo runs tests on several threads, so the tests
    // that open and close a window take this turn instead of racing each other for it.
    static CENSUS_TEST_TURN: Mutex<()> = Mutex::new(());

    // Item 67 named where a training cut's wait lives and could not name what it waits on. The census is
    // the measurement that half needs, and these drive the three door entry points the shipped hooks call.
    #[test]
    fn a_hole_census_counts_the_doors_crossed_while_its_window_stood_open() {
        let _turn = CENSUS_TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let outside = CutProbe::counted("census door crossed before the hole");
        let inside = CutProbe::counted("census door crossed three ways");

        outside.count();
        open_hole_census();
        inside.count();
        inside.observe(&[1.0]);
        inside.sample(0.5);
        let table = close_hole_census().expect("opening a hole installs its tally");

        assert_eq!(
            table.get("census door crossed three ways").map(|entry| entry.crossings),
            Some(3),
            "a door crossed through all three counting shapes counts three crossings"
        );
        assert!(
            !table.get("census door crossed three ways").is_some_and(|entry| entry.valued),
            "a counted door hands nothing worth a range, and claiming one would be a made up number"
        );
        assert!(
            !table.contains_key("census door crossed before the hole"),
            "a door crossed before the hole opened is not part of the hole"
        );

        let line = hole_census_line(6583, HOLE_END_NAMES[HOLE_END_CUT_IN], HOLE_END_NAMES[HOLE_END_PLAY_OUT], &table);
        assert!(line.contains("census door crossed three ways = 3"), "the census line names the door and its crossings: {line}");
        assert!(line.contains("over 6583 ms"), "the census line carries the hole it censuses: {line}");
        assert!(line.contains("from the training cut-in ending to the status play out"), "the census line names the stretch it censused: {line}");
        assert!(!line.contains("before the hole"), "the census line does not carry a door the hole never crossed: {line}");
    }

    // Item 70 needs more than crossings before it hands the game's own skip door an argument: it needs the
    // times the cut-in clock is being fed while the panel is held off. The peak a door keeps all run cannot
    // answer that, because the peak may have been set by a cut outside the hole.
    #[test]
    fn a_hole_census_keeps_the_lowest_and_highest_value_a_valued_door_handed() {
        let _turn = CENSUS_TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let clock = CutProbe::peaked("census clock door handed times");

        open_hole_census();
        clock.sample(3.0);
        clock.observe_peak(&[6.0], 6.0);
        clock.sample(1.5);
        clock.count();
        let table = close_hole_census().expect("opening a hole installs its tally");
        let entry = table.get("census clock door handed times").expect("the door was crossed inside the window");

        assert_eq!(entry.crossings, 4, "the valued shapes and the plain one all count as crossings");
        assert_eq!((entry.lowest, entry.highest), (1.5, 6.0), "the window keeps the spread it saw, not the run peak");
        assert!(entry.valued, "a door that handed a value is reported with one");

        let line = hole_census_line(3400, HOLE_END_NAMES[HOLE_END_PLAY_OUT], HOLE_END_NAMES[HOLE_END_CUT_IN], &table);
        assert!(line.contains("census clock door handed times = 4 (1.5 to 6 handed)"), "the census line prints the spread next to the crossings: {line}");
        assert!(line.contains("from the status play out to the training cut-in ending"), "the same census read the other way round names the stretch it actually censused: {line}");
    }

    #[test]
    fn cleaning_a_cut_closes_a_hole_census_that_never_reached_its_play_out() {
        let _turn = CENSUS_TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let leftover = CutProbe::counted("census door crossed after a cleaned cut");

        open_hole_census();
        close_cut_run();
        leftover.count();

        assert!(close_hole_census().is_none(), "cleaning a cut lowers the window, so nothing crossed after it is charged to a hole");
    }

    #[test]
    fn opening_a_second_hole_replaces_a_tally_nobody_closed() {
        let _turn = CENSUS_TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let stale = CutProbe::counted("census door of a hole a second cut-in replaced");

        open_hole_census();
        stale.count();
        open_hole_census();
        note_hole_census("census door of a hole a second cut-in replaced");
        let table = close_hole_census().expect("the second window carries its own tally");

        assert_eq!(
            table.get("census door of a hole a second cut-in replaced").map(|entry| entry.crossings),
            Some(1),
            "the replaced window is dropped rather than added to the new one"
        );
    }

    // C56 is the pairing defect: the clock only counted the hole when the cut-in ended before the status
    // panel played out, and runs 53 and 57 reached the two doors the other way round, so every 20 s report
    // printed `cut holes: 0 holes` beside a cut that ran 8,898 ms. These drive `pair_hole_open`, the
    // decision both shipped doors route through, at the timestamps those runs logged.
    #[test]
    fn a_cut_hole_pairs_whichever_end_of_it_the_game_reached_first() {
        // Run 15's order: `OnEndCutIn` at 08:17:42.935, `PlayOut` at 08:17:50.156.
        assert_eq!(pair_hole_open(-1, HOLE_END_CUT_IN, HOLE_END_CUT_IN, 42_935), None, "the first door opens the window rather than closing one");
        assert_eq!(pair_hole_open(42_935, HOLE_END_CUT_IN, HOLE_END_PLAY_OUT, 50_156), Some((7_221, HOLE_END_CUT_IN)), "the far door closes it");

        // Run 57's order: `PlayOut` at 20:44:22.847, `OnEndCutIn` at 20:44:24.247. Under the old pairing
        // this half reported nothing at all, which is the 0 holes the run printed.
        assert_eq!(pair_hole_open(-1, HOLE_END_PLAY_OUT, HOLE_END_PLAY_OUT, 22_847), None, "the first door opens the window whichever door it is");
        assert_eq!(pair_hole_open(22_847, HOLE_END_PLAY_OUT, HOLE_END_CUT_IN, 24_247), Some((1_400, HOLE_END_PLAY_OUT)), "and the other door closes it, named from the end that opened it");
    }

    #[test]
    fn one_cut_reports_one_hole_even_when_its_far_door_arrives_twice() {
        // Run 15 called `PlayOut` twice for one cut-in end. The mark is swapped out when it is read, so the
        // second call opens a fresh window instead of charging the same wait twice.
        assert_eq!(pair_hole_open(10_000, HOLE_END_CUT_IN, HOLE_END_PLAY_OUT, 17_950), Some((7_950, HOLE_END_CUT_IN)));
        assert_eq!(pair_hole_open(-1, HOLE_END_CUT_IN, HOLE_END_PLAY_OUT, 17_951), None, "a closed window has no mark left to pair with");
    }

    #[test]
    fn two_doors_on_the_same_end_of_a_hole_replace_each_other_instead_of_pairing() {
        // Two cut-in ends are not a hole, and neither are two play outs. The window moves to the later one
        // so a cut that never reached its far end is not measured across two cuts.
        assert_eq!(pair_hole_open(10_000, HOLE_END_CUT_IN, HOLE_END_CUT_IN, 18_000), None, "a cut-in end does not pair with a cut-in end");
        assert_eq!(pair_hole_open(18_000, HOLE_END_CUT_IN, HOLE_END_PLAY_OUT, 19_000), Some((1_000, HOLE_END_CUT_IN)), "and the window stands on the later mark");
    }

    #[test]
    fn a_hole_stretch_is_named_in_the_order_its_doors_were_reached() {
        assert_eq!(hole_stretch(HOLE_END_CUT_IN), ("the training cut-in ending", "the status play out"));
        assert_eq!(hole_stretch(HOLE_END_PLAY_OUT), ("the status play out", "the training cut-in ending"));
    }

    #[test]
    fn a_hole_mark_a_cut_teardown_left_behind_pairs_with_nothing() {
        let _turn = CENSUS_TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());

        // A status play out opened the window and the cut was cleaned before any cut-in end arrived. This
        // close counts as no cut run at all, and the reset still has to run: a mark standing across a
        // teardown would be read as the next turn's hole.
        CUT_HOLE_OPEN_MS.store(22_847, atomic::Ordering::Relaxed);
        CUT_HOLE_OPEN_END.store(HOLE_END_PLAY_OUT, atomic::Ordering::Relaxed);
        let holes_before = CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed);

        close_cut_run();

        assert_eq!(CUT_HOLE_OPEN_MS.load(atomic::Ordering::Relaxed), -1, "the teardown drops the mark rather than leaving it standing");
        assert_eq!(CUT_HOLE_OPEN_END.load(atomic::Ordering::Relaxed), HOLE_END_CUT_IN, "and it forgets which end made it");
        assert_eq!(
            pair_hole_open(CUT_HOLE_OPEN_MS.load(atomic::Ordering::Relaxed), CUT_HOLE_OPEN_END.load(atomic::Ordering::Relaxed), HOLE_END_CUT_IN, 92_148),
            None,
            "the next turn's doors do not pair across this teardown"
        );
        assert_eq!(CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed), holes_before, "a dropped mark is not a hole");
    }

    // The same two orders driven through `pair_hole_end`, the function both shipped doors call, so the
    // pairing is proven at the door and not only in the decision behind it.
    #[test]
    fn the_shipped_hole_doors_count_the_wall_in_the_order_the_game_reached_them() {
        let _turn = CENSUS_TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        CUT_HOLE_OPEN_MS.store(-1, atomic::Ordering::Relaxed);
        CUT_HOLE_OPEN_END.store(HOLE_END_CUT_IN, atomic::Ordering::Relaxed);
        close_hole_census();

        let holes_before = CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed);
        let total_before = CUT_HOLE_MS_TOTAL.load(atomic::Ordering::Relaxed);
        let cut_in_first_before = CUT_HOLE_END_RUNS[HOLE_END_CUT_IN].load(atomic::Ordering::Relaxed);
        let play_out_first_before = CUT_HOLE_END_RUNS[HOLE_END_PLAY_OUT].load(atomic::Ordering::Relaxed);

        // Run 57's turn: `TrainingCutStatus::PlayOut` at 20:44:22.847, `OnEndCutIn` at 20:44:24.247. Under
        // the pairing this build replaces, these two calls charged nothing at all.
        pair_hole_end(HOLE_END_PLAY_OUT, 22_847);
        pair_hole_end(HOLE_END_CUT_IN, 24_247);

        assert_eq!(CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed), holes_before + 1, "the turn has a hole and the probe counts it");
        assert_eq!(CUT_HOLE_MS_TOTAL.load(atomic::Ordering::Relaxed) - total_before, 1_400, "and it is priced as the stretch the two doors name");
        assert_eq!(CUT_HOLE_END_RUNS[HOLE_END_PLAY_OUT].load(atomic::Ordering::Relaxed), play_out_first_before + 1, "filed under the order this cut really walked");
        assert_eq!(CUT_HOLE_OPEN_MS.load(atomic::Ordering::Relaxed), -1, "a closed pairing leaves no mark standing");

        // Run 15's turn: `OnEndCutIn` at 08:17:42.935, `PlayOut` at 08:17:50.156.
        pair_hole_end(HOLE_END_CUT_IN, 42_935);
        pair_hole_end(HOLE_END_PLAY_OUT, 50_156);

        assert_eq!(CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed), holes_before + 2, "the older order still counts, as it always did");
        assert_eq!(CUT_HOLE_MS_TOTAL.load(atomic::Ordering::Relaxed) - total_before, 1_400 + 7_221, "both orders price the stretch they name");
        assert_eq!(CUT_HOLE_END_RUNS[HOLE_END_CUT_IN].load(atomic::Ordering::Relaxed), cut_in_first_before + 1);
        assert!(CUT_HOLE_WORST_MS.load(atomic::Ordering::Relaxed) >= 7_221, "the worst hole is the longest of the two stretches");
    }

    // C54 asked what paces the group typewrite path and answered it for a whole run off peak values; C62
    // answered what the interval costs and answered it for a whole run. These drive the per cascade reading.
    #[test]
    fn a_cascade_beat_is_the_wall_between_its_first_and_last_typewrite_start() {
        // Run 57 cascade 3 at the three plate starts its door logging printed, which stops after
        // `PROBE_DETAIL_LIMIT` calls (20:44:34.532, 34.999 and 35.166). The run's own count puts four
        // `StartTypewrite` calls inside that cascade, so this is a three plate record of it, not the
        // whole cascade: the number a replay has to read off the run is the span, not this beat.
        let cascade = CascadeRead {
            plates: 3,
            first_start_ms: 34_532,
            last_start_ms: 35_166,
            ..CascadeRead::default()
        };

        assert_eq!(cascade.beat_ms(), Some(317), "the mean step from one plate start to the next");

        // One plate types nothing after it, and a cascade that never started one has no beat to report.
        assert_eq!(CascadeRead { plates: 1, first_start_ms: 100, last_start_ms: 100, ..CascadeRead::default() }.beat_ms(), None);
        assert_eq!(CascadeRead::default().beat_ms(), None);
    }

    #[test]
    fn a_cascade_names_the_number_its_plate_steps_land_on() {
        // Run 57 cascade 3, at the two plate steps its door lines price. `StartTypewrite` calls 6, 7 and 8
        // are stamped 20:44:34.53229, 34.99975 and 35.16670 (`:1738`/`:1743`/`:1748`), so its two steps are
        // 467 ms and 167 ms to the millisecond - the probe's own clock reads the same pair within a
        // millisecond, which is the drift `a_cascade_span_is_the_probes_clock_not_the_difference_of_two_log_stamps`
        // measures on the spans. The caller hands 1.0 s while the UI holds `_groupInterval` and
        // `_sequenceInterval` at 0.1666665 (its plate list line, `:1736`). The 167 ms step is that beat; the
        // second the caller handed is not a step anyone took.
        let mut cascade = cascade_with_steps(&[467, 167]);
        cascade.interval = 1.0;
        cascade.group_interval = 0.1666665;
        cascade.sequence_interval = 0.1666665;

        assert_eq!(cascade_pace(&cascade), (3, 1), "the two numbers the class holds here are equal, so the report names the one it carries first and the step count says only one step landed on it");

        // The same step with the group interval out of the running names the sequence interval, which is the
        // reading the line is for.
        let mut sequence_only = cascade_with_steps(&[167]);
        sequence_only.interval = 1.0;
        sequence_only.group_interval = 0.5;
        sequence_only.sequence_interval = 0.1666665;

        assert_eq!(cascade_pace(&sequence_only), (4, 1), "_sequenceInterval when it is the number the step lands on");

        // Run 57 cascade 5, at the one plate step that run prices for it. Its 5,106 ms span is the worst of
        // the run's five cascades (`run log/hachimi-run57.log:1879`), and the same sentence quotes the run's
        // worst plate-to-plate step, 4,220 ms. That step is one of this cascade's, and the record below
        // carries no other: `run_57s_cascade_5_says_none_for_every_plate_step_its_reports_price` derives from
        // the reports at `:1834` and `:1879` that the six steps this cascade added total 4,287 ms or 4,288 ms,
        // so the five steps its door never printed share 67 ms or 68 ms between them. Those five are not
        // printed at all - `StartTypewrite` call lines stop at call 8, `PROBE_DETAIL_LIMIT`, and this
        // cascade's calls are the run's 14th to 20th - so they are left out of the record rather than filled
        // in with numbers the run did not print. What the class held is this cascade's own plate list line
        // (`:1864`): interval 1.0, `_delay` 0, `_tapWait` 0, `_groupInterval` and `_sequenceInterval`
        // 0.1666665. Nothing the class holds prices a 4,220 ms step, so the line says none rather than naming
        // the nearest number.
        let mut stalled = cascade_with_steps(&[4_220]);
        stalled.interval = 1.0;
        stalled.group_interval = 0.1666665;
        stalled.sequence_interval = 0.1666665;

        assert_eq!(cascade_pace(&stalled), (PACE_NONE, 0), "a cascade waiting on a number the class does not carry says none");
        assert!(fifth_cascade_carries_only_its_runs_steps(&stalled), "and a record naming this cascade carries only the plate steps run 57 prices for it");

        // The same answer for a cascade that read more than one step and priced none of them. Built for that
        // shape, not a run's numbers: run 57 supports exactly one step of cascade 5, and inventing a second
        // one to make the shape is the mistake this test's data has to stay clear of.
        let mut built = cascade_with_steps(&[1_350, 3_900]);
        built.delay = 0.5;
        built.tap_wait = 2.0;

        assert_eq!(cascade_pace(&built), (PACE_NONE, 0), "two steps that land on no number the class holds say none together, not a nearest number each");

        // A number the UI holds at 0 paces nothing, so it is never named as the answer.
        let mut tap_waited = cascade_with_steps(&[2_100]);
        tap_waited.tap_wait = 2.1;

        assert_eq!(cascade_pace(&tap_waited), (2, 1), "_tapWait is named when the cascade really is waiting on it");
    }

    // Builds the record one cascade's doors produced, with the plate steps they measured.
    fn cascade_with_steps(steps: &[i64]) -> CascadeRead {
        let mut cascade = CascadeRead { plates: steps.len() + 1, ..CascadeRead::default() };

        for (slot, step_ms) in steps.iter().enumerate().take(CASCADE_BEAT_LIMIT) {
            cascade.beats[slot] = *step_ms;
        }

        cascade.beats_seen = steps.len().min(CASCADE_BEAT_LIMIT);
        cascade
    }

    #[test]
    fn a_cascade_line_prices_its_pass_entry_to_completion_and_names_its_pacing() {
        let mut cascade = cascade_with_steps(&[167]);
        cascade.index = 3;
        cascade.open_ms = 34_527;
        cascade.close_ms = 35_732;
        cascade.closed_by = "its typewrite end";
        cascade.interval = 1.0;
        cascade.group_interval = 0.5;
        cascade.sequence_interval = 0.1666665;
        cascade.groups = 2;
        cascade.advances = 3;
        cascade.write_ms = 2;
        cascade.write_runs = 2;
        cascade.wait_ms = 500;
        cascade.wait_runs = 2;

        let line = cascade_line(&cascade);

        assert!(line.contains("plate cascade 3 closed 1205 ms from its plate list call to its typewrite end"), "the pass is entry to completion, not entry to the next entry: {line}");
        assert!(line.contains("held interval 1.0000 s _delay 0.0000 _tapWait 0.0000 _groupInterval 0.5000 s _sequenceInterval 0.1667 s"), "the numbers the class holds are printed next to the interval it was handed: {line}");
        assert!(line.contains("the plates advance on TrainingParamChangeUI._sequenceInterval of 0.1667 s, in 1 of 1 plate step(s)"), "the verdict names the number the cascade waits on: {line}");
        assert!(line.contains("type write leg mean 1.0 ms and wait to the next advance mean 250.0 ms"), "the legs the two unreachable constants own are measured off the doors: {line}");
    }

    // C56 is also a reporting rule, and it is the one a replay breaks on: a cascade's span is what this
    // module measured at its two doors, and a difference between the stamps of those doors' log lines is
    // not that span. Run 57 printed its cascade line 127 times over 20 s reports, at closed counts 0 twice
    // (`run log/hachimi-run57.log:1309`, `:1483`), 2 once (`:1687`), 3 twice (`:1771`, `:1793`), 4 once
    // (`:1834`) and 5 121 times (from `:1879`, `:3669` among them) - never once at 1 closed - and only the
    // four readings below have a cascade in them to price: 2 closed mean 1276.0 worst 1314, 3 mean 1252.3
    // worst 1314, 4 mean 1396.5 worst 1829, 5 mean 2138.4 worst 5106.
    const RUN57_CASCADE_PRINTINGS: [(usize, usize); 5] = [(0, 2), (2, 1), (3, 2), (4, 1), (5, 121)];
    const RUN57_CASCADE_READINGS: [(usize, &str, i64); 4] = [(2, "1276.0", 1_314), (3, "1252.3", 1_314), (4, "1396.5", 1_829), (5, "2138.4", 5_106)];

    /// Every whole-millisecond total that formats to the mean a report printed. The mean is `mean_of` over
    /// whole ms spans, so a printed mean prices its total to within half a tenth of a millisecond per
    /// reading: one total usually answers it, and that is what makes a cascade's span readable off
    /// successive reports. Where the count does not divide evenly the run is not readable more exactly than
    /// the set this returns, so the whole set is kept rather than whichever member is convenient.
    fn totals_a_mean_prices(count: usize, mean_text: &str) -> Vec<i64> {
        let near = mean_text.parse::<f64>().expect("the report prints a number") * count as f64;
        let half_window = count as f64 / 20.0 + 1.0;

        ((near - half_window).floor() as i64..=(near + half_window).ceil() as i64).filter(|total| format!("{:.1}", mean_of(*total, count)) == mean_text).collect()
    }

    /// The one total a printed mean prices, for the readings whose count divides it cleanly.
    fn cascade_total_a_mean_prices(count: usize, mean_text: &str) -> i64 {
        let priced = totals_a_mean_prices(count, mean_text);

        assert_eq!(priced.len(), 1, "the mean {mean_text} over {count} closed cascades prices {priced:?}");

        priced[0]
    }

    /// The readings of run 57 a set of spans fails, each said the way `report` prints it. Empty says the set
    /// reproduces the run. A span enters the fold the way the shipped close charges it - as the span the
    /// guard answers for a window's two clock reads - and the pair of numbers compared is the pair the
    /// shipped line prints: `mean_of` over the spans closed so far, and the worst of them.
    fn cascade_readings_a_set_fails(spans: &[i64]) -> Vec<String> {
        let mut failed = Vec::new();
        let mut closed = 0usize;
        let mut total_ms = 0i64;
        let mut worst_ms = 0i64;

        for (index, span_ms) in spans.iter().copied().enumerate() {
            let Some(span_ms) = span_from(0, span_ms) else {
                failed.push(format!("cascade {} closed before it opened", index + 1));
                continue;
            };

            closed += 1;
            total_ms += span_ms;
            worst_ms = worst_ms.max(span_ms);

            for (count, mean_text, worst_text) in RUN57_CASCADE_READINGS {
                if count != closed {
                    continue;
                }

                let mean = format!("{:.1}", mean_of(total_ms, closed));

                if mean != mean_text {
                    failed.push(format!("{closed} cascades closed mean {mean} ms, the run printed {mean_text} ms"));
                }

                if worst_ms != worst_text {
                    failed.push(format!("{closed} cascades closed worst {worst_ms} ms, the run printed {worst_text} ms"));
                }
            }
        }

        failed
    }

    #[test]
    fn run_57s_five_cascade_replay_prices_the_spans_its_readings_allow_and_its_first_two_as_one_sum() {
        const RUN57_CASCADE_SPANS_MS: [i64; 5] = [1_314, 1_238, 1_205, 1_829, 5_106];

        let failed = cascade_readings_a_set_fails(&RUN57_CASCADE_SPANS_MS);

        assert!(failed.is_empty(), "the five spans this module measured reproduce every cascade report run 57 printed, failed {failed:?}");
        assert_eq!(RUN57_CASCADE_SPANS_MS.iter().sum::<i64>(), 10_692, "a mean of 2138.4 over 5 closed cascades prices 10,692 ms in total");
        assert_eq!(RUN57_CASCADE_SPANS_MS.iter().copied().max(), Some(5_106), "and the worst the same sentence quotes is one of the five, not a rounding of a stamp");

        // What the run's readings do pin down: each mean prices exactly one running total, and a span is the
        // difference of two successive totals wherever exactly one cascade closed between the two readings.
        // That holds for cascades 3, 4 and 5 here and for nothing earlier. No reading of this run had one
        // cascade closed, so the first two arrive as a 2,552 ms sum, and quoting them as 1,314 and 1,238
        // borrows the printed worst. `run_57s_cascade_readings_do_not_name_which_of_its_first_two_cascades_was_the_worst`
        // is these same four readings with that pair swapped, and it reproduces every one of them. The pair
        // is priced here as the set its readings allow - the sum and the max - and never as two spans a
        // replay could read off run 57: that build printed no span tail and no per cascade line at all.
        let totals: Vec<i64> = RUN57_CASCADE_READINGS.iter().map(|&(count, mean_text, _)| cascade_total_a_mean_prices(count, mean_text)).collect();

        assert_eq!(totals, [2_552, 3_757, 5_586, 10_692], "the four means price four running totals");
        assert_eq!(RUN57_CASCADE_SPANS_MS[2], totals[1] - totals[0], "cascade 3 is a difference of two totals, the 1,205 ms its door lines also bracket (20:44:34.52773 to 35.73246)");
        assert_eq!(RUN57_CASCADE_SPANS_MS[3], totals[2] - totals[1], "cascade 4 likewise, and it is also the worst the run printed at four closed");
        assert_eq!(RUN57_CASCADE_SPANS_MS[4], totals[3] - totals[2], "and cascade 5, the run's worst, at 5,106 ms");
        assert_eq!(RUN57_CASCADE_SPANS_MS[..2].iter().sum::<i64>(), totals[0], "the first two the aggregate leaves as their sum, which is all it says about them");
        assert_eq!(RUN57_CASCADE_SPANS_MS[..2].iter().copied().max(), Some(RUN57_CASCADE_READINGS[0].2), "and the 1,314 ms printed as the worst with two closed is the larger of that pair - a premise about which cascade the max belonged to, not a reading off the line");
        assert!(RUN57_CASCADE_SPANS_MS[..4].iter().copied().max().unwrap_or_default() < RUN57_WORST_PLATE_STEP_MS, "every cascade but the fifth spans less than the run's worst plate-to-plate step, so that 4,220 ms step sits inside cascade 5");

        // The run's worst cascade through the shipped per cascade line. That line only ever prints
        // `close_ms - open_ms` off the probe's own clock, so the pair here is the measured difference, not
        // run 57's absolute clock, and it is the line the next census run has to print - at debug level, for
        // the first `PROBE_DETAIL_LIMIT` cascades of the session - for a replay to be read rather than
        // solved. Its span is also on the report's span tail, which every cascade gets. The record carries
        // the one plate step this run prices for cascade 5; the other five are derived only as a bound, in
        // `run_57s_cascade_5_says_none_for_every_plate_step_its_reports_price`, and never as numbers.
        let mut fifth = cascade_with_steps(&[RUN57_WORST_PLATE_STEP_MS]);
        fifth.index = 5;
        fifth.open_ms = 0;
        fifth.close_ms = 5_106;
        fifth.closed_by = "its typewrite end";

        let line = cascade_line(&fifth);

        assert!(line.contains("plate cascade 5 closed 5106 ms from its plate list call to its typewrite end"), "the line says 5,106 ms, the figure the run's own report quotes: {line}");
        assert!(line.contains("the plates advance on none of these numbers (1 plate step(s) read)"), "and 5,106 ms is still priced by none of the numbers the class holds: {line}");
    }

    #[test]
    fn a_cascade_span_is_the_probes_clock_not_the_difference_of_two_log_stamps() {
        // Run 57's five cascades at the stamps of the door lines that open and close them, in ms from
        // 20:44:00.000: 8.0325018 -> 9.3470794, 12.657871 -> 13.8956125, 34.5277363 -> 35.7324691,
        // 67.5799303 -> 69.4083793 and 92.6520255 -> 97.7574584 (`run log/hachimi-run57.log:1535`/`:1566`,
        // `:1575`/`:1586`, `:1736`/`:1753`, `:1801`/`:1804`, `:1864`/`:1868`).
        let stamp_pairs = [(8_032, 9_347), (12_657, 13_895), (34_527, 35_732), (67_579, 69_408), (92_652, 97_757)];
        let stamped: Vec<i64> = stamp_pairs.iter().map(|&(open_ms, close_ms)| span_from(open_ms, close_ms).unwrap_or_default()).collect();

        // Run the stamps through the same guard the shipped close uses and the numbers come out as the
        // replay that was written for the run - which is the mistake, not the run.
        assert_eq!(stamped, [1_315, 1_238, 1_205, 1_829, 5_105], "truncated logger stamps give the set the replay used");

        let failed = cascade_readings_a_set_fails(&stamped);

        assert_eq!(stamped.iter().sum::<i64>(), 10_692, "its five members do add to the total the run's last mean prices, which is why the total alone never caught it");
        assert_eq!(failed.len(), 6, "and the set fails {failed:?} of the readings the same run printed");

        // Moving the error onto cascade 4 to keep a worst of 5,106 is no better a replay. The run printed a
        // worst of 1,829 ms with four cascades closed, so none of those four is 1,828 ms, and 1,315 ms is
        // still above the 1,314 ms it printed with only two closed.
        let shifted = [1_315, 1_238, 1_205, 1_828, 5_106];
        let shifted_failed = cascade_readings_a_set_fails(&shifted);

        assert_eq!(shifted.iter().sum::<i64>(), 10_692, "this one prices the right total too");
        assert_eq!(shifted_failed.len(), 5, "and it fails {shifted_failed:?} of the run's readings, two cascades more than the set above");
    }

    #[test]
    fn the_cascade_line_of_run_57_was_printed_at_every_report_and_never_with_one_cascade_closed() {
        // The line is printed by every report, not once per cascade, so a replay works off the readings the
        // run left and not the ones it would like. Run 57's cascade line printed 127 times at the closed
        // counts the census above names, and the four readings quoted in the replay are four of those
        // printings. A span falls out of the mean and the worst as a difference of two totals only where
        // exactly one cascade closed between the two readings, and no reading of this run had one cascade
        // closed: its first two cascades land between the reading at 0 closed and the one at 2.
        let closed_counts: Vec<usize> = RUN57_CASCADE_PRINTINGS.iter().map(|&(count, _)| count).collect();
        let printings: usize = RUN57_CASCADE_PRINTINGS.iter().map(|&(_, times)| times).sum();
        let read_counts: Vec<usize> = RUN57_CASCADE_READINGS.iter().map(|&(count, _, _)| count).collect();

        assert_eq!(printings, 127, "how many times the cascade line printed in run 57, not the four times a replay quotes");
        assert_eq!(closed_counts, [0, 2, 3, 4, 5], "the closed counts it printed, in the order the log shows them");
        assert!(!closed_counts.contains(&1), "no reading of the run had exactly one cascade closed, which is the only shape a span is a difference of two totals in");
        assert_eq!(closed_counts[0] + 2, closed_counts[1], "two cascades closed between the first two readings, so the second priced them as one sum");
        assert_eq!(read_counts, [2, 3, 4, 5], "the readings a replay quotes are one at each count that had a cascade in it");
    }

    #[test]
    fn run_57s_cascade_readings_do_not_name_which_of_its_first_two_cascades_was_the_worst() {
        const WORST_FIRST: [i64; 5] = [1_314, 1_238, 1_205, 1_829, 5_106];
        const WORST_SECOND: [i64; 5] = [1_238, 1_314, 1_205, 1_829, 5_106];

        assert_ne!(WORST_FIRST, WORST_SECOND, "cascade 1 and cascade 2 swapped, which is the one thing the run's readings leave free about them");
        assert_eq!(WORST_FIRST.iter().sum::<i64>(), WORST_SECOND.iter().sum::<i64>(), "and the same total, which is why the totals alone never caught the reading that was never taken");
        assert!(cascade_readings_a_set_fails(&WORST_SECOND).is_empty(), "the swapped pair reproduces every cascade reading run 57 printed");

        let printed = aggregate_readings_a_set_prints(&WORST_FIRST, &[2, 3, 4, 5]);
        let logged: Vec<(String, i64)> = RUN57_CASCADE_READINGS.iter().map(|&(_, mean_text, worst_ms)| (mean_text.to_string(), worst_ms)).collect();

        assert_eq!(printed, logged, "the four readings folded out of these spans are the four the log holds");
        assert_eq!(printed, aggregate_readings_a_set_prints(&WORST_SECOND, &[2, 3, 4, 5]), "and folding the swapped pair gives the same four lines, mean and worst alike, so the aggregate is not what picked an order out");

        // The span tail does say it, because it is written at the close rather than read off the aggregate.
        let first_tail = ring_of(&WORST_FIRST).span_tail(0);
        let second_tail = ring_of(&WORST_SECOND).span_tail(0);

        assert!(first_tail.contains(" 1=1314 ms 2=1238 ms"), "{first_tail}");
        assert!(second_tail.contains(" 1=1238 ms 2=1314 ms"), "{second_tail}");
        assert_ne!(first_tail, second_tail, "the line with the spans on it tells the two runs apart: {first_tail} against {second_tail}");
    }

    #[test]
    fn cascades_closing_between_readings_leave_their_sum_and_nothing_else_in_the_aggregate() {
        // The shape the old sentence generalised from, carried past the point it works in: two cascades close
        // before the first reading, three between the first reading and the second, and none of those three
        // reaches the 1,700 ms the first two already printed as the worst. The second reading prices three
        // more cascades out of a total it cannot cut up, and its worst belongs to a cascade that closed
        // before them.
        const SHALLOW: [i64; 5] = [1_700, 300, 900, 400, 300];
        const DEEP: [i64; 5] = [1_700, 300, 800, 500, 300];

        assert_ne!(SHALLOW[2..], DEEP[2..], "three cascades measured differently");
        assert_eq!(SHALLOW[2..].iter().sum::<i64>(), DEEP[2..].iter().sum::<i64>(), "with the same total");
        assert!(SHALLOW[2..].iter().chain(DEEP[2..].iter()).all(|span| *span < 1_700), "and none of them is the worst either reading prints, so the worst says nothing about them");
        assert_eq!(aggregate_readings_a_set_prints(&SHALLOW, &[2, 5]), aggregate_readings_a_set_prints(&DEEP, &[2, 5]), "two readings that a replay has at hand are the same line for both runs");

        let shallow_tail = ring_of(&SHALLOW).span_tail(0);
        let deep_tail = ring_of(&DEEP).span_tail(0);

        assert!(shallow_tail.contains(" 3=900 ms 4=400 ms 5=300 ms"), "{shallow_tail}");
        assert!(deep_tail.contains(" 3=800 ms 4=500 ms 5=300 ms"), "{deep_tail}");
        assert_ne!(shallow_tail, deep_tail, "and the spans the line prints say which run the log is holding: {shallow_tail} against {deep_tail}");
    }

    #[test]
    fn the_cascade_line_still_prints_the_aggregate_half_of_every_line_run_57_logged() {
        // The five cascade lines run 57 logged (`run log/hachimi-run57.log:1309`, `:1687`, `:1771`, `:1834`,
        // `:1879`), each rebuilt from the totals its own printed means price. The change set adds a tail to
        // these lines; the half a replay already reads is held unchanged, byte for byte, by this test.
        let logged: [(&str, usize, &str, i64, usize, &str, i64); 5] = [
            ("Cutt probe plate cascade: 0 cascades closed mean 0.0 ms worst 0 ms, typewrite starts 0 inside a cascade mean 0.0 ms worst 0 ms", 0, "0.0", 0, 0, "0.0", 0),
            ("Cutt probe plate cascade: 2 cascades closed mean 1276.0 ms worst 1314 ms, typewrite starts 3 inside a cascade mean 244.3 ms worst 433 ms", 2, "1276.0", 1_314, 3, "244.3", 433),
            ("Cutt probe plate cascade: 3 cascades closed mean 1252.3 ms worst 1314 ms, typewrite starts 6 inside a cascade mean 266.8 ms worst 468 ms", 3, "1252.3", 1_314, 6, "266.8", 468),
            ("Cutt probe plate cascade: 4 cascades closed mean 1396.5 ms worst 1829 ms, typewrite starts 9 inside a cascade mean 229.8 ms worst 468 ms", 4, "1396.5", 1_829, 9, "229.8", 468),
            ("Cutt probe plate cascade: 5 cascades closed mean 2138.4 ms worst 5106 ms, typewrite starts 15 inside a cascade mean 423.7 ms worst 4220 ms", 5, "2138.4", 5_106, 15, "423.7", 4_220),
        ];

        for (text, closed, closed_mean, closed_worst, steps, steps_mean, steps_worst) in logged {
            let closed_total = if closed == 0 { 0 } else { totals_a_mean_prices(closed, closed_mean)[0] };
            let steps_total = if steps == 0 { 0 } else { totals_a_mean_prices(steps, steps_mean)[0] };
            let line = plate_cascade_line((closed, closed_total, closed_worst), (steps, steps_total, steps_worst), &ring_of(&[]), 0);

            assert_eq!(line, text, "the shipped line with no cascade closed since its reading, for the reading at {closed} closed");
        }
    }

    #[test]
    fn the_cascade_line_names_every_span_that_closed_between_its_readings() {
        const RUN57_CASCADE_SPANS_MS: [i64; 5] = [1_314, 1_238, 1_205, 1_829, 5_106];
        let ring = ring_of(&RUN57_CASCADE_SPANS_MS);

        // Read at the boundaries run 57 actually had, each with the ring that reading found in front of it.
        // The first reading that named any cascade named two at once, and it named them as two numbers rather
        // than as the 2,552 ms sum the aggregate left behind.
        let first = ring_of(&RUN57_CASCADE_SPANS_MS[..2]).span_tail(0);
        let second = ring_of(&RUN57_CASCADE_SPANS_MS[..3]).span_tail(2);
        let third = ring_of(&RUN57_CASCADE_SPANS_MS[..4]).span_tail(3);
        let fourth = ring_of(&RUN57_CASCADE_SPANS_MS[..5]).span_tail(4);

        assert_eq!(first, " cascades closed since the last report: 1=1314 ms 2=1238 ms", "the report run 57 had at two closed gives both cascades");
        assert_eq!(second, " cascades closed since the last report: 3=1205 ms", "{second}");
        assert_eq!(third, " cascades closed since the last report: 4=1829 ms", "{third}");
        assert_eq!(fourth, " cascades closed since the last report: 5=5106 ms", "{fourth}");

        let named: Vec<i64> = [&first, &second, &third, &fourth]
            .iter()
            .flat_map(|tail| tail.split_whitespace())
            .filter_map(|piece| piece.split_once('=').map(|(_, span)| span))
            .map(|span| span.parse::<i64>().expect("the tail prints whole milliseconds"))
            .collect();

        assert_eq!(named, RUN57_CASCADE_SPANS_MS, "the four readings name the five spans in close order, which is the replay the old sentence promised and the aggregate could not deliver");
        assert_eq!(named.iter().sum::<i64>(), cascade_total_a_mean_prices(5, "2138.4"), "and they add up to the total that run's last mean prices, so the tail invented nothing");

        // The shipped line, tail on it, aggregate half unchanged.
        let line = plate_cascade_line((5, 10_692, 5_106), (15, 6_356, 4_220), &ring, 4);

        assert!(line.starts_with("Cutt probe plate cascade: 5 cascades closed mean 2138.4 ms worst 5106 ms, typewrite starts 15 inside a cascade mean 423.7 ms worst 4220 ms"), "the run's line, as the run printed it: {line}");
        assert!(line.ends_with(" cascades closed since the last report: 5=5106 ms"), "{line}");
    }

    #[test]
    fn a_cascade_line_with_nothing_new_in_it_carries_no_spans() {
        let ring = ring_of(&[1_314, 1_238]);

        assert_eq!(ring.span_tail(2), "", "only four of run 57's 127 readings had a close no earlier reading had named; the other 123 carry no tail and cost nothing");
        assert_eq!(plate_cascade_line((2, 2_552, 1_314), (3, 733, 433), &ring, 2), "Cutt probe plate cascade: 2 cascades closed mean 1276.0 ms worst 1314 ms, typewrite starts 3 inside a cascade mean 244.3 ms worst 433 ms", "the line a build before this change set printed, exactly, when nothing closed since the last reading");
    }

    // The lines the write ups hand to the human, re-derived from the format strings the shipped report builds
    // them from. Two were quoted as the shipped build does not say them: the hole line ending `from the
    // training cut-in ending to the status play out` is the wording run 16 through run 57 printed and the
    // pairing repair replaced it, and the cascade line was quoted as its aggregate half alone, which is the
    // line this build prints only when nothing closed since its reading. A run cannot close C56, C54 or C62
    // on a sentence its own build never prints, so each line is pinned in full here - label, fields, order.
    #[test]
    fn the_cut_hole_line_c56_is_owed_is_the_both_orders_line_the_shipped_report_prints() {
        let line = hole_line(2, 14_127, 7_881, &[1, 1]);

        assert_eq!(line, "Cutt probe cut holes: 2 holes mean 7063.5 ms worst 7881 ms between the training cut-in ending and the status play out, 1 from the cut-in ending to the play out, 1 from the play out to the cut-in ending", "{line}");

        // The ending the write up still quotes is not in this sentence any more. A run that went looking for
        // it found nothing, which is how C56 stayed open on a run that did read the line.
        assert!(!line.contains(" ms from the training cut-in ending to the status play out"), "the ending an earlier report owed is not a wording this build prints: {line}");

        // Run 57's door order, driven through the shipped pairing at the stamps that run logged: the panel
        // played out at 20:44:22.847 and the cut-in ended 1,400 ms later, at 20:44:24.247. The window stands
        // on the play out end, so the orders name it, and the pairing prices the wait the build that run
        // played left unpriced: that build printed `0 holes mean 0.0 ms worst 0 ms` beside a cut of 8,898 ms.
        assert_eq!(pair_hole_open(-1, HOLE_END_PLAY_OUT, HOLE_END_PLAY_OUT, 22_847), None, "the panel opens the window");
        assert_eq!(pair_hole_open(22_847, HOLE_END_PLAY_OUT, HOLE_END_CUT_IN, 24_247), Some((1_400, HOLE_END_PLAY_OUT)), "and the later cut-in end closes it, named from the end that opened it");

        let held = hole_line(1, 1_400, 1_400, &[0, 1]);

        assert_eq!(held, "Cutt probe cut holes: 1 holes mean 1400.0 ms worst 1400 ms between the training cut-in ending and the status play out, 0 from the cut-in ending to the play out, 1 from the play out to the cut-in ending", "the same session, on the line this build prints: {held}");
    }

    #[test]
    fn the_plate_cascade_line_c54_and_c62_are_owed_carries_the_span_it_owes_after_the_aggregate_half() {
        const RUN57_CASCADE_SPANS_MS: [i64; 5] = [1_314, 1_238, 1_205, 1_829, 5_106];
        let ring = ring_of(&RUN57_CASCADE_SPANS_MS);
        let line = plate_cascade_line((5, 10_692, 5_106), (15, 6_356, 4_220), &ring, 4);

        assert_eq!(line, "Cutt probe plate cascade: 5 cascades closed mean 2138.4 ms worst 5106 ms, typewrite starts 15 inside a cascade mean 423.7 ms worst 4220 ms cascades closed since the last report: 5=5106 ms", "the whole line as the shipped report prints it at the reading run 57 ended on: {line}");

        // What the line is owed for, and the half a replay has to read it for: the aggregate half prices a
        // running total and a max, and the tail is where a span is named. A quote that stops at the aggregate
        // half asks a run for the line of a reading at which no cascade closed, and hands it nothing to read
        // a span off.
        assert!(line.contains(" cascades closed since the last report: "), "a cascade line with a span in it is a cascade line with this tail on it: {line}");
    }

    #[test]
    fn the_cut_kinds_line_named_beside_the_cascade_line_carries_the_probe_label_and_the_self_report() {
        // Run 57's reading, kind name and all (`TAG_NAMES` spells them: "tag answer unknown", "friendship
        // cut", "regular cut"): the run closed one friendship cut of 8,898 ms and its cut-in timeline never
        // stepped. The line it printed, at `run log/hachimi-run57.log:1773` through `:3716`, is exactly this.
        let line = cut_kinds_line(" friendship cut runs 1 wall 8898 ms", 0, 0, 0);

        assert_eq!(line, "Cutt probe cut kinds: friendship cut runs 1 wall 8898 ms timeline self report total frames peak 0 last frame peak 0 target fps 0", "the pair C54 is closed on, in full: {line}");
        assert!(line.starts_with("Cutt probe cut kinds:"), "the line a write up calls `cut kinds:` carries the probe label in front of it: {line}");
    }

    #[test]
    fn the_per_cascade_line_c54_and_c62_are_owed_is_said_in_full_at_debug_for_the_first_eight_cascades() {
        let mut cascade = cascade_with_steps(&[167]);
        cascade.index = 3;
        cascade.open_ms = 34_527;
        cascade.close_ms = 35_732;
        cascade.closed_by = "its typewrite end";
        cascade.groups = 2;
        cascade.advances = 3;
        cascade.interval = 1.0;
        cascade.group_interval = 0.5;
        cascade.sequence_interval = 0.1666665;
        cascade.write_ms = 2;
        cascade.write_runs = 2;
        cascade.wait_ms = 500;
        cascade.wait_runs = 2;

        assert_eq!(cascade_line(&cascade), "Cutt probe plate cascade 3 closed 1205 ms from its plate list call to its typewrite end: 2 plate start(s), no step between them, 2 group start(s), 3 advance(s), 0 tap screen(s); the UI held interval 1.0000 s _delay 0.0000 _tapWait 0.0000 _groupInterval 0.5000 s _sequenceInterval 0.1667 s; type write leg mean 1.0 ms and wait to the next advance mean 250.0 ms; the plates advance on TrainingParamChangeUI._sequenceInterval of 0.1667 s, in 1 of 1 plate step(s)", "the per cascade line as `close_plate_pass` prints it: {}", cascade_line(&cascade));

        // The limit that belongs on the line a run is told to look for. Run 15 closed 17 cascades, so under
        // this cap its ninth to seventeenth never got one; their spans were on the report's aggregate, which
        // prices no span of its own, so those cascades were never readable per cascade at all. The next census
        // run reads the tail for every span and this line for the first `PROBE_DETAIL_LIMIT` pace answers.
        assert_eq!(PROBE_DETAIL_LIMIT, 8, "the per cascade line prints for eight cascades of a session, and that is the number a run that needs a ninth one has to plan around");
    }

    #[test]
    fn the_span_ring_holds_the_last_closes_and_says_which_spans_it_dropped() {
        // Twenty cascades close between two readings on a ring that holds sixteen, driven through the writer
        // the shipped close uses and the reader the shipped report uses. The reading names the last sixteen in
        // close order and says four are gone, so a replay can tell a partial census from a whole one.
        let spans: Vec<i64> = (0..CASCADE_SPAN_RING + 4).map(|index| 1_000 + 10 * index as i64).collect();

        for (index, span_ms) in spans.iter().copied().enumerate() {
            record_cascade_span(index + 1, span_ms);
        }

        let ring = cascade_span_ring();
        let tail = ring.span_tail(0);

        assert_eq!(ring.writes, spans.len(), "one span recorded per cascade close");
        assert_eq!(ring.indexes[..4], [17, 18, 19, 20], "the ring wrapped: its oldest slots now hold the newest closes");
        assert_eq!(ring.indexes[4], 5, "and the slot after them holds the oldest span still kept");
        assert!(tail.contains(" cascades closed since the last report: 5=1040 ms"), "and the tail still reads them in close order: {tail}");
        assert!(tail.contains(" 16=1150 ms 17=1160 ms"), "straight across the wrap: {tail}");
        assert!(tail.contains(" 20=1190 ms"), "{tail}");
        assert!(tail.contains(" (the last 16 of 20 closes, 4 older spans no longer held)"), "and says what it cannot hold: {tail}");
        assert!(!tail.contains(" 1=1000 ms"), "the four spans older than the ring are named as missing, not quietly dropped: {tail}");
    }

    // The same rule read off a log that does have a one cascade boundary, so the rule is not only a limit.
    // Run 31 printed the cascade line 86 times, at closed counts 0 (`run log/hachimi-run31.log:958`), 2
    // (`:1139`), 6 (`:1317`), 8 (`:1353`, `:1376`) and 9 (81 printings from `:1409` to `:2505`), and its build
    // had no span tail. Only the last of those boundaries closed a single cascade.
    #[test]
    fn run_31s_cascade_line_names_the_one_span_its_readings_allow_and_leaves_the_rest_as_sums() {
        let readings: [(usize, &str, i64); 4] = [(2, "368.5", 378), (6, "366.0", 390), (8, "346.4", 390), (9, "338.2", 390)];
        let mut totals: Vec<i64> = vec![0];

        totals.extend(readings.iter().map(|(closed, printed, _)| {
            let candidates: Vec<i64> = (0..=20_000_i64).filter(|total| format!("{:.1}", mean_of(*total, *closed)) == *printed).collect();

            assert_eq!(candidates.len(), 1, "a printed mean and closed count price one running total, and more than one candidate would mean the readings are not pinned: {candidates:?}");
            candidates[0]
        }));

        assert_eq!(totals, [0, 737, 2_196, 2_771, 3_044], "each reading prices exactly one total over the cascades closed so far: {totals:?}");

        // The 8 to 9 boundary closed one cascade, so its span is the difference of two totals the log prints,
        // and nothing else about that cascade is needed to price it.
        assert_eq!(totals[4] - totals[3], 273, "cascade 9 closed 273 ms, and run 31's readings are what show it");

        // Every other boundary closed several cascades. The 2 to 6 boundary is four cascades inside 1,459 ms,
        // and two different span sets answer every reading that run logged, so the log cannot say which of
        // them ran.
        assert_eq!(totals[2] - totals[1], 1_459, "the 2 to 6 boundary is a four cascade sum, not a span");

        let pinned = [378, 359, 390, 390, 345, 334, 300, 275, 273];
        let swapped = [359, 378, 390, 389, 340, 340, 300, 275, 273];
        let at_closed = [2, 6, 8, 9];

        assert_eq!(
            aggregate_readings_a_set_prints(&pinned, &at_closed),
            aggregate_readings_a_set_prints(&swapped, &at_closed),
            "two different cascade sets print run 31's readings identically, so only the 273 ms span is a span that log shows"
        );
        assert_eq!(pinned[8], swapped[8], "and the one span both sets have to share is the one the readings name");
    }

    /// The pair the cascade line prints - `mean_of` over every span closed so far, and the worst of them -
    /// at each closed count in `at_closed`, folded the way `close_plate_pass` charges a span. Two span sets
    /// that answer this identically are a pair the aggregate cannot tell apart, however different the
    /// cascades they describe are.
    fn aggregate_readings_a_set_prints(spans: &[i64], at_closed: &[usize]) -> Vec<(String, i64)> {
        let mut printed = Vec::new();
        let mut closed = 0usize;
        let mut total_ms = 0i64;
        let mut worst_ms = 0i64;

        for span_ms in spans.iter().copied() {
            closed += 1;
            total_ms += span_ms;
            worst_ms = worst_ms.max(span_ms);

            if at_closed.contains(&closed) {
                printed.push((format!("{:.1}", mean_of(total_ms, closed)), worst_ms));
            }
        }

        printed
    }

    /// The span ring as `record_cascade_span` leaves it for a run that closed `spans.len()` cascades, fewer
    /// than the ring holds: slot order is close order, and no reading has named any of them yet.
    fn ring_of(spans: &[i64]) -> CascadeSpans {
        let mut ring = CascadeSpans::default();

        for (index, span_ms) in spans.iter().copied().enumerate() {
            ring.spans[index] = span_ms;
            ring.indexes[index] = index + 1;
        }

        ring.writes = spans.len();
        ring
    }

    // The cascade reading prices every cascade closed so far, and the typewrite reading prices every plate
    // step taken inside those cascades, so two successive readings price the steps of the cascade that
    // closed between them. That is the only way run 57's fifth cascade can be said step by step: the door
    // prints eight `StartTypewrite` calls (`PROBE_DETAIL_LIMIT`) and that cascade walked the run's 14th to
    // 20th, so not one of its six steps was ever printed. The four readings, each with the cascade count the
    // same report printed: 2 closed and 3 steps mean 244.3 worst 433 (`run log/hachimi-run57.log:1687`), 3
    // closed and 6 steps mean 266.8 worst 468 (`:1771`), 4 closed and 9 steps mean 229.8 worst 468 (`:1834`),
    // 5 closed and 15 steps mean 423.7 worst 4220 (`:1879`).
    const RUN57_STEPS_WHEN_FOUR_CLOSED: usize = 9;
    const RUN57_STEPS_WHEN_FIVE_CLOSED: usize = 15;
    const RUN57_WORST_PLATE_STEP_MS: i64 = 4_220;

    // What those two readings leave the fifth cascade's plate steps, both re-derived from the means a test
    // asserts rather than carried here on trust: its steps total 4,287 ms or 4,288 ms, so a step the door
    // never printed is at most 4,288 - 4,220 = 68 ms.
    const RUN57_FIFTH_CASCADE_STEPS_TOTAL_MS: i64 = 4_288;
    const RUN57_UNPRINTED_STEP_CEILING_MS: i64 = 68;

    /// What run 57's fifth cascade held, read off its own `InitializePlateList` call 5 line
    /// (`run log/hachimi-run57.log:1864`: 1.0, 0.0, 0.0, 0.16666650772094727, 0.16666650772094727), carrying
    /// the plate steps `steps`.
    fn run_57s_fifth_cascade(steps: &[i64]) -> CascadeRead {
        let mut cascade = cascade_with_steps(steps);
        cascade.index = 5;
        cascade.interval = 1.0;
        cascade.group_interval = 0.1666665;
        cascade.sequence_interval = 0.1666665;
        cascade
    }

    /// Whether a record of run 57's fifth cascade carries only plate steps that run can price for it. The
    /// run printed exactly one step of it, its worst at 4,220 ms, and left five steps that share 67 ms or 68
    /// ms between them, so a record of this cascade may hold any share of that 68 ms, or that printed step,
    /// or the printed step beside a share of it - but never a second step above the ceiling, and never more
    /// in total than the cascade's steps cost. A record that fails this names a step the run did not price:
    /// a number invented for the cascade rather than read off it.
    fn fifth_cascade_carries_only_its_runs_steps(cascade: &CascadeRead) -> bool {
        let steps: Vec<i64> = cascade.beats.iter().copied().take(cascade.beats_seen).collect();
        let printed: Vec<i64> = steps.iter().copied().filter(|step| *step > RUN57_UNPRINTED_STEP_CEILING_MS).collect();
        let total_ms: i64 = steps.iter().sum();

        match printed.as_slice() {
            [] => !steps.is_empty() && total_ms <= RUN57_UNPRINTED_STEP_CEILING_MS,
            [worst_ms] => *worst_ms == RUN57_WORST_PLATE_STEP_MS && total_ms <= RUN57_FIFTH_CASCADE_STEPS_TOTAL_MS,
            _ => false,
        }
    }

    /// Does a plate step of `step_ms` land on candidate `index` of the numbers `held` carries? A candidate
    /// the class holds at 0 paces nothing and `cascade_pace` skips it, so no step lands on it either.
    fn a_step_lands_on(held: &CascadeRead, index: usize, step_ms: i64) -> bool {
        let value_ms = f64::from(pace_value(held, index)) * f64::from(MS_PER_SECOND);

        value_ms > 0.0 && beats_against(&cascade_with_steps(&[step_ms]), value_ms).0 == 1
    }

    #[test]
    fn run_57s_cascade_5_says_none_for_every_plate_step_its_reports_price() {
        // Each reading's mean prices its whole millisecond total, and the difference between two successive
        // totals is what the cascade that closed between them cost in plate steps.
        let before_ms = cascade_total_a_mean_prices(RUN57_STEPS_WHEN_FOUR_CLOSED, "229.8");
        let after_ms = totals_a_mean_prices(RUN57_STEPS_WHEN_FIVE_CLOSED, "423.7");

        assert_eq!(before_ms, 2_068, "the plate steps taken when four cascades had closed price 2,068 ms");
        assert_eq!(after_ms, [6_355, 6_356], "the run's last mean 423.7 over 15 steps prices {after_ms:?} ms, and the run is not readable more exactly than that");

        // The fifth cascade is the only one that closed between those two readings, and it closed inside the
        // interval they bracket: its plate list call is at 20:45:32.652 and its typewrite end at
        // 20:45:37.757 (`:1864`/`:1868`), against the fourth cascade's typewrite end at 20:45:09.408
        // (`:1804`), which is before the nine step reading taken at 20:45:23. A plate step needs both its
        // ends inside one open cascade and `close_plate_pass` drops the previous mark, so no step spans two
        // cascades and the idle between them priced nothing. The steps the later reading adds are the fifth
        // cascade's, and only its.
        let fifth_ms: Vec<i64> = after_ms.iter().map(|total| total - before_ms).collect();
        let unprinted_ms: Vec<i64> = fifth_ms.iter().map(|total| total - RUN57_WORST_PLATE_STEP_MS).collect();

        assert_eq!(fifth_ms, [4_287, 4_288], "the plate steps the fifth cascade added total {fifth_ms:?} ms");
        assert!(fifth_ms.iter().all(|total| *total <= 5_106), "steps inside a cascade cannot total more than that cascade's own 5,106 ms span");
        assert_eq!(unprinted_ms, [67, 68], "the worst step counted with four cascades closed was 468 ms and the worst with five is 4,220 ms, so the 4,220 ms step is one of these and the steps the door never printed share {unprinted_ms:?} ms");

        // The same six read off the call totals rather than the step totals: 13 `StartTypewrite` calls with
        // four cascades closed (`:1827`) and 20 with five (`:1872`), and a cascade's first call has no step
        // in front of it.
        const RUN57_TYPEWRITE_CALLS_WITH_FOUR_CLOSED: usize = 13;
        const RUN57_TYPEWRITE_CALLS_WITH_FIVE_CLOSED: usize = 20;

        assert_eq!(RUN57_TYPEWRITE_CALLS_WITH_FIVE_CLOSED - RUN57_TYPEWRITE_CALLS_WITH_FOUR_CLOSED - 1, RUN57_STEPS_WHEN_FIVE_CLOSED - RUN57_STEPS_WHEN_FOUR_CLOSED, "seven calls make the six steps the step totals already said");
        assert_eq!(fifth_ms.iter().copied().max(), Some(RUN57_FIFTH_CASCADE_STEPS_TOTAL_MS), "so the total the record helper quotes is the larger of the two this reading prices");
        assert_eq!(unprinted_ms.iter().copied().max(), Some(RUN57_UNPRINTED_STEP_CEILING_MS), "and no single step the door left unprinted can be more than {RUN57_UNPRINTED_STEP_CEILING_MS} ms");

        // And every step this run prices for that cascade says none, printed or not. The 4,220 ms step is
        // 3,220 ms from the 1.0 s interval the caller handed and 4,053 ms from the 0.1666665 s beat the class
        // holds; a step at or under the 68 ms the unprinted ones share at most is at least 98 ms from that
        // beat and 932 ms from the interval, and 68 ms is the nearest a step that small gets to either, both
        // numbers standing above it. So `none` is the cascade's answer, not an artefact of the one step a
        // record is allowed to carry.
        let held = run_57s_fifth_cascade(&[]);

        assert_eq!(cascade_pace(&run_57s_fifth_cascade(&[RUN57_WORST_PLATE_STEP_MS])), (PACE_NONE, 0), "the one plate step the run printed for this cascade prices none of the numbers it held");

        for index in 0..PACE_CANDIDATES {
            assert!(!a_step_lands_on(&held, index, RUN57_WORST_PLATE_STEP_MS), "the run's 4,220 ms plate step does not land on {}", PACE_NAMES[index]);

            for step_ms in 0..=RUN57_UNPRINTED_STEP_CEILING_MS {
                assert!(!a_step_lands_on(&held, index, step_ms), "nor does a step of {step_ms} ms, which is as close as any step the door left unprinted gets to {}", PACE_NAMES[index]);
            }
        }
    }

    #[test]
    fn a_record_of_run_57s_fifth_cascade_is_refused_when_it_names_a_step_that_run_did_not_price() {
        // The record the pace reading carries for this cascade: the one step the run prices for it.
        assert!(fifth_cascade_carries_only_its_runs_steps(&run_57s_fifth_cascade(&[RUN57_WORST_PLATE_STEP_MS])), "the run's printed step is a record of this cascade");
        assert!(fifth_cascade_carries_only_its_runs_steps(&run_57s_fifth_cascade(&[RUN57_UNPRINTED_STEP_CEILING_MS])), "so is a step the size of everything its door left unprinted");
        assert!(fifth_cascade_carries_only_its_runs_steps(&run_57s_fifth_cascade(&[RUN57_WORST_PLATE_STEP_MS, RUN57_UNPRINTED_STEP_CEILING_MS])), "and the printed step beside that whole share, which is what its six steps cost in total");

        // The pair this test's data once carried: the run's worst step beside a second step of 3,100 ms.
        // 4,220 ms plus 3,100 ms is 7,320 ms of plate steps in a cascade whose six steps total 4,288 ms at
        // the outside and whose whole span is 5,106 ms, and it is more than the 6,355 ms the run priced for
        // all fifteen plate steps of all five cascades. Two steps above the ceiling is two printed steps,
        // and this run printed one of this cascade's.
        assert!(!fifth_cascade_carries_only_its_runs_steps(&run_57s_fifth_cascade(&[RUN57_WORST_PLATE_STEP_MS, 3_100])), "a second step that big is refused, whatever it makes the pace answer");

        // And a record cannot quietly invent several small ones either: the unprinted steps share 68 ms in
        // total, not 68 ms each.
        assert!(!fifth_cascade_carries_only_its_runs_steps(&run_57s_fifth_cascade(&[RUN57_UNPRINTED_STEP_CEILING_MS, RUN57_UNPRINTED_STEP_CEILING_MS])), "two steps of the ceiling are more than the unprinted steps share between them");
    }

    #[test]
    fn peak_merge_keeps_the_largest_finite_non_negative_value() {
        let after = peak_merge(0, 1.5);

        assert_eq!(f32::from_bits(peak_merge(after, 0.25)), 1.5);
        assert_eq!(f32::from_bits(peak_merge(after, 3.0)), 3.0);
        assert_eq!(peak_merge(after, f32::NAN), after);
        assert_eq!(peak_merge(after, -8.0), after);
    }

    #[test]
    fn a_gap_splits_at_the_plate_passes_inside_it() {
        // The shape run 12 read four times: a close, a plate pass during the idle, then the next cut
        // opening. The four legs are the whole gap, so whatever the pass was not doing stays visible as time
        // on one side of it or as the idle between two passes, instead of disappearing into a mean.
        let legs = gap_legs(1_000, 2_000, 6_000, 4_000, 9_000).expect("a gap with a plate pass between its ends has legs");

        assert_eq!(legs, (1_000, 4_000, 0, 3_000), "before the pass, the pass itself, nothing between passes, after it");
        assert_eq!(legs.0 + legs.1 + legs.2 + legs.3, 9_000 - 1_000, "the legs are the gap, with nothing invented and nothing lost");
    }

    #[test]
    fn a_gap_leg_prices_a_pass_entry_to_completion_and_names_the_idle_between_passes() {
        // Run 49b read `the pass itself mean 26,634.3 ms worst 79,903 ms` out of a door bounded at 0.25 s per
        // plate. Two passes that each took 500 ms inside a 9 s gap are the same reading: the pass is its
        // entry to its completion, and what sits between one completion and the next entry is its own leg.
        let legs = gap_legs(1_000, 2_000, 8_500, 1_000, 10_000).expect("a gap with two passes between its ends has legs");

        assert_eq!(legs, (1_000, 1_000, 5_500, 1_500), "the passes are charged 1,000 ms, not the 6,500 ms from the first call to the last");
        assert_eq!(legs.0 + legs.1 + legs.2 + legs.3, 10_000 - 1_000, "the legs are still the whole gap");
    }

    #[test]
    fn a_pass_span_that_reaches_past_the_gap_is_trimmed_rather_than_inventing_a_negative_leg() {
        // The sum the probe accumulated and the marks the gap is cut at come from different doors. A sum
        // larger than the span between the first entry and the last completion is a pairing this probe did
        // not earn, and the trim keeps the legs a reading rather than an artefact.
        let legs = gap_legs(1_000, 2_000, 3_000, 9_000, 9_000).expect("a gap with a pass between its ends has legs");

        assert_eq!(legs, (1_000, 1_000, 0, 6_000), "the pass leg is what the gap can hold");
    }

    #[test]
    fn a_plate_pass_mark_out_of_order_leaves_the_gap_whole() {
        // Every mark comes from one clock, so a mark outside the gap is a pairing this probe did not earn.
        // A leg made from it would move the claim rather than support it.
        assert_eq!(gap_legs(1_000, 500, 6_000, 1_000, 9_000), None, "a pass entry before the close is not in the gap");
        assert_eq!(gap_legs(1_000, 2_000, 12_000, 1_000, 9_000), None, "a pass completion after the next open is not in the gap");
        assert_eq!(gap_legs(-1, 2_000, 6_000, 1_000, 9_000), None, "a gap with no close is not a gap");
    }

    #[test]
    fn a_gap_with_no_plate_pass_counts_as_one_gap() {
        // The caller counts these apart so the report cannot read four legs of zero as an explanation of
        // an idle the probe never saw a door in.
        assert_eq!(gap_legs(1_000, -1, -1, 0, 9_000), None);
    }

    // A door that reports its values and keeps a peak counts one call. The peaked doors that printed
    // values called `observe` and then `sample`, and both incremented, so their totals line reported
    // twice the calls the game made (C53).
    #[test]
    fn a_peaked_door_that_reports_its_values_counts_one_call() {
        let probe = CutProbe::peaked("test door");

        probe.observe_peak(&[1.5], 1.5);
        probe.observe_peak(&[2.5], 2.5);

        assert_eq!(probe.calls(), 2);
        assert_eq!(probe.peak_bits(), 2.5f32.to_bits());
    }

    // A plate spacing belongs to a cut only when both events are inside it. An event from before the
    // cut opened, and the first event after it opened, both answer none rather than a number that
    // charges the idle between turns to the cut.
    #[test]
    fn plate_play_spacing_is_measured_only_inside_an_open_cut() {
        assert_eq!(play_cadence_ms(1_000, 400, 1_400), None);
        assert_eq!(play_cadence_ms(-1, 1_200, 1_400), None);
        assert_eq!(play_cadence_ms(1_000, -1, 1_400), None);
        assert_eq!(play_cadence_ms(1_000, 1_200, 2_100), Some(900));
    }

    #[test]
    fn a_plate_spacing_counts_in_the_window_that_already_held_the_earlier_event() {
        // Run 14 measured no spacing at all with the cut as the only window while 21 gauge plays and 22
        // plate list calls happened, because the cascade is often built between cuts (C55). The tighter
        // window wins when both hold the event, because a cascade inside a cut is the shorter span.
        assert_eq!(cadence_window_open_ms(1_000, 400, 1_200), 1_000);
        assert_eq!(cadence_window_open_ms(2_000, 400, 1_200), 400);
        // An event from before both windows opened is inside neither, and a window that never opened
        // cannot hold anything.
        assert_eq!(cadence_window_open_ms(2_000, 1_500, 1_200), -1);
        assert_eq!(cadence_window_open_ms(-1, -1, 1_200), -1);
    }

    #[test]
    fn a_spacing_between_two_cascades_is_not_charged_to_either_typewrite_clock() {
        // The typewrite clock only measures plates inside one cascade. A spacing that reaches back past
        // the open shows as none so the idle between cascades stays on the passes clock.
        assert_eq!(play_cadence_ms(500, 600, 900), Some(300));
        assert_eq!(play_cadence_ms(700, 600, 900), None);
        assert_eq!(play_cadence_ms(-1, 600, 900), None);
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
    fn two_overloads_of_the_same_method_keep_different_labels() {
        // `SkipRuntime(time)` and `SkipRuntime(frames, keep)` are two doors. An earlier version of the
        // totals line dropped the argument list, which printed both of them under one name.
        assert_eq!(CUTT_SKIP_RUNTIME_TIME.label(), "CutInTimelineController::SkipRuntime(time)");
        assert_eq!(CUTT_SKIP_RUNTIME_FRAMES.label(), "CutInTimelineController::SkipRuntime(frames, keep)");
        assert_ne!(CUTT_SKIP_RUNTIME_TIME.label(), CUTT_SKIP_RUNTIME_FRAMES.label());
    }

    #[test]
    fn the_stall_census_samples_the_controller_once_a_second() {
        // The first update on a controller is always sampled, then the census waits a full second before the
        // next read of the controller's clock. A sample a few frames later would be a second read of the same
        // stretch and would hide whether the clock is moving.
        assert!(stall_sample_due(-1, 0));
        assert!(!stall_sample_due(1_000, 1_999));
        assert!(stall_sample_due(1_000, 2_000));
        assert!(stall_sample_due(1_000, 9_000));
    }

    #[test]
    fn two_controllers_that_take_turns_keep_their_own_stall_clocks() {
        // Run 31's flip between two controllers on every frame. Each controller's clock starts at its own first
        // update and counts its own updates, however often the other one runs in between.
        let table = [const { StallSlot::new() }; STALL_SLOTS];
        let (first, second) = (0x2075_b2cd800usize, 0x2075_b2cd000usize);
        let mut now = 0;

        for _ in 0..10 {
            for controller in [first, second] {
                let slot = stall_slot_in(&table, controller, now);
                slot.last_update_ms.store(now, atomic::Ordering::Relaxed);
                slot.updates.fetch_add(1, atomic::Ordering::Relaxed);
            }

            now += 1;
        }

        let a = stall_slot_in(&table, first, now);
        let b = stall_slot_in(&table, second, now);

        assert_eq!(a.seen_since_ms.load(atomic::Ordering::Relaxed), 0, "the first controller counts from its first update");
        assert_eq!(b.seen_since_ms.load(atomic::Ordering::Relaxed), 0, "the second controller was not restarted by the first");
        assert_eq!(a.updates.load(atomic::Ordering::Relaxed), 10);
        assert_eq!(b.updates.load(atomic::Ordering::Relaxed), 10);
    }

    #[test]
    fn a_fifth_controller_takes_the_slot_nobody_has_updated_lately() {
        let table = [const { StallSlot::new() }; STALL_SLOTS];

        for (index, controller) in [0x1000usize, 0x2000, 0x3000, 0x4000].into_iter().enumerate() {
            let slot = stall_slot_in(&table, controller, index as i64);
            slot.last_update_ms.store(index as i64, atomic::Ordering::Relaxed);
        }

        let fifth = stall_slot_in(&table, 0x5000, 10);
        fifth.last_update_ms.store(10, atomic::Ordering::Relaxed);

        let holds = |controller: usize| table.iter().filter(|slot| slot.controller.load(atomic::Ordering::Relaxed) == controller).count();

        assert_eq!(holds(0x5000), 1, "the new controller has a slot");
        assert_eq!(holds(0x1000), 0, "the controller updated longest ago lost its slot");
        assert_eq!(holds(0x4000), 1, "the controllers updated lately keep theirs");
    }

    #[test]
    fn a_cut_run_is_attributed_to_the_screen_it_started_on() {
        assert_eq!(bucket_for(ViewId::SingleModeMain as i32, false), ViewBucket::Training);
        assert_eq!(bucket_for(ViewId::SingleModePaddock as i32, false), ViewBucket::Training);
        assert_eq!(bucket_for(ViewId::SingleModeResult as i32, false), ViewBucket::Training);
        assert_eq!(bucket_for(ViewId::Story as i32, false), ViewBucket::Story);
        // The story event mission screen is its own view, and the story event probe reports against it.
        assert_eq!(bucket_for(ViewId::StoryEventMission as i32, false), ViewBucket::StoryEvent);
        // Run 31 ended stuck on 1501 and every census line about it read `other`. The career end screens
        // play their inspiration cut-ins through the same timeline controller this probe hooks, so they
        // belong in the bucket that measures cut-ins (C62).
        assert_eq!(bucket_for(ViewId::SingleModeSuccessionEvent as i32, false), ViewBucket::StoryEvent);
        assert_eq!(bucket_for(ViewId::SingleModeSuccessionCut as i32, false), ViewBucket::StoryEvent);
        assert_eq!(bucket_for(ViewId::SingleModeSuccessionEvent as i32, true), ViewBucket::Race);
        assert_eq!(bucket_for(ViewId::GachaMain as i32, false), ViewBucket::Gacha);
        assert_eq!(bucket_for(ViewId::Title as i32, false), ViewBucket::Other);

        // The set a high speed write is held off on has to be the same set the frame clock buckets
        // as a training session, or the gate and the measurement disagree about what a turn is.
        assert!(VIEW_TRAINING.contains(&(ViewId::SingleModeMain as i32)));
        assert!(VIEW_TRAINING.contains(&(ViewId::SingleModeMonthStart as i32)));
        assert!(VIEW_TRAINING.contains(&(ViewId::SingleModeResult as i32)));
        assert!(!VIEW_TRAINING.contains(&(ViewId::Story as i32)));
        assert!(!VIEW_TRAINING.contains(&(ViewId::Title as i32)));

        // A skill cut-in inside a race reaches the same timeline controller this probe hooks. It is not
        // a training animation, and the scene check has to win over a view id the table does not name.
        assert_eq!(bucket_for(7000, true), ViewBucket::Race);
    }

    #[test]
    fn every_bucket_has_a_name_for_the_totals_line() {
        assert_eq!(BUCKET_NAMES.len(), BUCKET_COUNT);
        assert_eq!(BUCKET_NAMES[ViewBucket::Training as usize], "training screen");
    }

    #[test]
    fn a_cut_kind_comes_from_the_door_the_game_played_through() {
        // The tag cut-in player is the door a friendship cut is played through, so a call there decides
        // the kind even when no `IsValidTag` answer was seen.
        assert_eq!(tag_kind_for_run(true, TAG_UNKNOWN), TAG_FRIENDSHIP);
        assert_eq!(tag_kind_for_run(true, TAG_REGULAR), TAG_FRIENDSHIP);
        // Without that door the game's own answer stands, and an answer it never gave stays unknown.
        assert_eq!(tag_kind_for_run(false, TAG_FRIENDSHIP), TAG_FRIENDSHIP);
        assert_eq!(tag_kind_for_run(false, TAG_REGULAR), TAG_REGULAR);
        assert_eq!(tag_kind_for_run(false, TAG_UNKNOWN), TAG_UNKNOWN);
    }

    #[test]
    fn every_cut_kind_has_a_name_for_the_totals_line() {
        assert_eq!(TAG_NAMES.len(), TAG_COUNT);
        assert_eq!(TAG_NAMES[TAG_FRIENDSHIP], "friendship cut");
    }

    #[test]
    fn no_probe_is_measured_under_two_names() {
        // The install lines and this list are maintained by hand, and a duplicated entry is how run 9's
        // count would have been reported twice.
        let mut names: Vec<&str> = PROBES.iter().map(|probe| probe.name).collect();
        let total = names.len();

        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), total);
        assert_eq!(names.len(), PROBES.len());
    }

    #[test]
    fn the_friendship_cut_pacing_doors_are_counted_and_the_skip_doors_keep_the_time_they_are_handed() {
        // A friendship cut has never answered to `training_cut_speed` (2043, 2072, 2235 ms off against 2053,
        // 2129, 2044 ms armed), so its pacing doors are now in the census before anything is scaled on them:
        // the status panel's play in and the coroutine run 26 named as where the panel actually runs, its
        // frame class's three float doors and its own skip, and the tag cut-in player's effect doors.
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.name).collect();

        for want in [
            "SingleModeMainViewTrainingCutStatus::PlayIn(interval, frame, flag, action)",
            "SingleModeMainViewTrainingCutStatus::CoroutinePlayIn(interval, frame)",
            "SingleModeMainViewTrainingCutStatusFrame::PlayIn(int, int, int, int, int, int, time, int, flag)",
            "SingleModeMainViewTrainingCutStatusFrame::PlayInRootMotion(time)",
            "SingleModeMainViewTrainingCutStatusFrame::PlayOut(flag, time, action)",
            "SingleModeMainViewTrainingCutStatusFrame::PlayPreIn()",
            "SingleModeMainViewTrainingCutStatusFrame::Skip(int, int, int, int, int)",
            "SingleModeMainViewTagTrainingCutInPlayer::StopLineEffect()",
            "SingleModeMainViewTagTrainingCutInPlayer::DestroyButtonEffect()",
            "SingleModeMainViewTagTrainingCutInPlayer::PlaySuccessTextEffect()",
            "SingleModeMainViewTagTrainingCutInPlayer::DestroyLineEffect()",
            "SingleModeMainViewTagTrainingCutInPlayer::SetResultFlashPlayer(player)",
            "TrainingParamChangeSupportMemberA2U::PlayMemberCutin(ids, parent, action)",
            "TrainingParamChangeSupportMemberA2U::PlayAoharuSoulCutin(index, parent, action, action)",
            "TrainingParamChangeSupportMemberA2U::PlayAoharuSpSoulCutin(index, parent, action, action)",
            "TrainingParamChangeSupportMemberA2U::PlayOutMemberCutin(player, index, action)",
            "FlashPlayer::Play(action)",
            "AnMotion::SetMotionPlay(label, float)",
            "AnMotion::SetMotionPlay(label, int)",
            "AnMotion::SetMotionPlay(float)",
            "AnMotion::SetMotionPlay",
            "AnMotion::SetMotionStop",
            "FlashPlayer::Pause(flag)",
            "FlashPlayer::Resume()",
            "FlashPlayer::Stop()",
            "FlashPlayer::End()",
            "GraphicSettings::SetEffectTimeScale(scale)",
            "FlashActionPlayer::Play(label, time, label)",
            "FlashActionPlayer::Play(label, label)",
        ] {
            assert!(labels.contains(&want), "the census has no door for {want}");
        }

        // An auto skip of a training cut has to be asked for in the unit the game's own skip takes, and until
        // now these two doors were counted without ever keeping the time passed to them (item 70).
        assert!(CUTT_SKIP_RUNTIME_TIME.peaked, "SkipRuntime(time) no longer reports the time it was handed");
        assert!(CUTT_SKIP_TIME_DIRECT.peaked, "SkipTimeDirect(time) no longer reports the time it was handed");
        assert!(!CUTT_SKIP_RUNTIME_FRAMES.peaked, "the frames overload was swapped for the time overload it is not");

        let door = CutProbe::peaked("CutInTimelineController::SkipRuntime(time)");
        door.observe_peak(&[2.4], 2.4);

        assert_eq!(door.calls(), 1, "a peaked door counted its call twice (C53)");
        assert_eq!(f32::from_bits(door.peak_bits()), 2.4, "the skip door did not keep the time it was handed");
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

    // The command legs are the half of a training turn no lever reaches, and each of their marks can be
    // missing on its own, so the pairing is checked as plain marks rather than as a door call.
    #[test]
    fn a_command_leg_needs_both_of_its_marks_and_never_runs_backwards() {
        assert_eq!(command_legs(1_000, 500, 4_000), (Some(3_000), Some(500)));
        assert_eq!(command_legs(-1, 500, 4_000), (None, None), "a cut open with no command before it has no turnaround to report");
        assert_eq!(command_legs(1_000, -1, 4_000), (Some(3_000), None), "the first command of a run has no earlier cut to be late for");
        assert_eq!(command_legs(4_000, 5_000, 4_500), (Some(500), None), "a send reading before the cut it follows is out of order and is dropped");
    }

    // The silence reading exists because of the 2026-10-10 stall on the inspiration event, where
    // `UpdateSpeed` ran 3,456 to 46,652 times across censuses while every other training door sat frozen
    // at its last count. The split between the two kinds of counter is the whole rule, so it is the
    // thing the test drives.
    #[test]
    fn only_the_per_frame_door_is_exempt_from_a_silence_reading() {
        let entries: Vec<(&str, usize)> = vec![
            ("CutInTimelineController::UpdateSpeed()", 46_652),
            ("SingleModeMainTrainingCuttController::PlayTrainingCut(info)", 4),
            ("TrainingParamChangeUI::OnAllTypewriteEnd()", 9),
        ];

        assert_eq!(progress_total(&entries), 13, "the census counted the per-frame door as progress");
        assert_eq!(exempt_total(&entries), 46_652, "the census did not count the door that keeps running");
    }

    #[test]
    fn a_silence_has_to_outlast_the_longest_ordinary_gap_between_cuts() {
        assert!(!silence_due(SILENCE_WARN_SECS - 1), "a window shorter than the threshold was reported");
        assert!(silence_due(SILENCE_WARN_SECS));
    }

    #[test]
    fn the_silence_line_names_the_door_that_kept_running_and_the_clock_it_reads() {
        let line = silence_line(150, 9_000, 46_652, 1.0, 0);

        assert!(line.contains("flow silence 150 s"), "{line}");
        assert!(line.contains("CutInTimelineController::UpdateSpeed() ran 9000 more times (46652 total)"), "{line}");
        assert!(line.contains("Time.timeScale reads 1"), "{line}");
        assert!(line.contains("no training cut run open"), "{line}");
    }

    #[test]
    fn a_silence_with_a_cut_run_open_is_read_as_the_game_waiting_for_a_tap() {
        let line = silence_line(140, 8_400, 8_400, f32::NAN, 61_000);

        assert!(line.contains("a training cut run has been open for 61000 ms"), "{line}");
        assert!(line.contains("Time.timeScale reads unread"), "{line}");
    }

    // Item 106: the hold that keeps `ui_animation_scale` off the tween channel while a training cut plays.
    // The bound is the part a test can prove, because every stalled session measured so far (runs 52, 53
    // and 56) is one where `CleanUpCutt` was never called at all, so a hold that only that door could end
    // would leave the lever off for the rest of the day.
    #[test]
    fn a_cut_gate_hold_ends_at_its_wall_bound_when_the_close_door_never_arrives() {
        let gate = CutGate::new();

        assert!(!gate.engaged(), "a gate nobody opened is already holding the ui clock off");
        assert_eq!(gate.engage_at(0), 1, "the first hold of a session is not counted as the first");

        assert_eq!(gate.hold_expired_at(CUT_GATE_HOLD_US - 1), None, "the hold ended before its bound");
        assert!(gate.engaged(), "the hold let go of the ui clock early");
        assert_eq!(gate.hold_expired_at(CUT_GATE_HOLD_US), Some(CUT_GATE_HOLD_US), "the hold never ended on its own clock");
        assert!(!gate.engaged(), "an expired hold still has the ui clock off");
        assert_eq!(gate.tick(), None, "a closed hold is still timing the cut");
    }

    #[test]
    fn a_close_door_ends_a_cut_gate_hold_long_before_its_bound() {
        let gate = CutGate::new();

        gate.engage();
        gate.release();

        assert!(!gate.engaged(), "the close door let the hold run on");
        assert_eq!(gate.tick(), None, "a cut the game cleaned up still took a hold");
    }

    #[test]
    fn the_next_cut_rearms_the_cut_gate_clock() {
        let gate = CutGate::new();

        gate.engage_at(0);

        assert_eq!(gate.hold_expired_at(CUT_GATE_HOLD_US / 2), None, "the hold ended early");

        // The second cut opens half way through the first hold's bound. The clock restarts with the cut, so the
        // first hold's time is not carried into the one that follows it.
        assert_eq!(gate.engage_at(CUT_GATE_HOLD_US / 2), 2, "the second hold was counted as the first");
        assert!(gate.engaged());

        assert_eq!(
            gate.hold_expired_at(CUT_GATE_HOLD_US / 2 + CUT_GATE_HOLD_US - 1),
            None,
            "the previous cut's time was carried into this one"
        );
        assert_eq!(gate.hold_expired_at(CUT_GATE_HOLD_US / 2 + CUT_GATE_HOLD_US), Some(CUT_GATE_HOLD_US));
    }

    // The gate's arming, which is what the C57 write guard now stands on. Every one of these shapes used
    // to reach a played build as nothing at all: the resolver's own miss lines are debug level
    // (`AnimationSpeed.rs:1692`, `:1709`), the file filter is Info whenever `debug_mode` is off
    // (`core/log.rs:3-9`), and the install line required at least one armed door and a non-debug build.
    //
    // `gate_arming` only asks whether the resolver answered, so the two addresses below stand in for that
    // answer and neither is ever dereferenced: a test proves the decision, never the hook.
    const RESOLVED: usize = 0x1000;
    const NO_TARGET: usize = 0;

    fn gate_keys(keys: &[&'static str]) -> FnvHashSet<String> {
        keys.iter().map(|key| key.to_string()).collect()
    }

    // The ids the `new_hook!:` lines of a played build print for this module's gate sites, and for the two
    // `probe!` sites the census tests below arm. Spelled out because the whole point of checking them is
    // that they are the strings a run reads, not an approximation of them.
    const GATE_OPEN_ID: &str = "umamusume::TrainingCuttProbe::TrainingCutt_PlayTrainingCut";
    const GATE_SCENARIO_ID: &str = "umamusume::TrainingCuttProbe::TrainingCutt_PlayScenarioTrainingCut";
    const GATE_CLOSE_ID: &str = "umamusume::TrainingCuttProbe::TrainingCutt_CleanUpCutt";
    const GATE_DOOR_IDS: [&str; 3] = [GATE_OPEN_ID, GATE_SCENARIO_ID, GATE_CLOSE_ID];
    const PLAY_TRAINING_CUT_END_ID: &str = "umamusume::TrainingCuttProbe::TrainingCutt_PlayTrainingCutEndAsync";
    const PLAY_IN_TRAINING_STATUS_ID: &str = "umamusume::TrainingCuttProbe::TrainingCutt_PlayInTrainingStatus";

    #[test]
    fn the_gate_arms_behind_the_ids_new_hook_keys_those_sites_on() {
        use crate::il2cpp::hook::hook_is_disabled;

        // `disabled_hooks` is keyed on the arming site, and `new_hook!` builds that key as
        // `hook_id(module_path!() + "::" + the wrapper name)` (`hook/mod.rs:291`). The door check has to
        // spell the same key or it answers for a hook this file does not arm - the C27 mistake pointed the
        // other way. These are the three ids the log prints for these sites.
        let ids: Vec<&'static str> = gate_doors().iter().map(|door| door.id).collect();
        assert_eq!(
            ids,
            [
                "umamusume::TrainingCuttProbe::TrainingCutt_PlayTrainingCut",
                "umamusume::TrainingCuttProbe::TrainingCutt_PlayScenarioTrainingCut",
                "umamusume::TrainingCuttProbe::TrainingCutt_CleanUpCutt",
            ],
            "the gate is checking ids these arming sites do not answer to"
        );

        // And it answers with the same rule the shipped arming path decides from, for every shape of key a
        // config may hold: the id, the bare wrapper name a config written before ids existed holds,
        // another hook's id, a typo, a half written module path.
        let shapes: [&'static str; 4] = [
            "TrainingCutt_CleanUpCutt",
            "umamusume::NowLoading::Hide",
            "TrainingCutt_PlayTrainingCutt2",
            "TrainingCuttProbe::TrainingCutt_CleanUpCutt",
        ];

        for shape in shapes {
            let configured = gate_keys(&[shape]);

            for door in gate_doors() {
                assert_eq!(
                    key_putting_door_down(&configured, door.id).is_some(),
                    hook_is_disabled(&configured, door.id),
                    "the gate's door check and `hook_is_disabled` disagree about {shape:?} and {}",
                    door.id,
                );
            }
        }
    }

    #[test]
    fn a_config_naming_nothing_arms_every_door_the_gate_asks_for() {
        // The neutral half: `Config::disabled_hooks` is `#[serde(default)]`, an empty set is what a config
        // that says nothing about it holds, and an empty set changes nothing about this gate - the same
        // three doors and the same install line `529e4f5` was measured with in game.
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&[]));

        assert_eq!(arming.armed, 3);
        assert_eq!(arming.resolved, 3);
        assert!(arming.complete());
        assert!(arming.open_armed && arming.close_armed);
        assert!(arming.unresolved.is_empty() && arming.disabled.is_empty());

        let line = cut_gate_armed_line(&arming);
        assert!(line.contains("ui clock held at 1.0 while a training cut plays"), "{line}");
        assert!(line.contains("armed on 3 of 3 doors"), "{line}");
        assert!(line.contains("3 of 3 resolved"), "{line}");
        assert!(!line.contains("inert"), "{line}");
    }

    #[test]
    fn a_door_that_resolved_and_was_put_down_is_not_counted_as_armed() {
        // The C57 shape. `resolve_method` answered an address for all three doors - the half the count used
        // to stop at - and the player's config named one of them, so `new_hook!` took its other branch and
        // created no hook. The gate has two doors, and the number a run reads has to say two rather than
        // print `armed on 3 of 3 doors` about a gate missing a door it armed nothing on.
        let key = "umamusume::TrainingCuttProbe::TrainingCutt_PlayTrainingCut";
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys([key].as_slice()));

        assert_eq!(arming.resolved, 3, "the client does have all three methods; that half is still true");
        assert_eq!(arming.armed, 2, "a door a `disabled_hooks` key put down was counted as armed");
        assert!(!arming.complete(), "an incomplete gate took the all-armed line");
        assert!(arming.open_armed, "`PlayScenarioTrainingCut` is armed, so this gate can still open a hold");
        assert!(arming.close_armed);
        assert!(arming.unresolved.is_empty());
        assert_eq!(arming.disabled.len(), 1);
        assert_eq!(arming.disabled[0].0, GATE_DOOR_LABELS[0]);
        assert_eq!(arming.disabled[0].1, key);

        let line = cut_gate_missing_line(&arming, true);
        assert!(line.contains("armed on 2 of 3 doors"), "{line}");
        assert!(line.contains("3 of 3 resolved"), "{line}");
        assert!(line.contains(key), "{line}");
        assert!(line.contains(GATE_DOOR_LABELS[0]), "{line}");
        assert!(!line.contains("inert"), "{line}");
    }

    #[test]
    fn a_bare_wrapper_name_key_puts_a_gate_door_down_the_way_it_puts_any_hook_down() {
        // A config written while `disabled_hooks` matched on the wrapper name still reaches the door
        // (C27), and the gate counts it as unarmed the same way it counts an id.
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&["TrainingCutt_CleanUpCutt"]));

        assert_eq!(arming.resolved, 3);
        assert_eq!(arming.armed, 2, "the bare key put the close door down and it still counted as armed");
        assert!(arming.open_armed);
        assert!(!arming.close_armed);
        assert_eq!(arming.disabled[0].0, GATE_DOOR_LABELS[2]);

        // What that costs is the tick bound, and the line says it.
        let line = cut_gate_missing_line(&arming, true);
        assert!(line.contains("armed on 2 of 3 doors"), "{line}");
        assert!(line.contains(&format!("only on its {} s wall bound", CUT_GATE_HOLD_US / 1_000_000)), "{line}");
        assert!(!line.contains("the cut gate is inert"), "{line}");
    }

    #[test]
    fn a_gate_whose_doors_disabled_hooks_put_down_reports_itself_inert() {
        // Every door resolved, every door put down: the shape a played build used to read as a working
        // gate. `armed > 0` called it armed, `engaged()` answered the way a closed cut answers, no
        // refusal was ever reachable, and the install line printed
        // `Cutt gate: ui clock held at 1.0 while a training cut plays, armed on 3 of 3 doors`.
        let arming = gate_arming(
            RESOLVED,
            RESOLVED,
            RESOLVED,
            &gate_keys(&[
                "umamusume::TrainingCuttProbe::TrainingCutt_PlayTrainingCut",
                "umamusume::TrainingCuttProbe::TrainingCutt_PlayScenarioTrainingCut",
                "TrainingCutt_CleanUpCutt",
            ]),
        );

        assert_eq!(arming.resolved, 3);
        assert_eq!(arming.armed, 0);
        assert!(!arming.open_armed && !arming.close_armed);

        // The answer the three shipped decisions read is `false` all session for this build, and only the
        // install line says that no door able to open a hold armed.
        let line = cut_gate_missing_line(&arming, true);
        assert!(line.contains("armed on 0 of 3 doors"), "{line}");
        assert!(line.contains("3 of 3 resolved"), "{line}");
        assert!(line.contains("the cut gate is inert"), "{line}");
        assert!(!line.contains("no matching overload"), "{line}");

        for door in gate_doors() {
            assert!(line.contains(door.label), "{line}");
        }

        // Each door is named with the key the player actually wrote, which is not always the id: the close
        // door of this fixture was put down by its bare wrapper name.
        assert_eq!(arming.disabled.len(), 3);
        assert_eq!(arming.disabled[2].1, "TrainingCutt_CleanUpCutt");

        for (_label, key) in &arming.disabled {
            assert!(line.contains(&format!("key {key:?}")), "{line}");
        }
    }

    #[test]
    fn a_gate_with_only_its_close_door_armed_cannot_open_a_hold() {
        // `armed > 0` called this a guard. `CutGate::engage` is reached only from the two start doors
        // (`TrainingCutt_PlayTrainingCut`, `TrainingCutt_PlayScenarioTrainingCut`), so with both put down
        // `engaged()` answers the way a closed cut answers for the whole session and no door ever ends a
        // hold that never started.
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&["TrainingCutt_PlayTrainingCut", "TrainingCutt_PlayScenarioTrainingCut"]));

        assert_eq!(arming.armed, 1);
        assert!(arming.close_armed, "the close door did arm");
        assert!(!arming.open_armed, "one armed door was reported as an armed gate");

        // The reproduction of the finding, kept in the test that closes it: this gate has an armed door,
        // so the count rule this build no longer uses counts it as armed, while the half that matters -
        // a door able to call `CutGate::engage` - is not armed at all.
        assert!(arming.armed > 0, "the count this build stopped reading still counts this gate as armed");
        assert!(!arming.open_armed, "the gate has no door that can open a hold");

        let line = cut_gate_missing_line(&arming, true);
        assert!(line.contains("armed on 1 of 3 doors"), "{line}");
        assert!(line.contains("the cut gate is inert"), "{line}");
    }

    #[test]
    fn a_gate_whose_start_doors_resolved_nothing_cannot_observe_a_cut() {
        // The same gate as above from the other cause: this client answered for the close door and for
        // neither start door. `armed == 1` is the number the install line prints, and `armed > 0` is the
        // rule that used to turn that number into "the gate is armed" - the count and the flag answer
        // different questions, and only the flag is what the two write passes stand on.
        let arming = gate_arming(NO_TARGET, NO_TARGET, RESOLVED, &gate_keys(&[]));

        assert_eq!(arming.armed, 1, "the close door did arm");
        assert_eq!(arming.resolved, 1);
        assert!(arming.close_armed);
        assert!(!arming.open_armed, "a gate with no door that opens a hold was counted as armed");
        assert_eq!(arming.unresolved, [GATE_DOOR_LABELS[0], GATE_DOOR_LABELS[1]]);

        // The count armed nothing: no door that opens a hold armed, whatever `armed` says.
        assert!(arming.armed > 0);
        assert!(!arming.open_armed, "the armed door is the close door, and a close door opens nothing");

        // Named from the client's side: no `disabled_hooks` key put these doors down, the resolver did
        // not answer for them.
        let line = cut_gate_missing_line(&arming, true);
        assert!(line.contains("armed on 1 of 3 doors"), "{line}");
        assert!(line.contains("1 of 3 resolved"), "{line}");
        assert!(line.contains("no matching overload"), "{line}");
        assert!(line.contains(GATE_DOOR_LABELS[0]), "{line}");
        assert!(line.contains(GATE_DOOR_LABELS[1]), "{line}");
        assert!(!line.contains("disabled_hooks"), "{line}");
        assert!(line.contains("the cut gate is inert"), "{line}");

        // Not the tick bound clause: with no start door there is no hold for the close door to end, and
        // the line that warns about a bounded hold would point a run at the wrong cost.
        assert!(!line.contains("wall bound"), "{line}");
    }

    #[test]
    fn a_gate_with_one_start_door_armed_is_a_gate_that_can_still_see_a_cut() {
        // The boundary the fix must not overshoot: `open_armed` answers that some door able to open a
        // hold armed, not that every one did. Half the pair still opens holds, still answers
        // `ArmedOpen` for a cut that came through it, and the install line still names the door that is
        // missing - which is what tells a run that a cut played through that door is outside the window.
        let arming = gate_arming(NO_TARGET, RESOLVED, RESOLVED, &gate_keys(&[]));

        assert_eq!(arming.armed, 2);
        assert!(arming.open_armed, "`PlayScenarioTrainingCut` opens a hold on its own");

        let line = cut_gate_missing_line(&arming, true);
        assert!(line.contains(GATE_DOOR_LABELS[0]), "{line}");
        assert!(!line.contains(GATE_DOOR_LABELS[1]), "{line}");
        assert!(!line.contains("inert"), "{line}");
        assert!(!line.contains("wall bound"), "{line}");
    }

    #[test]
    fn an_install_that_arms_nothing_names_every_door_it_could_not_arm() {
        // The whole chain on this side, driven by the function `init` calls rather than by its inputs.
        // With no class there is nothing to resolve, so `arm_cut_gate` never reaches il2cpp on this path
        // and a test can run it: the answer it returns is the one `init` prints, and that line is what a
        // run reads to know the gate behind the ui clock hold and the two write passes does not exist.
        let arming = arm_cut_gate(None, &gate_keys(&[]));

        assert_eq!(arming.armed, 0);
        assert_eq!(arming.resolved, 0);
        assert!(!arming.open_armed && !arming.close_armed);
        assert!(!arming.unresolved.is_empty(), "the gate names the doors it could not arm");
        assert_eq!(arming.unresolved, GATE_DOOR_LABELS);

        let line = cut_gate_missing_line(&arming, false);
        assert!(line.contains("armed on 0 of 3 doors"), "{line}");
        assert!(line.contains("no class"), "{line}");
        assert!(line.contains("the cut gate is inert"), "{line}");
    }

    #[test]
    fn a_gate_with_no_class_names_every_door_it_could_not_arm() {
        // The 0-of-3 shape: `class_for_label` answering None for
        // `Gallop.SingleModeMainTrainingCuttController`. It printed nothing in either mode, so a build
        // whose guard was inert read in `hachimi.log` exactly like a build whose guard worked.
        let line = cut_gate_missing_line(&GateArming { unresolved: GATE_DOOR_LABELS.to_vec(), ..GateArming::default() }, false);

        assert!(line.contains("armed on 0 of 3 doors"), "{line}");
        assert!(line.contains("0 of 3 resolved"), "{line}");
        assert!(line.contains("no class"), "{line}");
        assert!(line.contains("SingleModeMainTrainingCuttController::PlayTrainingCut(info)"), "{line}");
        assert!(line.contains("SingleModeMainTrainingCuttController::PlayScenarioTrainingCut(info)"), "{line}");
        assert!(line.contains("SingleModeMainTrainingCuttController::CleanUpCutt()"), "{line}");
        assert!(line.contains("the cut gate is inert"), "{line}");
    }

    #[test]
    fn a_gate_missing_its_close_door_says_that_every_cut_costs_the_tick_bound() {
        // The 2-of-3 shape: with no `CleanUpCutt` wrapper armed nothing calls `CutGate::release`, so a hold
        // ends only on its tick bound and every cut of the session costs the window in which both write
        // passes refuse the raise and its owed restore. This is where that becomes readable before the
        // first cut instead of after one.
        let arming = gate_arming(RESOLVED, RESOLVED, NO_TARGET, &gate_keys(&[]));
        let line = cut_gate_missing_line(&arming, true);

        assert!(line.contains("armed on 2 of 3 doors"), "{line}");
        assert!(line.contains("2 of 3 resolved"), "{line}");
        assert!(line.contains("no matching overload"), "{line}");
        assert!(line.contains("SingleModeMainTrainingCuttController::CleanUpCutt()"), "{line}");
        assert!(line.contains(&format!("only on its {} s wall bound", CUT_GATE_HOLD_US / 1_000_000)), "{line}");
        assert!(!line.contains("the cut gate is inert"), "{line}");
    }

    #[test]
    fn the_install_line_names_the_same_doors_the_census_counts() {
        // A door named as missing in the install line is the door the totals line counts, so a run can
        // check the arming against the calls the session actually made.
        assert_eq!(GATE_DOOR_LABELS[0], CUTT_PLAY_TRAINING_CUT.label());
        assert_eq!(GATE_DOOR_LABELS[1], CUTT_PLAY_SCENARIO_TRAINING_CUT.label());
        assert_eq!(GATE_DOOR_LABELS[2], CUTT_CLEAN_UP_CUTT.label());

        let doors = gate_doors();

        assert_eq!(doors[0].label, GATE_DOOR_LABELS[0]);
        assert_eq!(doors[1].label, GATE_DOOR_LABELS[1]);
        assert_eq!(doors[2].label, GATE_DOOR_LABELS[2]);
        assert!(!doors[0].closes && !doors[1].closes, "the two start doors are the pair that opens a hold");
        assert!(doors[2].closes, "the close door is the one that ends one");
    }

    #[test]
    fn the_gate_publishes_only_the_answer_529e4f5_shipped() {
        // `cut_gate_engaged` is the whole public surface of the gate: `AnimationSpeed::tween_clocks`,
        // `HighSpeedSetting::apply` and both of `StoryTimelineController`'s writes read that one bool and
        // nothing else, so no reader can be handed a richer answer than the shipped one and no reader has
        // to work out on its own whether the doors behind the answer armed.
        assert!(!cut_gate_engaged(), "a gate no install armed reported a cut it never saw");

        // The shape the gate was measured with in game, and the line a run reads for it: every door armed,
        // so `false` from here on is a closed cut and not an unarmed gate.
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&[]));
        assert!(arming.open_armed && arming.close_armed);
        assert!(cut_gate_armed_line(&arming).contains("armed on 3 of 3 doors"), "the armed line does not name the doors it armed");
    }

    // The census half of the same arming. `init` prints two lines off one install - the gate's and the
    // census's - and they used to count differently: the census was seeded off `arming.armed` while its body
    // still added a door for every address `resolve_method` answered, so `Cutt probe: 5 doors installed`
    // described a build that created 4 hooks. `the_census_counts_a_door_new_hook_never_arms_as_resolved_only`
    // is that reproduction, run against the counting `init` uses now.
    //
    // The ids are the ones these arming sites answer to: `census_door_id!` spells an id the way `new_hook!`
    // does (`hook/mod.rs:291`), and run 33 prints the same spelling for the doors `probe!` arms
    // (`run log/hachimi-run33.log:530-544`). `TrainingCutt_PlayTrainingCut` and
    // `TrainingCutt_PlayTrainingCutEndAsync` share a prefix, not a name, which is what the bare-name half of
    // the key rule is exact about.
    const PLAY_TRAINING_CUT_END_LABEL: &str = "SingleModeMainTrainingCuttController::PlayTrainingCutEndAsync";
    const PLAY_IN_TRAINING_STATUS_LABEL: &str = "SingleModeMainTrainingCuttController::PlayInTrainingStatus";

    #[test]
    fn the_census_counts_a_door_under_the_id_its_arming_site_answers_to() {
        // A census that matched a key against a name instead of the id `new_hook!` builds would answer for
        // hooks this build does not arm - the C27 mistake pointed the other way. These are the strings the
        // `new_hook!:` lines print for these sites.
        let censused: Vec<&'static str> = census_fixture_doors().iter().map(|(_, id)| *id).collect();
        let armed_here: Vec<&'static str> = gate_doors().iter().map(|door| door.id).collect();

        assert_eq!(censused, [PLAY_TRAINING_CUT_END_ID, PLAY_IN_TRAINING_STATUS_ID], "the census matched keys against ids these sites do not answer to");
        assert_eq!(armed_here, GATE_DOOR_IDS, "the gate checked keys against ids these sites do not answer to");
    }

    #[test]
    fn a_bare_wrapper_name_reaches_the_door_whose_name_it_is_and_not_the_door_it_only_resembles() {
        use crate::il2cpp::hook::hook_key_match;

        // TrainingCutt's doors share a prefix, so this is where a bare key could take down a door the player
        // did not name. The bare half of the rule is string equality on the last segment of the id.
        let mut doors: Vec<&'static str> = gate_doors().iter().map(|door| door.id).collect();
        doors.extend_from_slice(&census_fixture_doors().iter().map(|(_, id)| *id).collect::<Vec<&'static str>>());

        let reached: Vec<&'static str> = doors.iter().copied().filter(|id| hook_key_match("TrainingCutt_PlayTrainingCut", id).is_some()).collect();

        assert_eq!(reached, [GATE_OPEN_ID], "a bare key put down {} door(s) besides the one its name is", reached.len());

        // A half written id reaches nothing, and the census leaves that door armed.
        let half = "TrainingCuttProbe::TrainingCutt_CleanUpCutt";
        assert!(doors.iter().all(|id| hook_key_match(half, id).is_none()), "a half written id reached a door");
    }

    #[test]
    fn the_census_counts_a_door_new_hook_never_arms_as_resolved_only() {
        use crate::il2cpp::hook::hook_is_disabled;

        // The reproduction, run against the counting `init` uses now. The old body counted
        // `arming.armed + 1` for every address the resolver answered, which read 5 doors installed for the
        // build this fixture describes; `new_hook!` created 4 hooks (`hook/mod.rs:293` took its other branch
        // for the cut end async door).
        let doors = census_fixture_doors();
        let disabled = gate_keys(&[doors[0].1]);
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &disabled);
        let mut census = CensusCounts::from_gate(&arming);

        for (label, id) in doors {
            census.record_resolved(label, id, &disabled);
        }

        let ids: Vec<&'static str> = gate_doors().iter().map(|door| door.id).chain(doors.iter().map(|(_, id)| *id)).collect();
        let created = ids.iter().filter(|id| !hook_is_disabled(&disabled, id)).count();

        assert_eq!(census.resolved, 5, "every door that resolved is still accounted for");
        assert_eq!(census.armed, created, "the census armed {} doors for a build that creates {created} hooks", census.armed);
        assert_eq!(census.disabled, vec![(PLAY_TRAINING_CUT_END_LABEL, doors[0].1.to_string())], "the census did not name the door and the key that put it down");

        let line = census_line(&census, 5);
        assert!(line.contains("armed on 4 of 5 doors"), "{line}");
        assert!(line.contains("5 of 5 resolved"), "{line}");
        assert!(!line.contains("installed"), "the census line collapsed the two facts back into one count: {line}");

        let named = census_disabled_line(&census);
        assert!(named.contains("armed nothing on 1 resolved door"), "{named}");
        assert!(named.contains(PLAY_TRAINING_CUT_END_LABEL), "{named}");
        assert!(named.contains(&format!("key {:?}", doors[0].1)), "{named}");
    }

    #[test]
    fn a_census_that_arms_every_door_says_resolved_and_armed_are_the_same_number_only_here() {
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&[]));
        let census = CensusCounts::from_gate(&arming);

        assert_eq!((census.asked, census.resolved, census.armed), (3, 3, 3));
        assert!(census.disabled.is_empty());

        // Both lines a run reads, from one arming, with the two facts named apart.
        let gate_line = cut_gate_armed_line(&arming);
        let probe_line = census_line(&census, 3);

        for line in [&gate_line, &probe_line] {
            assert!(line.contains("armed on 3 of 3 doors"), "{line}");
            assert!(line.contains("3 of 3 resolved"), "{line}");
        }

        assert!(probe_line.contains("3 doors carry a counted kind"), "{probe_line}");
    }

    #[test]
    fn a_census_door_put_down_by_a_disabled_hooks_id_counts_as_resolved_and_never_as_armed() {
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&[GATE_SCENARIO_ID]));
        let census = CensusCounts::from_gate(&arming);

        assert_eq!((census.asked, census.resolved, census.armed), (3, 3, 2), "a door the config named still counted as armed");

        let line = census_line(&census, 2);
        assert!(line.contains("armed on 2 of 3 doors"), "{line}");
        assert!(line.contains("3 of 3 resolved"), "{line}");

        // The census half of the clause names the door and the key, the way the gate line does.
        let mut doors = CensusCounts::default();
        doors.record_resolved(PLAY_IN_TRAINING_STATUS_LABEL, PLAY_IN_TRAINING_STATUS_ID, &gate_keys(&[PLAY_IN_TRAINING_STATUS_ID]));

        assert_eq!((doors.asked, doors.resolved, doors.armed), (1, 1, 0), "a door armed nothing and still counted as armed");
        assert_eq!(doors.disabled[0].1, PLAY_IN_TRAINING_STATUS_ID);
    }

    #[test]
    fn a_census_door_put_down_by_a_bare_wrapper_name_counts_as_resolved_and_names_the_key_written() {
        // A config written while `disabled_hooks` matched on the wrapper name still reaches the door (C27),
        // and the census counts it unarmed the same way it counts an id - while the door that merely shares
        // the prefix stays armed.
        let arming = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&["TrainingCutt_CleanUpCutt"]));
        let census = CensusCounts::from_gate(&arming);

        assert_eq!((census.asked, census.resolved, census.armed), (3, 3, 2), "the bare key put the close door down and it still counted as armed");

        let mut doors = CensusCounts::default();
        doors.record_resolved(PLAY_TRAINING_CUT_END_LABEL, PLAY_TRAINING_CUT_END_ID, &gate_keys(&["TrainingCutt_PlayTrainingCutEndAsync", "TrainingCutt_PlayInTrainingStatus"]));
        doors.record_resolved(PLAY_IN_TRAINING_STATUS_LABEL, PLAY_IN_TRAINING_STATUS_ID, &gate_keys(&["TrainingCutt_PlayTrainingCut"]));

        // Two doors asked and resolved; the first armed nothing under its own bare name, the second armed
        // because the key written next to it is a different door's name.
        assert_eq!((doors.asked, doors.resolved, doors.armed), (2, 2, 1), "the bare-name half of the rule reached a door it only resembles");
        assert_eq!(doors.disabled, vec![(PLAY_TRAINING_CUT_END_LABEL, "TrainingCutt_PlayTrainingCutEndAsync".to_string())]);

        let named = census_disabled_line(&doors);
        assert!(named.contains("armed nothing on 1 resolved door"), "{named}");
        assert!(named.contains(&format!("key {:?}", "TrainingCutt_PlayTrainingCutEndAsync")), "{named}");
        assert!(!named.contains("TrainingCutt_PlayInTrainingStatus"), "the line named a door the key did not reach: {named}");
    }

    #[test]
    fn a_census_door_that_resolved_nothing_counts_as_neither_number() {
        // The client has no such method: that is the `resolved` half answering no, and it is not the same
        // shape as a door the player switched off.
        let arming = gate_arming(RESOLVED, RESOLVED, NO_TARGET, &gate_keys(&[]));
        let mut census = CensusCounts::from_gate(&arming);
        census.record_resolved(PLAY_IN_TRAINING_STATUS_LABEL, PLAY_IN_TRAINING_STATUS_ID, &gate_keys(&[]));
        census.record_miss();

        assert_eq!((census.asked, census.resolved, census.armed), (5, 3, 3));
        assert!(census.disabled.is_empty(), "a door the resolver never answered for was reported as disabled");

        let line = census_line(&census, 3);
        assert!(line.contains("armed on 3 of 5 doors"), "{line}");
        assert!(line.contains("3 of 5 resolved"), "{line}");
    }

    #[test]
    fn a_module_with_no_class_resolves_and_arms_nothing_in_either_line() {
        // `class_for_label` answering None for `Gallop.SingleModeMainTrainingCuttController`: the census
        // takes the same 0-of-3 answer the gate line prints instead of counting doors it never asked for.
        let arming = arm_cut_gate(None, &gate_keys(&[]));
        let census = CensusCounts::from_gate(&arming);

        assert_eq!((census.asked, census.resolved, census.armed), (3, 0, 0));
        assert!(census.disabled.is_empty());

        let line = census_line(&census, 0);
        assert!(line.contains("armed on 0 of 3 doors"), "{line}");
        assert!(line.contains("0 of 3 resolved"), "{line}");

        let gate_line = cut_gate_missing_line(&arming, false);
        assert!(gate_line.contains("armed on 0 of 3 doors"), "{gate_line}");
        assert!(gate_line.contains("0 of 3 resolved"), "{gate_line}");
    }

    #[test]
    fn the_gate_line_and_the_census_line_report_one_arming_in_the_same_two_numbers() {
        // The five shapes, read both ways. A run that only ever looks at the census line and a run that only
        // ever looks at the gate line have to come away with the same resolved count and the same armed
        // count; before this they could not, because only the gate line consulted the key rule.
        let all_armed = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&[]));
        let id_down = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&[GATE_OPEN_ID]));
        let name_down = gate_arming(RESOLVED, RESOLVED, RESOLVED, &gate_keys(&["TrainingCutt_CleanUpCutt"]));
        let unresolved = gate_arming(NO_TARGET, RESOLVED, RESOLVED, &gate_keys(&[]));
        let no_class = arm_cut_gate(None, &gate_keys(&[]));

        let shapes: [(&GateArming, bool); 5] = [(&all_armed, true), (&id_down, true), (&name_down, true), (&unresolved, true), (&no_class, false)];

        for (arming, class_found) in shapes {
            let census = CensusCounts::from_gate(arming);
            let armed = arming.armed;
            let resolved = arming.resolved;

            assert_eq!(census.armed, armed, "the census armed {} doors, the gate line armed {armed}", census.armed);
            assert_eq!(census.resolved, resolved, "the census resolved {} doors, the gate line resolved {resolved}", census.resolved);
            assert_eq!(census.asked, 3);

            let gate_line = if arming.complete() {
                cut_gate_armed_line(arming)
            }
            else {
                cut_gate_missing_line(arming, class_found)
            };
            let probe_line = census_line(&census, armed);

            for line in [&gate_line, &probe_line] {
                assert!(line.contains(&format!("armed on {armed} of 3 doors")), "{line}");
                assert!(line.contains(&format!("{resolved} of 3 resolved")), "{line}");
            }
        }
    }
}
