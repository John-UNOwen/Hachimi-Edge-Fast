use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicBool, AtomicI64, AtomicUsize};

use crate::{
    core::Hachimi,
    il2cpp::{
        hook::umamusume::{
            AnimationSpeed,
            ProbeSuite,
            SceneManager,
            TrainingCuttProbe::{bit, class_for_label, peak_seconds, report_due, CutProbe, PROBE_DETAIL_LIMIT, REPORT_INTERVAL_SECS},
        },
        types::*,
    },
};

// Observe only doors on the skill spark path: the effect player that paces each spark, the skill learning
// coroutine that plays the gained skills, and the sound each spark plays.
//
// The question this answers is why career skill sparks show one at a time when a faster build passes them in a
// flash. The game paces its own sparks, and none of that pace is a number this fork has read. The constants that
// set it, `SkillEffectPlayer.EFFECT_PLAY_INTERVAL_SEC` and `DELAY_ACTIVATE_SEC`, are `static const` on this client,
// so IL2CPP folds them into their call sites and no field read can see them (AnimationSpeed.rs, the C13 note). The
// readable form of the pace is the gap between two sparks, and this module measures that gap from the sound each
// spark plays. The smallest of those gaps is the shortest time a spark takes on this client.
//
// Names come from the live dump: `Gallop.SkillEffectPlayer` (`UpdateElapsedTime/1`, `UpdateDelayPlay/1` and
// `SetSpeed/1` take a `float`, `ClearDelayPlay/0` takes nothing, all instance methods),
// `Gallop.SingleModeSkillLearningViewController::CoroutineSkillGetChangeView/1` (takes the gained list and returns
// an `IEnumerator`), and `Gallop.SingleModeUtils` (`static PlaySkillGetSe/2` takes `(int, bool)`, and
// `static PlayUniqueSkillGetSe/0`).
//
// Nothing here changes an argument or a return value. Every wrapper hands the call to the game untouched, and the
// whole module is behind `debug_mode`, so a player who has that off gets no hook at all.

const R4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_R4;
const I4: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_I4;
const BOOL: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_BOOLEAN;
const CLASS: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_CLASS;
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;
const NO_PARAMS: &[Il2CppTypeEnum] = &[];
const ONE_FLOAT: &[Il2CppTypeEnum] = &[R4];
const GENERIC: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_GENERICINST;
const ONE_GENERIC: &[Il2CppTypeEnum] = &[GENERIC];
const INT_AND_FLAG: &[Il2CppTypeEnum] = &[I4, BOOL];

// The effect clock: the step every spark effect is handed per call, and the peak of it.
static EFFECT_ELAPSED: CutProbe = CutProbe::peaked("SkillEffectPlayer::UpdateElapsedTime(dt)");
static EFFECT_DELAY: CutProbe = CutProbe::peaked("SkillEffectPlayer::UpdateDelayPlay(dt)");
static EFFECT_SPEED: CutProbe = CutProbe::peaked("SkillEffectPlayer::SetSpeed(speed)");
static EFFECT_CLEAR_DELAY: CutProbe = CutProbe::counted("SkillEffectPlayer::ClearDelayPlay()");
// The coroutine that plays the gained skills. Each call starts a burst, which `start_burst` closes.
static GET_CHANGE_VIEW: CutProbe = CutProbe::counted("SingleModeSkillLearningViewController::CoroutineSkillGetChangeView(list)");
// Every skill sound, race ones included. The gap measure below keeps the career sounds apart from the race ones.
static GET_SOUND: CutProbe = CutProbe::counted("SingleModeUtils::PlaySkillGetSe(id, flag)");
static GET_UNIQUE_SOUND: CutProbe = CutProbe::counted("SingleModeUtils::PlayUniqueSkillGetSe()");

pub(crate) static PROBES: [&CutProbe; 7] = [
    &EFFECT_ELAPSED,
    &EFFECT_DELAY,
    &EFFECT_SPEED,
    &EFFECT_CLEAR_DELAY,
    &GET_CHANGE_VIEW,
    &GET_SOUND,
    &GET_UNIQUE_SOUND,
];

// The gap between two career skill sounds, in microseconds, in buckets whose upper edges are below.
const SOUND_EDGES_US: [i64; 6] = [50_000, 100_000, 200_000, 400_000, 800_000, 1_600_000];
const SOUND_BUCKETS: usize = 7;
const SOUND_BUCKET_NAMES: [&str; SOUND_BUCKETS] =
    ["under 50 ms", "50 to 100 ms", "100 to 200 ms", "200 to 400 ms", "400 to 800 ms", "800 to 1600 ms", "1.6 s and over"];

