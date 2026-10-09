use std::ffi::CString;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicU32, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{AnimationSpeed, SceneDefine::ViewId, SceneManager},
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
const ONE_ACTION: &[Il2CppTypeEnum] = &[CLASS];
// Dumped as `void(generic<System.Collections.Generic.List<...>:24B>, float)`. The dump spells the
// first parameter as a generic, and the generic resolver is what answers it (C48).
const LIST_AND_FLOAT: &[Il2CppTypeEnum] = &[CLASS, R4];
const ID_AND_FLAGS: &[Il2CppTypeEnum] = &[VALUETYPE, BOOL, BOOL];
const RESULT_AND_LIST: &[Il2CppTypeEnum] = &[VALUETYPE, CLASS];
const FRAMES_AND_FLAG: &[Il2CppTypeEnum] = &[I4, BOOL];
// `PlayCutIn/2 -> void(generic<List<SupportCardData>>, class<System.Action>)`.
const LIST_AND_ACTION: &[Il2CppTypeEnum] = &[CLASS, CLASS];
// `PlayOut/2 -> void(bool, class<System.Action>)`.
const FLAG_AND_ACTION: &[Il2CppTypeEnum] = &[BOOL, CLASS];

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

    // The frame hot shape: one increment, one max, no slice and no formatting until a chunk boundary.
    pub(crate) fn sample(&self, value: f32) {
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        if self.peaked {
            let bits = peak_merge(self.peak.load(atomic::Ordering::Relaxed), value);
            self.peak.fetch_max(bits, atomic::Ordering::Relaxed);
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

static PROBES: [&CutProbe; 52] = [
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

// One view read per cut run, not per frame. `GetCurrentViewId` is the method the mod already resolves
// for other features, and its wrapper refuses an unresolved address instead of jumping to 0 (C1).
pub(crate) fn current_view_id() -> i32 {
    let scene_manager = SceneManager::instance();

    if scene_manager.is_null() {
        return 0;
    }

    SceneManager::GetCurrentViewId(scene_manager)
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

// The animation doors run 11 never looked at, on the classes that hold the gauge and the param plates.
// Counting only: an argument is recorded and handed back untouched.
static HP_GAUGE_PLAY_IN: CutProbe = CutProbe::counted("SingleModeMainViewHpGauge::PlayIn()");
static HP_GAUGE_PLAY_VALUE: CutProbe = CutProbe::counted("SingleModeMainViewHpGauge::PlayValue(value)");
static HP_GAUGE_PLAY_OUT: CutProbe = CutProbe::counted("SingleModeMainViewHpGauge::PlayOut()");
static STATUS_PLAY_PRE_IN: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::PlayPreIn()");
static STATUS_PLAY_END: CutProbe = CutProbe::counted("SingleModeMainViewTrainingCutStatus::PlayEnd()");
static PLATE_GET_IS_AUTO_PLAY: CutProbe = CutProbe::counted("TrainingParamChangeUI::get_IsAutoPlay()");

// A span from a door the game reached to the point the run ended, guarded against a request that never
// happened or landed after the close. Kept apart from the callers so the guard is a test.
fn span_from(requested: i64, now: i64) -> Option<i64> {
    if requested < 0 || now < requested {
        return None;
    }

    Some(now - requested)
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
    if let Some(gap_ms) = span_from(RUN_CLOSED_MS.swap(-1, atomic::Ordering::Relaxed), now) {
        let gaps = CUT_GAP_RUNS.fetch_add(1, atomic::Ordering::Relaxed) + 1;

        CUT_GAP_MS_TOTAL.fetch_add(gap_ms, atomic::Ordering::Relaxed);
        CUT_GAP_BUCKET_MS[bucket as usize].fetch_add(gap_ms, atomic::Ordering::Relaxed);
        CUT_GAP_WORST_MS.fetch_max(gap_ms, atomic::Ordering::Relaxed);

        if gaps <= PROBE_DETAIL_LIMIT {
            info!("Cutt probe: cut gap {gap_ms} ms from the previous close to this open on view {view} {}", BUCKET_NAMES[bucket as usize]);
        }
    }
}

// Called from `CleanUpCutt`, which run 10 measured 35 times against 19 cut starts. A close with nothing open
// is the game cleaning a cutt it already cleaned, so it is not counted as a run.
fn close_cut_run() {
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

    RUN_CLOSED_MS.store(now, atomic::Ordering::Relaxed);

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
// Kept as a count, not as the run boundary. Run 9 showed a training cut-in never reaches it, which is why
// `cut runs` read 0 while the timeline was reached 3194 times (C47). A cut-in that does reset the timeline
// is still visible here.
extern "C" fn CuttTimeline_ResetCurrentTime(this: *mut Il2CppObject) {
    CUTT_RESET_CURRENT_TIME.count();

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

    // Where the tap wait clock starts. Only while a run is open, so a request outside a measured cut
    // cannot be charged to one.
    if RUN_OPENED_MS.load(atomic::Ordering::Relaxed) >= 0 {
        RUN_TAP_REQUESTED_MS.store(elapsed_ms(), atomic::Ordering::Relaxed);
    }

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
    record_plate_step();

    get_orig_fn!(TrainingParamChangeUI_InitializePlateList, InitializePlateListFn)(this, list, interval);
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
}

// The gauge and plate animation doors run 11 never opened. Every argument is handed to the original
// untouched. `SetProgressbarBlendTime` records its float because it is a duration, and a scaling hook
// only belongs on it once a run shows how often the game reaches it.
type HpGaugePlayInFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn HpGauge_PlayIn(this: *mut Il2CppObject) {
    HP_GAUGE_PLAY_IN.count();

    get_orig_fn!(HpGauge_PlayIn, HpGaugePlayInFn)(this);
}

type HpGaugePlayValueFn = extern "C" fn(this: *mut Il2CppObject, value: i32);
extern "C" fn HpGauge_PlayValue(this: *mut Il2CppObject, value: i32) {
    HP_GAUGE_PLAY_VALUE.observe(&[value as f64]);

    get_orig_fn!(HpGauge_PlayValue, HpGaugePlayValueFn)(this, value);
}

type HpGaugePlayOutFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn HpGauge_PlayOut(this: *mut Il2CppObject) {
    HP_GAUGE_PLAY_OUT.count();

    get_orig_fn!(HpGauge_PlayOut, HpGaugePlayOutFn)(this);
}

type StatusPlayPreInFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn TrainingCutStatus_PlayPreIn(this: *mut Il2CppObject) {
    STATUS_PLAY_PRE_IN.count();

    get_orig_fn!(TrainingCutStatus_PlayPreIn, StatusPlayPreInFn)(this);
}

type StatusPlayEndFn = extern "C" fn(this: *mut Il2CppObject);
extern "C" fn TrainingCutStatus_PlayEnd(this: *mut Il2CppObject) {
    STATUS_PLAY_END.count();

    get_orig_fn!(TrainingCutStatus_PlayEnd, StatusPlayEndFn)(this);
}

type PlateGetIsAutoPlayFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
extern "C" fn PlateUI_GetIsAutoPlay(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(PlateUI_GetIsAutoPlay, PlateGetIsAutoPlayFn)(this);
    PLATE_GET_IS_AUTO_PLAY.observe(&[bit(value)]);

    value
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

type SetIsPlayingCuttFn = extern "C" fn(this: *mut Il2CppObject, playing: bool);
// Dumped: `set_IsPlayingCutt/1 -> void(bool)` on the training cutt controller. Run 10 installed it and it
// printed nothing for a whole career (C49), so it no longer drives the run. It stays counted because a
// client that does use the flag would show up here, and the value is handed back exactly as it arrived.
extern "C" fn TrainingCutt_SetIsPlayingCutt(this: *mut Il2CppObject, playing: bool) {
    CUTT_SET_IS_PLAYING_CUTT.count();

    get_orig_fn!(TrainingCutt_SetIsPlayingCutt, SetIsPlayingCuttFn)(this, playing);
}

type IsPlayingCuttFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
extern "C" fn TrainingCutt_GetIsPlayingCutt(this: *mut Il2CppObject) -> bool {
    CUTT_GET_IS_PLAYING_CUTT.count();

    get_orig_fn!(TrainingCutt_GetIsPlayingCutt, IsPlayingCuttFn)(this)
}

type CuttBoolFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
// `IsAutoPlay/0 -> bool()`: whether the cut-in is already set to play without a tap. Sampled as a peak
// of 1.0 so the totals line answers "was it ever on" without a line per read.
extern "C" fn TrainingCutt_IsAutoPlay(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(TrainingCutt_IsAutoPlay, CuttBoolFn)(this);
    CUTT_IS_AUTO_PLAY.sample(flag_bit(value));

    value
}

type CuttVoidFn = extern "C" fn(this: *mut Il2CppObject);
// The three drivers the cut-in runs through. Counted rather than sampled: these are the frames the
// animation actually spends, and a run's cost is the count of them.
extern "C" fn TrainingCutt_UpdateTrainingCutIn(this: *mut Il2CppObject) {
    CUTT_UPDATE_TRAINING_CUT_IN.count();

    get_orig_fn!(TrainingCutt_UpdateTrainingCutIn, CuttVoidFn)(this);
}

extern "C" fn TrainingCutt_FixedUpdateTrainingCutIn(this: *mut Il2CppObject) {
    CUTT_FIXED_UPDATE_TRAINING_CUT_IN.count();

    get_orig_fn!(TrainingCutt_FixedUpdateTrainingCutIn, CuttVoidFn)(this);
}

extern "C" fn TrainingCutt_LateUpdateTrainingCutIn(this: *mut Il2CppObject) {
    CUTT_LATE_UPDATE_TRAINING_CUT_IN.count();

    get_orig_fn!(TrainingCutt_LateUpdateTrainingCutIn, CuttVoidFn)(this);
}

extern "C" fn TrainingCutt_CleanUpCutt(this: *mut Il2CppObject) {
    CUTT_CLEAN_UP_CUTT.count();
    close_cut_run();

    get_orig_fn!(TrainingCutt_CleanUpCutt, CuttVoidFn)(this);
}

extern "C" fn TrainingCutt_PlayInTrainingStatus(this: *mut Il2CppObject) {
    CUTT_PLAY_IN_TRAINING_STATUS.count();

    get_orig_fn!(TrainingCutt_PlayInTrainingStatus, CuttVoidFn)(this);
}

extern "C" fn TrainingCutt_PlayOutTrainingStatus(this: *mut Il2CppObject) {
    CUTT_PLAY_OUT_TRAINING_STATUS.count();

    get_orig_fn!(TrainingCutt_PlayOutTrainingStatus, CuttVoidFn)(this);
}

type PlayTrainingCutFn = extern "C" fn(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject;
// Dumped: `PlayTrainingCut/1 -> IEnumerator(class<Gallop.SingleModeMainTrainingCuttController.CuttPlayInfo>)`.
// The coroutine object is created by the original and handed straight back, so nothing here changes what
// the game ends up playing.
extern "C" fn TrainingCutt_PlayTrainingCut(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject {
    CUTT_PLAY_TRAINING_CUT.count();
    open_cut_run();

    get_orig_fn!(TrainingCutt_PlayTrainingCut, PlayTrainingCutFn)(this, info)
}

extern "C" fn TrainingCutt_PlayScenarioTrainingCut(this: *mut Il2CppObject, info: *mut Il2CppObject) -> *mut Il2CppObject {
    CUTT_PLAY_SCENARIO_TRAINING_CUT.count();
    open_cut_run();

    get_orig_fn!(TrainingCutt_PlayScenarioTrainingCut, PlayTrainingCutFn)(this, info)
}

type TrainingIdCoroutineFn = extern "C" fn(this: *mut Il2CppObject, id: i32) -> *mut Il2CppObject;
// Dumped: `TrainingAsync/1 -> IEnumerator(struct<Gallop.TrainingDefine.TrainingCommandId:4B>)`. A four
// byte struct travels in a general purpose register (A5) and this wrapper only passes it through, so it
// never has to interpret what the id means.
extern "C" fn TrainingCutt_TrainingAsync(this: *mut Il2CppObject, id: i32) -> *mut Il2CppObject {
    CUTT_TRAINING_ASYNC.count();

    get_orig_fn!(TrainingCutt_TrainingAsync, TrainingIdCoroutineFn)(this, id)
}

extern "C" fn TrainingCutt_PlayTrainingSaboriAsync(this: *mut Il2CppObject, id: i32) -> *mut Il2CppObject {
    CUTT_PLAY_TRAINING_SABORI.count();
    open_cut_run();

    get_orig_fn!(TrainingCutt_PlayTrainingSaboriAsync, TrainingIdCoroutineFn)(this, id)
}

type PlayTrainingCutEndFn = extern "C" fn(this: *mut Il2CppObject, id: i32, first: bool, second: bool) -> *mut Il2CppObject;
extern "C" fn TrainingCutt_PlayTrainingCutEndAsync(this: *mut Il2CppObject, id: i32, first: bool, second: bool) -> *mut Il2CppObject {
    CUTT_PLAY_TRAINING_CUT_END.count();

    get_orig_fn!(TrainingCutt_PlayTrainingCutEndAsync, PlayTrainingCutEndFn)(this, id, first, second)
}

type IsValidTagFn = extern "C" fn(this: *mut Il2CppObject, result: i32, cards: *mut Il2CppObject) -> bool;
// Dumped: `IsValidTag/2 -> bool(struct<TrainingResultType:4B>, generic<List<SupportCardData>>)`. This is
// the game deciding whether the cards a training produced carry a friendship, which is the only place a
// probe can tell a friendship cut-in from a regular one (A28). The answer is remembered for the cut that
// is opened next, and both arguments are handed back untouched.
extern "C" fn TrainingCutt_IsValidTag(this: *mut Il2CppObject, result: i32, cards: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(TrainingCutt_IsValidTag, IsValidTagFn)(this, result, cards);

    LAST_TAG_ANSWER.store(if value { TAG_FRIENDSHIP } else { TAG_REGULAR }, atomic::Ordering::Relaxed);
    TAG_IS_VALID_TAG.count();

    value
}

type TagCutInPlayerPlayFn = extern "C" fn(this: *mut Il2CppObject, cards: *mut Il2CppObject, done: *mut Il2CppObject);
// Dumped: `PlayCutIn/2 -> void(generic<List<SupportCardData>>, class<System.Action>)`. Both parameters are
// references the wrapper holds as addresses and passes straight back. The call is also what settles the
// kind of the cut that is open, because this is the door a friendship cut-in is played through.
extern "C" fn TagCutInPlayer_PlayCutIn(this: *mut Il2CppObject, cards: *mut Il2CppObject, done: *mut Il2CppObject) {
    TAG_PLAYER_PLAY_CUT_IN.count();
    RUN_TAG_PLAYER_SEEN.store(1, atomic::Ordering::Relaxed);

    get_orig_fn!(TagCutInPlayer_PlayCutIn, TagCutInPlayerPlayFn)(this, cards, done);
}

type TagCutInPlayerPlayOutFn = extern "C" fn(this: *mut Il2CppObject, done: *mut Il2CppObject);
extern "C" fn TagCutInPlayer_PlayCutInOut(this: *mut Il2CppObject, done: *mut Il2CppObject) {
    TAG_PLAYER_PLAY_CUT_OUT.count();

    get_orig_fn!(TagCutInPlayer_PlayCutInOut, TagCutInPlayerPlayOutFn)(this, done);
}

type StaticIsValidTagFn = extern "C" fn(cards: *mut Il2CppObject) -> bool;
// Dumped: `IsValidTag/1 -> static bool(generic<List<SupportCardData>>)`. A static target has no hidden
// `this`, so this wrapper declares only the dumped argument (A3).
extern "C" fn TagCutInPlayer_IsValidTag(cards: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(TagCutInPlayer_IsValidTag, StaticIsValidTagFn)(cards);

    LAST_TAG_ANSWER.store(if value { TAG_FRIENDSHIP } else { TAG_REGULAR }, atomic::Ordering::Relaxed);
    TAG_PLAYER_IS_VALID_TAG.count();

    value
}

type GetTotalTimeFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
// The cut-in engine's own answer to how long the animation is, read from inside its detours where `this`
// is a live timeline. A length measured this way does not have to be inferred from wall clock (A29).
extern "C" fn CuttTimeline_GetTotalTime(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CuttTimeline_GetTotalTime, GetTotalTimeFn)(this);
    CUTT_GET_TOTAL_TIME.sample(value);

    value
}

type TimelineGetSpeedFn = extern "C" fn(this: *mut Il2CppObject) -> f32;
extern "C" fn CuttTimeline_GetSpeed(this: *mut Il2CppObject) -> f32 {
    let value = get_orig_fn!(CuttTimeline_GetSpeed, TimelineGetSpeedFn)(this);
    CUTT_GET_SPEED.sample(value);

    value
}

type TimelineIntFn = extern "C" fn(this: *mut Il2CppObject) -> i32;
extern "C" fn CuttTimeline_GetTotalFrameCeil(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(CuttTimeline_GetTotalFrameCeil, TimelineIntFn)(this);

    if value > 0 {
        TIMELINE_TOTAL_FRAMES_PEAK.fetch_max(value as u32, atomic::Ordering::Relaxed);
    }

    value
}

extern "C" fn CuttTimeline_GetCurrentFrame(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(CuttTimeline_GetCurrentFrame, TimelineIntFn)(this);

    if value > 0 {
        TIMELINE_CURRENT_FRAME_PEAK.fetch_max(value as u32, atomic::Ordering::Relaxed);
    }

    value
}

extern "C" fn CuttTimeline_GetTargetFps(this: *mut Il2CppObject) -> i32 {
    let value = get_orig_fn!(CuttTimeline_GetTargetFps, TimelineIntFn)(this);

    if value > 0 {
        TIMELINE_TARGET_FPS.fetch_max(value as u32, atomic::Ordering::Relaxed) as i32;
    }

    value
}

type SetSkipFrameFn = extern "C" fn(this: *mut Il2CppObject, frames: i32);
// `set_SkipFrame/1 -> void(int)`: whether the game itself uses the door an auto skip option would have to
// use. Observed only, and the frame count is written back untouched (A30).
extern "C" fn CuttTimeline_SetSkipFrame(this: *mut Il2CppObject, frames: i32) {
    CUTT_SET_SKIP_FRAME.observe(&[frames as f64]);

    get_orig_fn!(CuttTimeline_SetSkipFrame, SetSkipFrameFn)(this, frames);
}

type TimelineSetFlagFn = extern "C" fn(this: *mut Il2CppObject, playing: bool);
extern "C" fn CuttTimeline_SetIsAutoPlay(this: *mut Il2CppObject, playing: bool) {
    CUTT_SET_IS_AUTO_PLAY.observe(&[bit(playing)]);

    get_orig_fn!(CuttTimeline_SetIsAutoPlay, TimelineSetFlagFn)(this, playing);
}

extern "C" fn CuttTimeline_GetIsAutoPlay(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(CuttTimeline_GetIsAutoPlay, CuttBoolFn)(this);
    CUTT_GET_IS_AUTO_PLAY.sample(flag_bit(value));

    value
}

type StatusPlayOutFn = extern "C" fn(this: *mut Il2CppObject, flag: bool, action: *mut Il2CppObject);
extern "C" fn TrainingCutStatus_PlayOut(this: *mut Il2CppObject, flag: bool, action: *mut Il2CppObject) {
    STATUS_PLAY_OUT.observe(&[bit(flag)]);

    get_orig_fn!(TrainingCutStatus_PlayOut, StatusPlayOutFn)(this, flag, action);
}

type StatusIntervalFn = extern "C" fn(this: *mut Il2CppObject, time: f32) -> f32;
// `GetIntervalOutBegine/1 -> float(float)`, with no setter sibling in the dump. Sampled as a peak because
// it is the gap the status panel waits before it plays out.
extern "C" fn TrainingCutStatus_GetIntervalOutBegine(this: *mut Il2CppObject, time: f32) -> f32 {
    let value = get_orig_fn!(TrainingCutStatus_GetIntervalOutBegine, StatusIntervalFn)(this, time);
    STATUS_INTERVAL_OUT.observe(&[time as f64, value as f64]);
    STATUS_INTERVAL_OUT.sample(value);

    value
}

type StatusBoolFn = extern "C" fn(this: *mut Il2CppObject) -> bool;
// `WillRankUpInHighSpeedMode/0 -> bool()`: the game's own decision that the status panel may rank up in
// high speed mode. Sampled as a peak of 1.0 so the totals line answers whether it ever said yes.
extern "C" fn TrainingCutStatus_WillRankUpInHighSpeedMode(this: *mut Il2CppObject) -> bool {
    let value = get_orig_fn!(TrainingCutStatus_WillRankUpInHighSpeedMode, StatusBoolFn)(this);
    STATUS_RANK_UP_HIGH_SPEED.sample(flag_bit(value));

    value
}

extern "C" fn TrainingCutStatus_ExistPlayingFrame(this: *mut Il2CppObject) -> bool {
    STATUS_EXIST_PLAYING_FRAME.count();

    get_orig_fn!(TrainingCutStatus_ExistPlayingFrame, StatusBoolFn)(this)
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
    generic_probe!(plate_ui, TrainingParamChangeUI_InitializePlateList, "InitializePlateList", LIST_AND_FLOAT, VOID, "TrainingParamChangeUI::InitializePlateList");
    probe!(plate_ui, PlateUI_GetIsAutoPlay, "get_IsAutoPlay", NO_PARAMS, BOOL, "TrainingParamChangeUI::get_IsAutoPlay");

    // The gauge animation is the largest piece of the training screen that run 11 measured no part of.
    probe!(hp_gauge, HpGauge_PlayIn, "PlayIn", NO_PARAMS, VOID, "SingleModeMainViewHpGauge::PlayIn");
    probe!(hp_gauge, HpGauge_PlayValue, "PlayValue", ONE_INT, VOID, "SingleModeMainViewHpGauge::PlayValue");
    probe!(hp_gauge, HpGauge_PlayOut, "PlayOut", NO_PARAMS, VOID, "SingleModeMainViewHpGauge::PlayOut");
    probe!(cut_status, TrainingCutStatus_PlayPreIn, "PlayPreIn", NO_PARAMS, VOID, "SingleModeMainViewTrainingCutStatus::PlayPreIn");
    probe!(cut_status, TrainingCutStatus_PlayEnd, "PlayEnd", NO_PARAMS, VOID, "SingleModeMainViewTrainingCutStatus::PlayEnd");
    probe!(main_view, SingleModeMain_CoroutineDoTweenTimeScale, "CoroutineDoTweenTimeScale", NO_PARAMS, CLASS, "SingleModeMainViewController::CoroutineDoTweenTimeScale");
    probe!(main_view, SingleModeMain_WaitTap, "WaitTap", NO_PARAMS, CLASS, "SingleModeMainViewController::WaitTap");

    info!("Cutt probe: {installed} doors installed, {} of them carry a counted kind in the totals line, cut runs measured from the cut start doors to CleanUpCutt and attributed to the view they start on", PROBES.len());

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

    info!("Cutt probe totals at {now_sec} s:{line} cut runs {runs} wall {wall_ms} ms timeline {peak_ms} ms open {open_for} ms");
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
    info!("Cutt probe cut kinds:{kinds} timeline self report total frames peak {total_frames} last frame peak {last_frame} target fps {target_fps}");
    info!("Cutt probe training scaling points reached:{doors}");
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
}
