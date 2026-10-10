use std::ffi::CString;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicUsize};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use fnv::FnvHashMap;
use once_cell::sync::Lazy;

use crate::{
    core::{hachimi::recover_lock, settings_preset, Hachimi},
    il2cpp::{
        hook::umamusume::{AnimationSpeed, SceneDefine::ViewId, SceneManager},
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
}

impl CutProbe {
    pub(crate) const fn counted(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0), peaked: false, peak: AtomicU32::new(0) }
    }

    pub(crate) const fn peaked(name: &'static str) -> Self {
        Self { name, calls: AtomicUsize::new(0), peaked: true, peak: AtomicU32::new(0) }
    }

    // First hits print the values, later ones only the count, so a path the game polls every frame
    // cannot fill the log.
    pub(crate) fn observe(&self, values: &[f64]) {
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

fn hole_census_line(span_ms: i64, table: &FnvHashMap<&'static str, HoleCensusEntry>) -> String {
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

    format!("Cutt probe cut hole census over {span_ms} ms: {doors} counted door(s) crossed {crossings} times while the status panel was held off: {listed}")
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
// The friendship split v1 could not make. `IsValidTag` answers whether the cards a training produced
// carry a friendship, and the tag cut-in player is the door a friendship cut-in is played through.
static TAG_IS_VALID_TAG: CutProbe = CutProbe::counted("SingleModeMainTrainingCuttController::IsValidTag(result, cards)");
static TAG_PLAYER_IS_VALID_TAG: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::IsValidTag(cards)");
static TAG_PLAYER_PLAY_CUT_IN: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::PlayCutIn(cards, done)");
static TAG_PLAYER_PLAY_CUT_OUT: CutProbe = CutProbe::counted("SingleModeMainViewTagTrainingCutInPlayer::PlayCutInOut(done)");

static PROBES: [&CutProbe; 71] = [
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
    &TAG_IS_VALID_TAG,
    &TAG_PLAYER_IS_VALID_TAG,
    &TAG_PLAYER_PLAY_CUT_IN,
    &TAG_PLAYER_PLAY_CUT_OUT,
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
const VIEW_STORY_EVENT: i32 = ViewId::StoryEventMission as i32;
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
    // event probe is measuring.
    else if view_id == VIEW_STORY_EVENT {
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
// session. `high_speed_settings` writes the game's own story and training HighSpeedType values, and
// run 16 logged its raise at 09:00:22 and its restore at 09:03:24 with one of these screens up,
// inside a window where a training cut runtime never terminated. Both values live on the save loader
// and in StoryManager's saved setting, so the write the next turn reads is the one taken before the
// turn, not the one taken during it. The ids come from the game's own `ViewId`, so a client that
// moves one shows up here as a compile error rather than as a gate that quietly holds nothing.
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
// each gap at the plate list calls seen inside it: the wait before the plate list, the plate list pass
// itself, and the time from its last call to the next cut opening. All three come from one clock and
// always add up to the gap, so whatever is left over shows as a leg instead of being guessed at. A gap
// with no plate list call between its ends is counted apart rather than given invented legs.
static GAP_PLATE_FIRST_MS: AtomicI64 = AtomicI64::new(-1);
static GAP_PLATE_LAST_MS: AtomicI64 = AtomicI64::new(-1);
static GAP_LEG_BEFORE_RUNS: AtomicUsize = AtomicUsize::new(0);
static GAP_LEG_BEFORE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_BEFORE_WORST_MS: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_PLATE_RUNS: AtomicUsize = AtomicUsize::new(0);
static GAP_LEG_PLATE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static GAP_LEG_PLATE_WORST_MS: AtomicI64 = AtomicI64::new(0);
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

// The wall between a training cut-in finishing and the status panel playing out. Both training heavy runs
// put nearly all of a slow turn here: 9,740 ms in run 14 and 7,221 ms in run 15, with no door reached in
// between, while the frame clock drew the whole way through at about 175 frames a second. The mark is
// made by the story event probe's training cut-in door and consumed by the status play out door, so a cut
// reports at most one hole and a cut that plays out at once reports none.
static CUT_HOLE_FROM_MS: AtomicI64 = AtomicI64::new(-1);
static CUT_HOLE_RUNS: AtomicUsize = AtomicUsize::new(0);
static CUT_HOLE_MS_TOTAL: AtomicI64 = AtomicI64::new(0);
static CUT_HOLE_WORST_MS: AtomicI64 = AtomicI64::new(0);

// A span from a door the game reached to the point the run ended, guarded against a request that never
// happened or landed after the close. Kept apart from the callers so the guard is a test.
fn span_from(requested: i64, now: i64) -> Option<i64> {
    if requested < 0 || now < requested {
        return None;
    }

    Some(now - requested)
}

/// One gap cut into its three legs at the plate list calls seen inside it. Every mark is read from the
/// same clock, so a mark out of order means the pairing is wrong and the gap is left whole: a leg made
/// from bad marks is worse than a gap nobody explained.
fn gap_legs(close_ms: i64, first_plate_ms: i64, last_plate_ms: i64, open_ms: i64) -> Option<(i64, i64, i64)> {
    let before = span_from(close_ms, first_plate_ms)?;
    let plate = span_from(first_plate_ms, last_plate_ms)?;
    let after = span_from(last_plate_ms, open_ms)?;

    Some((before, plate, after))
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

// The gap belongs to the plate list door the probe already stands on, so splitting it costs no new hook:
// a `InitializePlateList` call between a close and the next open says the game was building param plates
// during that idle, and the time on either side of those calls is the time it was not.
fn record_gap_legs(close_ms: i64, open_ms: i64) {
    let first_plate = GAP_PLATE_FIRST_MS.swap(-1, atomic::Ordering::Relaxed);
    let last_plate = GAP_PLATE_LAST_MS.swap(-1, atomic::Ordering::Relaxed);

    match gap_legs(close_ms, first_plate, last_plate, open_ms) {
        Some((before, plate, after)) => {
            GAP_LEG_BEFORE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
            GAP_LEG_BEFORE_MS_TOTAL.fetch_add(before, atomic::Ordering::Relaxed);
            GAP_LEG_BEFORE_WORST_MS.fetch_max(before, atomic::Ordering::Relaxed);
            GAP_LEG_PLATE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
            GAP_LEG_PLATE_MS_TOTAL.fetch_add(plate, atomic::Ordering::Relaxed);
            GAP_LEG_PLATE_WORST_MS.fetch_max(plate, atomic::Ordering::Relaxed);
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

    // A new gap begins at this close, so the plate list marks of the previous one are dropped rather than
    // carried into it.
    GAP_PLATE_FIRST_MS.store(-1, atomic::Ordering::Relaxed);
    GAP_PLATE_LAST_MS.store(-1, atomic::Ordering::Relaxed);

    // The teardown is also where a cascade that never reported its typewrite end stops being open. A
    // window left open would charge the next turn's first plate event to the cascade before it.
    close_plate_pass();

    // A hole is only a hole inside one cut, so a cut-in end that never reached a play out is dropped
    // here rather than paired with the next cut's.
    CUT_HOLE_FROM_MS.store(-1, atomic::Ordering::Relaxed);

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

type GetTrainingCutTimeScaleFn = extern "C" fn(scale: f32) -> f32;
// Dumped static: `GetTrainingCutTimeScale/1 -> static float(float)`, so the wrapper declares the
// dumped argument and no `this` (A3).
def_detour! {
    TrainingCuttUtils_GetTrainingCutTimeScale(scale: f32) -> f32 {
            let value = get_orig_fn!(TrainingCuttUtils_GetTrainingCutTimeScale, GetTrainingCutTimeScaleFn)(scale);
        GET_TRAINING_CUT_TIME_SCALE.observe_peak(&[scale as f64, value as f64], value);

        value
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

type CuttUpdateSpeedFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    CuttTimeline_UpdateSpeed(this: *mut Il2CppObject) {
            CUTT_UPDATE_SPEED.count();

        get_orig_fn!(CuttTimeline_UpdateSpeed, CuttUpdateSpeedFn)(this);
    }
    bail {
                get_orig_fn!(CuttTimeline_UpdateSpeed, CuttUpdateSpeedFn)(this)
    }
}

type CuttSkipRuntimeTimeFn = extern "C" fn(this: *mut Il2CppObject, time: f32);
def_detour! {
    CuttTimeline_SkipRuntimeTime(this: *mut Il2CppObject, time: f32) {
            CUTT_SKIP_RUNTIME_TIME.observe(&[time as f64]);

        get_orig_fn!(CuttTimeline_SkipRuntimeTime, CuttSkipRuntimeTimeFn)(this, time);
    }
    bail {
                get_orig_fn!(CuttTimeline_SkipRuntimeTime, CuttSkipRuntimeTimeFn)(this, time)
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
            CUTT_SKIP_TIME_DIRECT.observe(&[time as f64]);

        get_orig_fn!(CuttTimeline_SkipTimeDirect, CuttSkipTimeDirectFn)(this, time);
    }
    bail {
                get_orig_fn!(CuttTimeline_SkipTimeDirect, CuttSkipTimeDirectFn)(this, time)
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
    open_plate_pass();
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
        open_plate_pass();

        get_orig_fn!(TrainingParamChangeUI_StartSequence, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_StartSequence, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_StartGroupTypewrite(this: *mut Il2CppObject) {
            PLATE_START_GROUP_TYPEWRITE.count();

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

        get_orig_fn!(TrainingParamChangeUI_StartTypewrite, PlateIntFn)(this, index);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_StartTypewrite, PlateIntFn)(this, index)
    }
}

def_detour! {
    TrainingParamChangeUI_OnNextTypewrite(this: *mut Il2CppObject) {
            PLATE_ON_NEXT_TYPEWRITE.count();

        get_orig_fn!(TrainingParamChangeUI_OnNextTypewrite, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnNextTypewrite, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_OnEndTypeWrite(this: *mut Il2CppObject, index: i32) {
            PLATE_ON_END_TYPE_WRITE.observe(&[index as f64]);

        get_orig_fn!(TrainingParamChangeUI_OnEndTypeWrite, PlateIntFn)(this, index);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnEndTypeWrite, PlateIntFn)(this, index)
    }
}

def_detour! {
    TrainingParamChangeUI_OnAllTypewriteEnd(this: *mut Il2CppObject) {
            PLATE_ON_ALL_TYPEWRITE_END.count();
        close_plate_pass();

        get_orig_fn!(TrainingParamChangeUI_OnAllTypewriteEnd, PlateVoidFn)(this);
    }
    bail {
                get_orig_fn!(TrainingParamChangeUI_OnAllTypewriteEnd, PlateVoidFn)(this)
    }
}

def_detour! {
    TrainingParamChangeUI_OnTapScreen(this: *mut Il2CppObject) {
            PLATE_ON_TAP_SCREEN.count();

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

    // The same door is the split point of the gap when no cut is open. A plate list pass during a cut
    // belongs to that cut's wall, so it is not charged to the idle between cuts.
    if RUN_OPENED_MS.load(atomic::Ordering::Relaxed) < 0 && RUN_CLOSED_MS.load(atomic::Ordering::Relaxed) >= 0 {
        if GAP_PLATE_FIRST_MS.load(atomic::Ordering::Relaxed) < 0 {
            GAP_PLATE_FIRST_MS.store(now, atomic::Ordering::Relaxed);
        }

        GAP_PLATE_LAST_MS.store(now, atomic::Ordering::Relaxed);
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
fn open_plate_pass() {
    if PLATE_PASS_OPEN_MS.load(atomic::Ordering::Relaxed) < 0 {
        PLATE_PASS_OPEN_MS.store(elapsed_ms(), atomic::Ordering::Relaxed);
    }
}

fn close_plate_pass() {
    let opened = PLATE_PASS_OPEN_MS.swap(-1, atomic::Ordering::Relaxed);
    PLATE_TYPEWRITE_LAST_MS.store(-1, atomic::Ordering::Relaxed);

    if let Some(span_ms) = span_from(opened, elapsed_ms()) {
        PLATE_PASS_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        PLATE_PASS_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
        PLATE_PASS_WORST_MS.fetch_max(span_ms, atomic::Ordering::Relaxed);
    }
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

// Marks the moment a training cut-in finished. The door for it lives in `StoryEventProbe` because that is
// where the cut-in helper doors are, and the hole it opens is measured here because the other end of it,
// the status panel playing out, is a door this probe owns.
pub(crate) fn note_cut_in_end() {
    // A second cut-in ending while an earlier hole is still open means that hole never reached its status
    // play out. Its tally is dropped rather than carried into the new window.
    close_hole_census();
    CUT_HOLE_FROM_MS.store(elapsed_ms(), atomic::Ordering::Relaxed);
    open_hole_census();
    super::CutStateProbe::note_hole_open();
}

// Closes the hole at the status play out. The mark is swapped out rather than read so a cut that calls
// `PlayOut` twice, which run 15 did twice, charges the wait once.
fn record_cut_hole() {
    let from_ms = CUT_HOLE_FROM_MS.swap(-1, atomic::Ordering::Relaxed);

    if let Some(span_ms) = span_from(from_ms, elapsed_ms()) {
        CUT_HOLE_RUNS.fetch_add(1, atomic::Ordering::Relaxed);
        CUT_HOLE_MS_TOTAL.fetch_add(span_ms, atomic::Ordering::Relaxed);
        CUT_HOLE_WORST_MS.fetch_max(span_ms, atomic::Ordering::Relaxed);
        record_in_window(|window| {
            window.holes += 1;
            window.hole_ms += span_ms;
        });
        // Which doors the wait crossed, said once, at the moment the wait ends.
        if let Some(table) = close_hole_census() {
            info!("{}", hole_census_line(span_ms, &table));
        }
        // The same stretch read off the cut's own coroutine: whether the engine kept calling it while the
        // status panel was held off decides whether this is a wait the fork can reach at all.
        super::CutStateProbe::note_hole_closed(span_ms);
    }
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
// printed nothing for a whole career (C49), so it no longer drives the run. It stays counted because a
// client that does use the flag would show up here, and the value is handed back exactly as it arrived.
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
            CUTT_CLEAN_UP_CUTT.count();
        close_cut_run();

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
            CUTT_PLAY_TRAINING_CUT.count();
        open_cut_run();

        // The coroutine the game just built is the state machine of the whole cut, and the values it captured
        // are the ones the game computed for this cut. They are read after the original ran, and the object is
        // handed back exactly as it came (CutStateProbe).
        let coroutine = get_orig_fn!(TrainingCutt_PlayTrainingCut, PlayTrainingCutFn)(this, info);
        super::CutStateProbe::note_play_training_cut(coroutine);

        coroutine
    }
}

def_detour! {
    TrainingCutt_PlayScenarioTrainingCut(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject {
            CUTT_PLAY_SCENARIO_TRAINING_CUT.count();
        open_cut_run();

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
        record_cut_hole();

        get_orig_fn!(TrainingCutStatus_PlayOut, StatusPlayOutFn)(this, flag, action);
    }
    bail {
                get_orig_fn!(TrainingCutStatus_PlayOut, StatusPlayOutFn)(this, flag, action)
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
    let hp_gauge = class_for_label(umamusume, "Gallop.SingleModeMainViewHpGauge");
    let main_view = class_for_label(umamusume, "Gallop.SingleModeMainViewController");
    let tag_player = class_for_label(umamusume, "Gallop.SingleModeMainViewTagTrainingCutInPlayer");
    // The class that owns the stat change presentation for a training turn: its coroutine
    // `PlayParameterChangeAsync/2` is the level above the plate list door.
    let story_view = class_for_label(umamusume, "Gallop.StoryViewController");

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

    probe!(cut_status, TrainingCutStatus_Skip, "Skip", ONE_FLAG, VOID, "SingleModeMainViewTrainingCutStatus::Skip");
    // `PlayIn` is not probed here any more: AnimationSpeed scales it, and its count and its raw value
    // reach this report through TRAINING_HIT_SLOTS (C51).
    probe!(cut_status, TrainingCutStatus_PlayOut, "PlayOut", FLAG_AND_ACTION, VOID, "SingleModeMainViewTrainingCutStatus::PlayOut");
    probe!(cut_status, TrainingCutStatus_GetIntervalOutBegine, "GetIntervalOutBegine", ONE_FLOAT, R4, "SingleModeMainViewTrainingCutStatus::GetIntervalOutBegine");
    probe!(cut_status, TrainingCutStatus_WillRankUpInHighSpeedMode, "WillRankUpInHighSpeedMode", NO_PARAMS, BOOL, "SingleModeMainViewTrainingCutStatus::WillRankUpInHighSpeedMode");
    probe!(cut_status, TrainingCutStatus_ExistPlayingFrame, "ExistPlayingFrame", NO_PARAMS, BOOL, "SingleModeMainViewTrainingCutStatus::ExistPlayingFrame");

    probe!(cutt_controller, TrainingCutt_WaitTapAsync, "WaitTapAsync", NO_PARAMS, CLASS, "SingleModeMainTrainingCuttController::WaitTapAsync");
    probe!(cutt_controller, TrainingCutt_FadeOutResultFlash, "FadeOutResultFlash", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::FadeOutResultFlash");

    // The boundary v1 had to guess at, and the doors that say which cut is playing.
    probe!(cutt_controller, TrainingCutt_SetIsPlayingCutt, "set_IsPlayingCutt", ONE_FLAG, VOID, "SingleModeMainTrainingCuttController::set_IsPlayingCutt");
    probe!(cutt_controller, TrainingCutt_GetIsPlayingCutt, "get_IsPlayingCutt", NO_PARAMS, BOOL, "SingleModeMainTrainingCuttController::get_IsPlayingCutt");
    probe!(cutt_controller, TrainingCutt_IsAutoPlay, "IsAutoPlay", NO_PARAMS, BOOL, "SingleModeMainTrainingCuttController::IsAutoPlay");
    probe!(cutt_controller, TrainingCutt_UpdateTrainingCutIn, "UpdateTrainingCutIn", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::UpdateTrainingCutIn");
    probe!(cutt_controller, TrainingCutt_FixedUpdateTrainingCutIn, "FixedUpdateTrainingCutIn", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::FixedUpdateTrainingCutIn");
    probe!(cutt_controller, TrainingCutt_LateUpdateTrainingCutIn, "LateUpdateTrainingCutIn", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::LateUpdateTrainingCutIn");
    probe!(cutt_controller, TrainingCutt_PlayTrainingCut, "PlayTrainingCut", ONE_INFO, CLASS, "SingleModeMainTrainingCuttController::PlayTrainingCut");
    probe!(cutt_controller, TrainingCutt_PlayScenarioTrainingCut, "PlayScenarioTrainingCut", ONE_INFO, CLASS, "SingleModeMainTrainingCuttController::PlayScenarioTrainingCut");
    probe!(cutt_controller, TrainingCutt_PlayTrainingSaboriAsync, "PlayTrainingSaboriAsync", ONE_ID, CLASS, "SingleModeMainTrainingCuttController::PlayTrainingSaboriAsync");
    probe!(cutt_controller, TrainingCutt_PlayTrainingCutEndAsync, "PlayTrainingCutEndAsync", ID_AND_FLAGS, CLASS, "SingleModeMainTrainingCuttController::PlayTrainingCutEndAsync");
    probe!(cutt_controller, TrainingCutt_TrainingAsync, "TrainingAsync", ONE_ID, CLASS, "SingleModeMainTrainingCuttController::TrainingAsync");
    probe!(cutt_controller, TrainingCutt_PlayInTrainingStatus, "PlayInTrainingStatus", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::PlayInTrainingStatus");
    probe!(cutt_controller, TrainingCutt_PlayOutTrainingStatus, "PlayOutTrainingStatus", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::PlayOutTrainingStatus");
    probe!(cutt_controller, TrainingCutt_CleanUpCutt, "CleanUpCutt", NO_PARAMS, VOID, "SingleModeMainTrainingCuttController::CleanUpCutt");
    generic_probe!(cutt_controller, TrainingCutt_IsValidTag, "IsValidTag", RESULT_AND_LIST, BOOL, "SingleModeMainTrainingCuttController::IsValidTag");

    // The friendship door (A28). `PlayCutIn` and the static `IsValidTag` are the two places a tag cut-in is
    // named by the game itself rather than by a class label.
    generic_probe!(tag_player, TagCutInPlayer_PlayCutIn, "PlayCutIn", LIST_AND_ACTION, VOID, "SingleModeMainViewTagTrainingCutInPlayer::PlayCutIn");
    probe!(tag_player, TagCutInPlayer_PlayCutInOut, "PlayCutInOut", ONE_ACTION, VOID, "SingleModeMainViewTagTrainingCutInPlayer::PlayCutInOut");
    static_generic_probe!(tag_player, TagCutInPlayer_IsValidTag, "IsValidTag", ONE_INFO, BOOL, "SingleModeMainViewTagTrainingCutInPlayer::IsValidTag");

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

    info!("Cutt probe: {installed} doors installed, {} of them carry a counted kind in the totals line, cut runs measured from the cut start doors to CleanUpCutt and attributed to the view they start on", PROBES.len());

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

    // The scaling points the mod already installs on the training screen. They printed install lines in
    // run 8 and no call line, and this probe cannot hook those addresses a second time, so their own
    // call counts ride along on this line (A21).
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
    let mean_ms = |total: i64, count: usize| match count {
        0 => 0.0,
        n => total as f64 / n as f64,
    };

    info!(
        "Cutt probe cut clocks: tap waits {tap_runs} of {runs} closes mean {:.1} ms, gaps between cuts {gap_runs} mean {:.1} ms worst {} ms, plate list steps {plate_runs} mean {:.1} ms worst {} ms",
        mean_ms(tap_ms, tap_runs),
        mean_ms(gap_ms, gap_runs),
        CUT_GAP_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_ms(plate_ms, plate_runs),
        PLATE_STEP_WORST_MS.load(atomic::Ordering::Relaxed)
    );

    // The gap and the wall above, split at the doors inside them. This is the line that says whether the
    // next lever should aim at the plate list, at what the game does after the plate list, or at the cut
    // itself, and it costs no hook because all three marks come from doors the probe already stands on.
    let leg_runs = GAP_LEG_BEFORE_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe gap legs: {leg_runs} of {gap_runs} gaps held a plate list pass, before it mean {:.1} ms worst {} ms, the pass itself mean {:.1} ms worst {} ms, after it mean {:.1} ms worst {} ms, {} gaps held none",
        mean_ms(GAP_LEG_BEFORE_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
        GAP_LEG_BEFORE_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_ms(GAP_LEG_PLATE_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
        GAP_LEG_PLATE_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_ms(GAP_LEG_AFTER_MS_TOTAL.load(atomic::Ordering::Relaxed), leg_runs),
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
        mean_ms(SEND_TO_OPEN_MS_TOTAL.load(atomic::Ordering::Relaxed), send_to_open_runs),
        SEND_TO_OPEN_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_ms(CLOSE_TO_SEND_MS_TOTAL.load(atomic::Ordering::Relaxed), close_to_send_runs),
        CLOSE_TO_SEND_WORST_MS.load(atomic::Ordering::Relaxed)
    );

    let played_runs = WALL_OPEN_TO_TAP_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe cut wall legs: {played_runs} of {runs} closes asked for a tap, the cut played mean {:.1} ms worst {} ms before it asked",
        mean_ms(WALL_OPEN_TO_TAP_MS_TOTAL.load(atomic::Ordering::Relaxed), played_runs),
        WALL_OPEN_TO_TAP_WORST_MS.load(atomic::Ordering::Relaxed)
    );
    let gauge_runs = GAUGE_PLAY_RUNS.load(atomic::Ordering::Relaxed);
    let icon_runs = PLATE_PLAY_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe plate cadence: gauge plays {gauge_runs} inside a cut or a cascade mean {:.1} ms worst {} ms, plate icon plays {icon_runs} inside a cut or a cascade mean {:.1} ms worst {} ms",
        mean_ms(GAUGE_PLAY_MS_TOTAL.load(atomic::Ordering::Relaxed), gauge_runs),
        GAUGE_PLAY_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_ms(PLATE_PLAY_MS_TOTAL.load(atomic::Ordering::Relaxed), icon_runs),
        PLATE_PLAY_WORST_MS.load(atomic::Ordering::Relaxed)
    );

    // How long one cascade of stat plates takes, and how far apart its plates arrive. Run 14 answered
    // neither: it counted 22 plate list calls 5,129.3 ms apart and said nothing about the time inside one
    // cascade, which is the difference between a cascade that runs for seconds and a controller that
    // starts a new one every few seconds. Those two need different doors (C54).
    let pass_runs = PLATE_PASS_RUNS.load(atomic::Ordering::Relaxed);
    let typewrite_runs = PLATE_TYPEWRITE_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe plate cascade: {pass_runs} cascades closed mean {:.1} ms worst {} ms, typewrite starts {typewrite_runs} inside a cascade mean {:.1} ms worst {} ms",
        mean_ms(PLATE_PASS_MS_TOTAL.load(atomic::Ordering::Relaxed), pass_runs),
        PLATE_PASS_WORST_MS.load(atomic::Ordering::Relaxed),
        mean_ms(PLATE_TYPEWRITE_MS_TOTAL.load(atomic::Ordering::Relaxed), typewrite_runs),
        PLATE_TYPEWRITE_WORST_MS.load(atomic::Ordering::Relaxed)
    );
    let hole_runs = CUT_HOLE_RUNS.load(atomic::Ordering::Relaxed);

    info!(
        "Cutt probe cut holes: {hole_runs} holes mean {:.1} ms worst {} ms from the training cut-in ending to the status play out",
        mean_ms(CUT_HOLE_MS_TOTAL.load(atomic::Ordering::Relaxed), hole_runs),
        CUT_HOLE_WORST_MS.load(atomic::Ordering::Relaxed)
    );
    info!("Cutt probe cut kinds:{kinds} timeline self report total frames peak {total_frames} last frame peak {last_frame} target fps {target_fps}");
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

        let line = hole_census_line(6583, &table);
        assert!(line.contains("census door crossed three ways = 3"), "the census line names the door and its crossings: {line}");
        assert!(line.contains("over 6583 ms"), "the census line carries the hole it censuses: {line}");
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

        let line = hole_census_line(3400, &table);
        assert!(line.contains("census clock door handed times = 4 (1.5 to 6 handed)"), "the census line prints the spread next to the crossings: {line}");
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

    #[test]
    fn peak_merge_keeps_the_largest_finite_non_negative_value() {
        let after = peak_merge(0, 1.5);

        assert_eq!(f32::from_bits(peak_merge(after, 0.25)), 1.5);
        assert_eq!(f32::from_bits(peak_merge(after, 3.0)), 3.0);
        assert_eq!(peak_merge(after, f32::NAN), after);
        assert_eq!(peak_merge(after, -8.0), after);
    }

    #[test]
    fn a_gap_splits_at_the_plate_list_calls_inside_it() {
        // The shape run 12 read four times: a close, a plate list pass during the idle, then the next cut
        // opening. The three legs are the whole gap, so whatever the plate list was not doing stays
        // visible as time on one side of it instead of disappearing into a mean.
        let legs = gap_legs(1_000, 2_000, 6_000, 9_000).expect("a gap with a plate list pass between its ends has legs");

        assert_eq!(legs, (1_000, 4_000, 3_000), "before the plate list, the pass itself, after it");
        assert_eq!(legs.0 + legs.1 + legs.2, 9_000 - 1_000, "the legs are the gap, with nothing invented and nothing lost");
    }

    #[test]
    fn a_plate_list_mark_out_of_order_leaves_the_gap_whole() {
        // Every mark comes from one clock, so a mark outside the gap is a pairing this probe did not earn.
        // A leg made from it would move the claim rather than support it.
        assert_eq!(gap_legs(1_000, 500, 6_000, 9_000), None, "a plate list call before the close is not in the gap");
        assert_eq!(gap_legs(1_000, 2_000, 12_000, 9_000), None, "a plate list call after the next open is not in the gap");
        assert_eq!(gap_legs(-1, 2_000, 6_000, 9_000), None, "a gap with no close is not a gap");
    }

    #[test]
    fn a_gap_with_no_plate_list_pass_counts_as_one_gap() {
        // The caller counts these apart so the report cannot read three legs of zero as an explanation of
        // an idle the probe never saw a door in.
        assert_eq!(gap_legs(1_000, -1, -1, 9_000), None);
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
    fn a_cut_run_is_attributed_to_the_screen_it_started_on() {
        assert_eq!(bucket_for(ViewId::SingleModeMain as i32, false), ViewBucket::Training);
        assert_eq!(bucket_for(ViewId::SingleModePaddock as i32, false), ViewBucket::Training);
        assert_eq!(bucket_for(ViewId::SingleModeResult as i32, false), ViewBucket::Training);
        assert_eq!(bucket_for(ViewId::Story as i32, false), ViewBucket::Story);
        // The story event mission screen is its own view, and the story event probe reports against it.
        assert_eq!(bucket_for(ViewId::StoryEventMission as i32, false), ViewBucket::StoryEvent);
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
}