static SOUND_LAST_US: AtomicI64 = AtomicI64::new(-1);
static SOUND_GAPS: AtomicUsize = AtomicUsize::new(0);
static SOUND_GAP_MIN_US: AtomicI64 = AtomicI64::new(i64::MAX);
static SOUND_GAP_MAX_US: AtomicI64 = AtomicI64::new(0);
static SOUND_GAP_TOTAL_US: AtomicI64 = AtomicI64::new(0);
static SOUND_GAP_BUCKETS: [AtomicUsize; SOUND_BUCKETS] = [const { AtomicUsize::new(0) }; SOUND_BUCKETS];
// Race sounds are counted apart. A race plays its own skill cut-ins through the same sound, and they are not the
// career sparks this module is measuring.
static RACE_SOUNDS: AtomicUsize = AtomicUsize::new(0);
// A burst is the career sounds between two starts of the skill get coroutine.
static BURST_SOUNDS: AtomicUsize = AtomicUsize::new(0);
static BURSTS_CLOSED: AtomicUsize = AtomicUsize::new(0);
static BURST_LARGEST: AtomicUsize = AtomicUsize::new(0);

static ARMED: AtomicBool = AtomicBool::new(false);
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

// The bucket a career gap falls in: the first edge it is below, or the last bucket when it clears them all.
pub(crate) fn sound_gap_bucket(gap_us: i64) -> usize {
    SOUND_EDGES_US.iter().position(|&edge| gap_us < edge).unwrap_or(SOUND_EDGES_US.len())
}

// The gap between two career sounds, or nothing when there is no earlier sound or the clock went backwards.
pub(crate) fn sound_gap(last_us: i64, now_us: i64) -> Option<i64> {
    if last_us < 0 || now_us < last_us {
        return None;
    }

    Some(now_us - last_us)
}

fn note_sound(in_race: bool) {
    let now = ProbeSuite::now_us();

    // A probe with no clock has nothing to measure a gap with, so it counts nothing here.
    if now < 0 {
        return;
    }

    if in_race {
        RACE_SOUNDS.fetch_add(1, atomic::Ordering::Relaxed);
        return;
    }

    BURST_SOUNDS.fetch_add(1, atomic::Ordering::Relaxed);

    let last = SOUND_LAST_US.swap(now, atomic::Ordering::Relaxed);
    let Some(gap) = sound_gap(last, now) else {
        return;
    };

    let gaps = SOUND_GAPS.fetch_add(1, atomic::Ordering::Relaxed) + 1;

    SOUND_GAP_TOTAL_US.fetch_add(gap, atomic::Ordering::Relaxed);
    SOUND_GAP_MIN_US.fetch_min(gap, atomic::Ordering::Relaxed);
    SOUND_GAP_MAX_US.fetch_max(gap, atomic::Ordering::Relaxed);
    SOUND_GAP_BUCKETS[sound_gap_bucket(gap)].fetch_add(1, atomic::Ordering::Relaxed);

    // The first gaps are named one by one, so a run shows the actual spacing and not only the summary.
    if gaps <= PROBE_DETAIL_LIMIT {
        info!("Skill spark probe: career sound {gaps} came {} ms after the sound before it", gap / 1_000);
    }
}

// The coroutine start closes the burst that came before it. An empty burst is not a burst.
fn start_burst() {
    let sounds = BURST_SOUNDS.swap(0, atomic::Ordering::Relaxed);

    if sounds == 0 {
        return;
    }

    BURSTS_CLOSED.fetch_add(1, atomic::Ordering::Relaxed);
    BURST_LARGEST.fetch_max(sounds, atomic::Ordering::Relaxed);
}

type EffectTimeFn = extern "C" fn(this: *mut Il2CppObject, dt: f32);
def_detour! {
    SkillEffectPlayer_UpdateElapsedTime(this: *mut Il2CppObject, dt: f32) {
            EFFECT_ELAPSED.sample(dt);

        get_orig_fn!(SkillEffectPlayer_UpdateElapsedTime, EffectTimeFn)(this, dt);
    }
    bail {
                get_orig_fn!(SkillEffectPlayer_UpdateElapsedTime, EffectTimeFn)(this, dt)
    }
}

