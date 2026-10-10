use std::ffi::CStr;
use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;

use crate::{
    core::Hachimi,
    il2cpp::{hook::umamusume::AnimationSpeed, symbols::get_class, types::*},
};

// Observe only doors on the two yield instructions a coroutine parks on.
//
// A training turn is not only tweens. Run 17 measured a 7.4 s mean hole between the plate cascade ending
// and the next turn, and C58 proved the fork's two speed layers act on the tween clock and on the
// duration the game hands out, never on a Unity yield: `ui_animation_scale` multiplies DOTween's elapsed
// time, `time_scale` multiplies Unity's, and a `WaitUntil` has no duration for either to multiply. So the
// open question is how much of a training turn is spent inside a coroutine wait at all.
//
// Every `WaitForSeconds` the game constructs is a wait that respects `Time.timeScale`, and every
// `WaitForSecondsRealTime` is one that does not. Counting both, and how long each one asked for, says
// whether the hole this fork has been chasing is inside a yield the fork can reach, or outside both clocks.
//
// The wrappers hand the constructor its argument back untouched and write nothing, so a measurement of a
// duration cannot shorten or lengthen it. Installed only when debug_mode is on, like the other probes.

pub(crate) const WAIT_BUCKETS: [&str; 6] = ["under 0.05 s", "0.05 to 0.25 s", "0.25 to 1 s", "1 to 3 s", "3 to 10 s", "10 s and over"];

// The upper edge of every bucket but the last. A wait lands in the first bucket it is below, so a 0.05
// wait is in "0.05 to 0.25 s" and not in "under 0.05 s", which keeps one wait counted once.
const BUCKET_EDGES: [f32; 5] = [0.05, 0.25, 1.0, 3.0, 10.0];

// Which bucket a wait belongs in. A duration that is not a number, or is negative, has no bucket: it is a
// constructor this probe is not going to turn into a length it invented.
pub(crate) fn wait_bucket(seconds: f32) -> Option<usize> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }

    Some(BUCKET_EDGES.iter().position(|&edge| seconds < edge).unwrap_or(BUCKET_EDGES.len()))
}

// Whole milliseconds, so the totals a run reports stay integers the way the cut clocks do.
const MS_PER_SECOND: f32 = 1000.0;

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

fn report_due(now_sec: i64, last_sec: i64, totals: usize, last_totals: usize, interval: i64) -> bool {
    if totals == 0 || totals == last_totals {
        return false;
    }

    last_sec < 0 || now_sec - last_sec >= interval
}

const REPORT_INTERVAL_SECS: i64 = 20;
const DETAIL_LIMIT: usize = 8;
const CHUNK: usize = 512;

static START: OnceLock<Instant> = OnceLock::new();
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_TOTALS: AtomicUsize = AtomicUsize::new(0);

struct WaitRecorder {
    name: &'static str,
    calls: AtomicUsize,
    total_ms: AtomicI64,
    peak_ms: AtomicI64,
    buckets: [AtomicUsize; WAIT_BUCKETS.len()],
}

