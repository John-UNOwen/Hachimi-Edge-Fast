use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicBool, AtomicI32, AtomicI64};
use std::sync::OnceLock;
use std::time::Instant;

use super::{CutStateProbe, DialogProbe, InheritFlowProbe, SkillSparkProbe, StoryEventProbe, TrainingCuttProbe};
use super::TrainingCuttProbe::CutProbe;

// The probe suite's shared parts: one clock, the list of every stamped door, and the watchdog that reads both.
//
// Every counted door in the probe modules stamps the time it was last crossed on this clock (`CutProbe::stamp`).
// When a career view stands still for WATCHDOG_SECS, the watchdog writes one line that says which doors were
// crossed most recently and which installed doors were never crossed at all. A stall is then read from the run
// that stalled, instead of from another run with one more door added.
//
// Observe only. Nothing in this file writes to the game, and the snapshot is taken on the quiet tick path
// (`GameFrameProbe::report_if_due`), never inside a frame.

static SUITE_START: OnceLock<Instant> = OnceLock::new();

// A view that stands this long gets one snapshot per stay. A training turn can spend a minute on the player's
// own decision alone, so a snapshot reads a long stay and does not claim the stay was a stall.
pub(crate) const WATCHDOG_SECS: i64 = 60;
const WATCHDOG_US: i64 = WATCHDOG_SECS * 1_000_000;
// The bound on the two lists the line prints. The line says how many it left out.
const WATCHDOG_RECENT: usize = 12;
const WATCHDOG_SILENT_NAMES: usize = 40;

// Starts the clock. Called from the frame clock's init, which runs before any door is armed.
pub(crate) fn start() {
    let _ = SUITE_START.set(Instant::now());
}

// Microseconds since `start`, or -1 before it. Microseconds because a 200 fps frame is 5 ms, and a millisecond
// clock reads that frame as 4, 5 or 6 ms.
pub(crate) fn now_us() -> i64 {
    match SUITE_START.get() {
        Some(start) => start.elapsed().as_micros() as i64,
        None => -1,
    }
}

// Every door the watchdog can see. `StoryFrameProbe` keeps its own door type, which does not stamp, so its doors
// are outside the census and the watchdog cannot name them as silent.
pub(crate) fn all_doors() -> Vec<&'static CutProbe> {
    let mut doors: Vec<&'static CutProbe> = Vec::new();

    doors.extend(TrainingCuttProbe::PROBES.iter().copied());
    doors.extend(StoryEventProbe::PROBES.iter().copied());
    doors.extend(DialogProbe::PROBES.iter().copied());
    doors.extend(CutStateProbe::PROBES.iter().copied());
    doors.extend(SkillSparkProbe::PROBES.iter().copied());
    doors.extend(InheritFlowProbe::PROBES.iter().copied());

    doors
}

pub(crate) fn watchdog_due(since_us: i64, now_us: i64) -> bool {
    since_us >= 0 && now_us - since_us >= WATCHDOG_US
}

// The view the game is standing on and when it was first asked for. `observe` runs once per game tick and does a
// few atomic operations. `take_due` runs on the quiet path and answers each stay that crossed the threshold once.
pub(crate) struct Watch {
    view: AtomicI32,
    since_us: AtomicI64,
    reported: AtomicBool,
    due: AtomicBool,
}

impl Watch {
    pub(crate) const fn new() -> Self {
        Self {
            view: AtomicI32::new(i32::MIN),
            since_us: AtomicI64::new(-1),
            reported: AtomicBool::new(false),
            due: AtomicBool::new(false),
        }
    }

    pub(crate) fn observe(&self, view: i32, now: i64) {
        if now < 0 {
            return;
        }

        if self.view.load(atomic::Ordering::Relaxed) != view {
            self.view.store(view, atomic::Ordering::Relaxed);
            self.since_us.store(now, atomic::Ordering::Relaxed);
            self.reported.store(false, atomic::Ordering::Relaxed);
            return;
        }

        let since = self.since_us.load(atomic::Ordering::Relaxed);

        if !self.reported.load(atomic::Ordering::Relaxed) && watchdog_due(since, now) {
            self.reported.store(true, atomic::Ordering::Relaxed);
            self.due.store(true, atomic::Ordering::Relaxed);
        }
    }