def_detour! {
    SkillEffectPlayer_UpdateDelayPlay(this: *mut Il2CppObject, dt: f32) {
            super::SkillPopupProbe::note_effect_queue(this);
            EFFECT_DELAY.sample(dt);

        get_orig_fn!(SkillEffectPlayer_UpdateDelayPlay, EffectTimeFn)(this, dt);
    }
    bail {
                get_orig_fn!(SkillEffectPlayer_UpdateDelayPlay, EffectTimeFn)(this, dt)
    }
}

type EffectSpeedFn = extern "C" fn(this: *mut Il2CppObject, speed: f32);
def_detour! {
    SkillEffectPlayer_SetSpeed(this: *mut Il2CppObject, speed: f32) {
            EFFECT_SPEED.observe_peak(&[speed as f64], speed);

        get_orig_fn!(SkillEffectPlayer_SetSpeed, EffectSpeedFn)(this, speed);
    }
    bail {
                get_orig_fn!(SkillEffectPlayer_SetSpeed, EffectSpeedFn)(this, speed)
    }
}

type EffectVoidFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    SkillEffectPlayer_ClearDelayPlay(this: *mut Il2CppObject) {
            EFFECT_CLEAR_DELAY.count();

        get_orig_fn!(SkillEffectPlayer_ClearDelayPlay, EffectVoidFn)(this);
    }
    bail {
                get_orig_fn!(SkillEffectPlayer_ClearDelayPlay, EffectVoidFn)(this)
    }
}

type ChangeViewCoroutineFn = extern "C" fn(this: *mut Il2CppObject, gained: *mut Il2CppObject) -> *mut Il2CppObject;
def_detour! {
    SingleModeSkillLearningViewController_CoroutineSkillGetChangeView(this: *mut Il2CppObject, gained: *mut Il2CppObject) -> *mut Il2CppObject {
            GET_CHANGE_VIEW.count();
        start_burst();

        get_orig_fn!(SingleModeSkillLearningViewController_CoroutineSkillGetChangeView, ChangeViewCoroutineFn)(this, gained)
    }
    bail {
                get_orig_fn!(SingleModeSkillLearningViewController_CoroutineSkillGetChangeView, ChangeViewCoroutineFn)(this, gained)
    }
}

type SkillGetSeFn = extern "C" fn(id: i32, flag: bool);
def_detour! {
    SingleModeUtils_PlaySkillGetSe(id: i32, flag: bool) {
            GET_SOUND.observe(&[id as f64, bit(flag)]);
        note_sound(SceneManager::is_race_scene_family());

        get_orig_fn!(SingleModeUtils_PlaySkillGetSe, SkillGetSeFn)(id, flag);
    }
    bail {
                get_orig_fn!(SingleModeUtils_PlaySkillGetSe, SkillGetSeFn)(id, flag)
    }
}

type UniqueSkillGetSeFn = extern "C" fn();
def_detour! {
    SingleModeUtils_PlayUniqueSkillGetSe() {
            GET_UNIQUE_SOUND.count();

        get_orig_fn!(SingleModeUtils_PlayUniqueSkillGetSe, UniqueSkillGetSeFn)();
    }
    bail {
                get_orig_fn!(SingleModeUtils_PlayUniqueSkillGetSe, UniqueSkillGetSeFn)()
    }
}

pub fn init(umamusume: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let effect = class_for_label(umamusume, "Gallop.SkillEffectPlayer");
    let learning = class_for_label(umamusume, "Gallop.SingleModeSkillLearningViewController");
    let utils = class_for_label(umamusume, "Gallop.SingleModeUtils");

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

    // A parameter the dump spells as generic<...> reports GENERICINST, so it resolves through the generic matcher,
    // the way the training probe installs its gained-list door (C48). The plain reference matcher refuses it.
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

    probe!(effect, SkillEffectPlayer_UpdateElapsedTime, "UpdateElapsedTime", ONE_FLOAT, VOID, "SkillEffectPlayer::UpdateElapsedTime(dt)");
    probe!(effect, SkillEffectPlayer_UpdateDelayPlay, "UpdateDelayPlay", ONE_FLOAT, VOID, "SkillEffectPlayer::UpdateDelayPlay(dt)");
    probe!(effect, SkillEffectPlayer_SetSpeed, "SetSpeed", ONE_FLOAT, VOID, "SkillEffectPlayer::SetSpeed(speed)");
    probe!(effect, SkillEffectPlayer_ClearDelayPlay, "ClearDelayPlay", NO_PARAMS, VOID, "SkillEffectPlayer::ClearDelayPlay()");
    generic_probe!(
        learning,
        SingleModeSkillLearningViewController_CoroutineSkillGetChangeView,
        "CoroutineSkillGetChangeView",
        ONE_GENERIC,
        CLASS,
        "SingleModeSkillLearningViewController::CoroutineSkillGetChangeView(list)"
    );
    static_probe!(utils, SingleModeUtils_PlaySkillGetSe, "PlaySkillGetSe", INT_AND_FLAG, VOID, "SingleModeUtils::PlaySkillGetSe(id, flag)");
    static_probe!(utils, SingleModeUtils_PlayUniqueSkillGetSe, "PlayUniqueSkillGetSe", NO_PARAMS, VOID, "SingleModeUtils::PlayUniqueSkillGetSe()");

    ARMED.store(true, atomic::Ordering::Release);
    info!("Skill spark probe: {installed} of {} doors installed, career sound gaps and bursts are measured from the first career sound", PROBES.len());

    if !missing.is_empty() {
        info!("Skill spark probe: not installed, no class or no matching overload: {}", missing.join(", "));
    }
}