impl WaitRecorder {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            calls: AtomicUsize::new(0),
            total_ms: AtomicI64::new(0),
            peak_ms: AtomicI64::new(0),
            buckets: [const { AtomicUsize::new(0) }; WAIT_BUCKETS.len()],
        }
    }

    // A constructor is not a frame hot path, but it is not rare either, so it costs increments only. The
    // first waits of a run are printed, later ones are counted, and the histogram is printed by the report.
    fn note(&self, seconds: f32) {
        let calls = self.calls.fetch_add(1, atomic::Ordering::Relaxed) + 1;
        let ms = (seconds * MS_PER_SECOND) as i64;

        // Run 25 read a 12404 ms hold in one branch of the training cut coroutine while the cut-in's own
        // clock sat frozen at 2.4 of 2.4 seconds, so the wait is not the animation. The census of a hole is
        // the one place that can say whether the wait is a chain of yields this fork can reach, and the only
        // door that knows is this one. Costs a hash entry while a hole window stands open, nothing else.
        crate::il2cpp::hook::umamusume::note_hole_census_value(self.name, seconds);

        if ms > 0 {
            self.total_ms.fetch_add(ms, atomic::Ordering::Relaxed);
            self.peak_ms.fetch_max(ms, atomic::Ordering::Relaxed);
        }

        if let Some(index) = wait_bucket(seconds) {
            self.buckets[index].fetch_add(1, atomic::Ordering::Relaxed);
        }

        if calls <= DETAIL_LIMIT {
            debug!("Wait probe {} call {}: {seconds:.4} s", self.name, calls);
        }
        else if calls % CHUNK == 0 {
            debug!("Wait probe {} {} calls, longest {} ms", self.name, calls, self.peak_ms.load(atomic::Ordering::Relaxed));
        }
    }

    fn totals(&self) -> usize {
        self.calls.load(atomic::Ordering::Relaxed)
    }

    fn report(&self, now_sec: i64) {
        let calls = self.totals();

        if calls == 0 {
            return;
        }

        let total_ms = self.total_ms.load(atomic::Ordering::Relaxed);
        let peak_ms = self.peak_ms.load(atomic::Ordering::Relaxed);
        let mean_ms = total_ms as f64 / calls as f64;
        let name = self.name;
        let mut buckets = String::new();

        for (index, label) in WAIT_BUCKETS.iter().enumerate() {
            let count = self.buckets[index].load(atomic::Ordering::Relaxed);

            if count == 0 {
                continue;
            }

            let _ = write!(buckets, " {label} {count}");
        }

        info!(
            "Wait probe {name} at {now_sec} s: {calls} waits armed {total_ms} ms total mean {mean_ms:.1} ms longest {peak_ms} ms,{buckets}"
        );
    }
}

static WAIT_SECONDS: WaitRecorder = WaitRecorder::new("UnityEngine.WaitForSeconds");
static WAIT_REALTIME: WaitRecorder = WaitRecorder::new("UnityEngine.WaitForSecondsRealTime");

type WaitCtorFn = extern "C" fn(this: *mut Il2CppObject, seconds: f32);

// `WaitForSeconds::.ctor/1 -> void(float)`. The float the constructor is handed is the wait the coroutine
// asked for, so it is read out of the argument rather than out of `m_Duration` after the fact.
def_detour! {
    WaitForSeconds_ctor(this: *mut Il2CppObject, seconds: f32) {
            WAIT_SECONDS.note(seconds);

        get_orig_fn!(WaitForSeconds_ctor, WaitCtorFn)(this, seconds);
    }
    bail {
                get_orig_fn!(WaitForSeconds_ctor, WaitCtorFn)(this, seconds)
    }
}

// `WaitForSecondsRealTime::.ctor/1 -> void(float)`: the same wait measured against unscaled time, which is
// the one `time_scale` cannot reach.
def_detour! {
    WaitForSecondsRealTime_ctor(this: *mut Il2CppObject, seconds: f32) {
            WAIT_REALTIME.note(seconds);

        get_orig_fn!(WaitForSecondsRealTime_ctor, WaitCtorFn)(this, seconds);
    }
    bail {
                get_orig_fn!(WaitForSecondsRealTime_ctor, WaitCtorFn)(this, seconds)
    }
}

const ONE_FLOAT: &[Il2CppTypeEnum] = &[Il2CppTypeEnum_IL2CPP_TYPE_R4];
const VOID: Il2CppTypeEnum = Il2CppTypeEnum_IL2CPP_TYPE_VOID;

// Resolves the constructor of one yield class through the same signature matcher the scaling hooks use.
// `None` and `Some(0)` are different answers and the run reports them apart. The first says the class is not
// in this build, which is what Unity does with an engine class a build cannot reach. The second says the
// class is there and its constructor is not the one this wrapper declares.
fn resolve_ctor(unity: *const Il2CppImage, namespace: &CStr, class: &CStr) -> Option<usize> {
    let class = match get_class(unity, namespace, class) {
        Ok(class) => class,
        Err(_) => return None,
    };

    Some(unsafe { AnimationSpeed::resolve_method(class, ".ctor", ONE_FLOAT, VOID) })
}

// The word one door deserves, kept out of `init` so the wording a run reads is a testable thing.
pub(crate) fn door_state(found: Option<usize>) -> &'static str {
    match found {
        None => "is not a class in this build",
        Some(0) => "is there with no constructor this wrapper can stand on",
        Some(_) => "armed",
    }
}