    // The view and how long it has stood, once for each stay that crossed the threshold.
    pub(crate) fn take_due(&self, now: i64) -> Option<(i32, i64)> {
        if !self.due.swap(false, atomic::Ordering::Relaxed) {
            return None;
        }

        Some((self.view.load(atomic::Ordering::Relaxed), now - self.since_us.load(atomic::Ordering::Relaxed)))
    }
}

pub(crate) static WATCH: Watch = Watch::new();

// The line the watchdog writes: the view, how long it stood, the doors crossed most recently, and the doors never
// crossed. On a stall, the answer a run has to give is which doors stayed silent while the view held.
pub(crate) fn watchdog_line(view: i32, stood_us: i64, doors: &[&CutProbe], now: i64) -> String {
    let mut crossed: Vec<(&str, usize, i64)> = Vec::new();
    let mut silent: Vec<&str> = Vec::new();

    for door in doors {
        let last = door.last_us();

        if door.calls() == 0 || last < 0 {
            silent.push(door.label());
        }
        else {
            crossed.push((door.label(), door.calls(), now - last));
        }
    }

    crossed.sort_by_key(|&(_, _, age)| age);

    let mut line = format!(
        "Stall watchdog: view {view} stood {} s, {} of {} doors crossed this run",
        stood_us / 1_000_000,
        crossed.len(),
        doors.len()
    );

    line.push_str(". Crossed most recently:");

    for (label, calls, age) in crossed.iter().take(WATCHDOG_RECENT) {
        let _ = write!(line, " {label} {calls} calls, last {} ms ago;", age / 1_000);
    }

    let _ = write!(line, " never crossed {}:", silent.len());

    for name in silent.iter().take(WATCHDOG_SILENT_NAMES) {
        let _ = write!(line, " {name};");
    }

    if silent.len() > WATCHDOG_SILENT_NAMES {
        let _ = write!(line, " and {} more", silent.len() - WATCHDOG_SILENT_NAMES);
    }

    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_watchdog_waits_for_the_threshold_and_answers_each_stay_once() {
        let watch = Watch::new();

        watch.observe(1501, 0);
        watch.observe(1501, WATCHDOG_US - 1);
        assert_eq!(watch.take_due(WATCHDOG_US - 1), None, "a stay short of the threshold is not a snapshot");

        watch.observe(1501, WATCHDOG_US);
        assert_eq!(watch.take_due(WATCHDOG_US), Some((1501, WATCHDOG_US)));

        watch.observe(1501, WATCHDOG_US + 5_000_000);
        assert_eq!(watch.take_due(WATCHDOG_US + 5_000_000), None, "the same stay is answered once");

        let next = WATCHDOG_US + 7_000_000;
        watch.observe(1500, next);
        watch.observe(1500, next + WATCHDOG_US);
        assert_eq!(watch.take_due(next + WATCHDOG_US), Some((1500, WATCHDOG_US)), "a new view starts its own clock");
    }

    #[test]
    fn a_clock_that_has_not_started_never_arms_the_watchdog() {
        let watch = Watch::new();

        watch.observe(1501, -1);
        watch.observe(1501, -1);
        assert_eq!(watch.take_due(WATCHDOG_US * 2), None);
        assert!(!watchdog_due(-1, WATCHDOG_US * 2));
    }

    #[test]
    fn the_watchdog_line_names_the_recent_doors_and_the_silent_ones() {
        let recent = CutProbe::counted("test recent door");
        let quiet = CutProbe::counted("test quiet door");

        recent.observe(&[]);
        recent.stamp_at(9_000_000);

        let doors: [&CutProbe; 2] = [&recent, &quiet];
        let line = watchdog_line(1501, 61_000_000, &doors, 10_000_000);

        assert!(line.starts_with("Stall watchdog: view 1501 stood 61 s, 1 of 2 doors crossed this run"), "{line}");
        assert!(line.contains("test recent door 1 calls, last 1000 ms ago;"), "{line}");
        assert!(line.contains("never crossed 1: test quiet door;"), "{line}");
    }

    #[test]
    fn every_door_in_the_suite_has_its_own_label() {
        // The watchdog keys its lists by label, so two doors under one name would hide one another.
        let doors = all_doors();
        let mut labels: Vec<&str> = doors.iter().map(|door| door.label()).collect();
        let total = labels.len();

        labels.sort_unstable();
        labels.dedup();

        assert_eq!(labels.len(), total, "two doors share a label across the suite");
        assert!(total > 150, "the suite lists the doors of every stamped module, found {total}");
    }
}