// Called from the GameSystem update detour beside the other probe reports.
pub fn report_if_due() {
    if !ARMED.load(atomic::Ordering::Acquire) {
        return;
    }

    let totals: usize = PROBES.iter().map(|probe| probe.calls()).sum();
    let now_sec = ProbeSuite::now_us() / 1_000_000;
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

    let gaps = SOUND_GAPS.load(atomic::Ordering::Relaxed);
    let gap_text = match gaps {
        0 => String::from("career sound gaps 0"),
        n => format!(
            "career sound gaps {n}, min {} ms, mean {:.1} ms, worst {} ms",
            SOUND_GAP_MIN_US.load(atomic::Ordering::Relaxed) / 1_000,
            SOUND_GAP_TOTAL_US.load(atomic::Ordering::Relaxed) as f64 / n as f64 / 1_000.0,
            SOUND_GAP_MAX_US.load(atomic::Ordering::Relaxed) / 1_000
        ),
    };

    let mut buckets = String::new();

    for (index, name) in SOUND_BUCKET_NAMES.iter().enumerate() {
        let count = SOUND_GAP_BUCKETS[index].load(atomic::Ordering::Relaxed);

        if count == 0 {
            continue;
        }

        let _ = write!(buckets, " {name} {count};");
    }

    info!(
        "Skill spark probe totals at {now_sec} s:{line} | {gap_text} [{buckets} ] | bursts closed {} largest {} sounds, open burst {} sounds | race sounds {}",
        BURSTS_CLOSED.load(atomic::Ordering::Relaxed),
        BURST_LARGEST.load(atomic::Ordering::Relaxed),
        BURST_SOUNDS.load(atomic::Ordering::Relaxed),
        RACE_SOUNDS.load(atomic::Ordering::Relaxed),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_career_gap_lands_in_exactly_one_bucket() {
        assert_eq!(sound_gap_bucket(0), 0);
        assert_eq!(sound_gap_bucket(49_999), 0);
        assert_eq!(sound_gap_bucket(50_000), 1);
        assert_eq!(sound_gap_bucket(99_999), 1);
        assert_eq!(sound_gap_bucket(1_599_999), 5);
        assert_eq!(sound_gap_bucket(1_600_000), 6);
        assert_eq!(sound_gap_bucket(i64::MAX), 6);
        assert_eq!(SOUND_EDGES_US.len() + 1, SOUND_BUCKETS, "one bucket per edge and one past the last edge");
        assert_eq!(SOUND_BUCKET_NAMES.len(), SOUND_BUCKETS);
    }

    #[test]
    fn a_gap_needs_an_earlier_sound_and_a_clock_that_moves_forward() {
        assert_eq!(sound_gap(-1, 5_000), None, "the first sound has nothing to be measured against");
        assert_eq!(sound_gap(9_000, 5_000), None, "a clock that went back is not a gap");
        assert_eq!(sound_gap(5_000, 5_000), Some(0));
        assert_eq!(sound_gap(1_000_000, 1_250_000), Some(250_000));
    }

    #[test]
    fn every_skill_door_is_named_by_the_class_that_owns_it() {
        let labels: Vec<&str> = PROBES.iter().map(|probe| probe.label()).collect();

        assert_eq!(labels.len(), 7);
        assert!(labels.iter().all(|label| {
            label.starts_with("SkillEffectPlayer::")
                || label.starts_with("SingleModeSkillLearningViewController::")
                || label.starts_with("SingleModeUtils::")
        }));
    }
}