pub fn init(unity: *const Il2CppImage) {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());

    // The two doors are armed here rather than through one helper, because `new_hook!` keys
    // `disabled_hooks` on the wrapper's id and one helper standing on both doors is one id for both
    // doors (C27): the key would put the scaled wait and the realtime wait down together.
    let scaled = resolve_ctor(unity, c"UnityEngine", c"WaitForSeconds");
    let realtime = resolve_ctor(unity, c"UnityEngine", c"WaitForSecondsRealTime");

    if scaled.unwrap_or(0) != 0 {
        let addr = scaled.unwrap_or(0);
        new_hook!(addr, WaitForSeconds_ctor);
    }

    if realtime.unwrap_or(0) != 0 {
        let addr = realtime.unwrap_or(0);
        new_hook!(addr, WaitForSecondsRealTime_ctor);
    }

    info!(
        "Wait probe: UnityEngine.WaitForSeconds {}, UnityEngine.WaitForSecondsRealTime {}",
        door_state(scaled),
        door_state(realtime)
    );
}

// Called from the GameSystem update tick beside the other probe reports. A run that armed no wait prints
// nothing.
pub fn report_if_due() {
    if START.get().is_none() {
        return;
    }

    let totals = WAIT_SECONDS.totals() + WAIT_REALTIME.totals();
    let now_sec = elapsed_ms() / 1000;

    if !report_due(now_sec, LAST_REPORT_SEC.load(atomic::Ordering::Relaxed), totals, LAST_TOTALS.load(atomic::Ordering::Relaxed), REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_TOTALS.store(totals, atomic::Ordering::Relaxed);

    WAIT_SECONDS.report(now_sec);
    WAIT_REALTIME.report(now_sec);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_lands_in_exactly_one_bucket() {
        assert_eq!(wait_bucket(0.0), Some(0));
        assert_eq!(wait_bucket(0.049_999), Some(0));
        assert_eq!(wait_bucket(0.05), Some(1));
        assert_eq!(wait_bucket(0.16), Some(1));
        assert_eq!(wait_bucket(1.0), Some(3));
        assert_eq!(wait_bucket(7.5), Some(4));
        assert_eq!(wait_bucket(10.0), Some(5));
        assert_eq!(wait_bucket(9_999.0), Some(5));
    }

    #[test]
    fn a_duration_that_is_not_a_number_gets_no_bucket() {
        assert_eq!(wait_bucket(-0.5), None);
        assert_eq!(wait_bucket(f32::NAN), None);
        assert_eq!(wait_bucket(f32::INFINITY), None);
    }

    #[test]
    fn the_histogram_covers_every_bucket_name() {
        assert_eq!(WAIT_BUCKETS.len(), BUCKET_EDGES.len() + 1);

        for index in 0..WAIT_BUCKETS.len() {
            assert!(!WAIT_BUCKETS[index].is_empty());
        }
    }

    #[test]
    fn the_report_waits_for_a_wait_and_for_the_interval() {
        assert!(!report_due(30, -1, 0, 0, REPORT_INTERVAL_SECS));
        assert!(report_due(3, -1, 4, 0, REPORT_INTERVAL_SECS));
        assert!(!report_due(15, 3, 9, 4, REPORT_INTERVAL_SECS));
        assert!(report_due(24, 3, 9, 4, REPORT_INTERVAL_SECS));
    }

    #[test]
    fn a_wait_is_censused_under_the_name_the_report_prints() {
        // The hole census now carries these doors, and a reader has to be able to match a census entry to
        // the histogram line. Different labels would make a wait inside a hole look like some other door.
        assert_eq!(WAIT_SECONDS.name, "UnityEngine.WaitForSeconds");
        assert_eq!(WAIT_REALTIME.name, "UnityEngine.WaitForSecondsRealTime");
    }

    #[test]
    fn a_class_that_is_absent_and_a_constructor_that_refused_say_different_things() {
        assert_eq!(door_state(Some(0x1000)), "armed");
        assert_eq!(door_state(Some(0)), "is there with no constructor this wrapper can stand on");
        assert_eq!(door_state(None), "is not a class in this build");
    }
}
