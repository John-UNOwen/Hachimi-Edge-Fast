use std::fmt::Write as _;
use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicUsize};

use crate::{
    core::Hachimi,
    il2cpp::hook::umamusume::{
        ProbeSuite,
        SceneManager,
        TrainingCuttProbe::{bucket_for, report_due, BUCKET_COUNT, BUCKET_NAMES, PROBE_DETAIL_LIMIT, REPORT_INTERVAL_SECS},
    },
};

// A frame clock over the whole game, hung on the `GameSystem_Update` detour the fork already owns.
//
// Every probe in this fork counts doors, and a door count is not a cost. Run 10 measured 317.1 s on the
// training screen and 18.6 s on story, but nothing said how much of that was frames the game was drawing
// and how much was the player reading. This answers the drawing half without adding a single new hook: the
// gap between two consecutive `GameSystem_Update` calls is one rendered frame, and a career of those gaps
// says which screen spends frames and how large the worst frame in the session was.
//
// The clock is in microseconds. At the 200 fps target a frame is 5 ms, and a millisecond clock reads that
// frame as 4, 5 or 6 ms, which is an error as large as the frame itself. The work this probe does on a frame
// runs after the frame's timestamp is taken, so it is charged to the next gap: the cost of the probe is part of
// the frame it measures. That is why the screen is read from the atomic `ChangeView` sets and not from the
// game, which would be two managed calls on every frame.
//
// A gap over SLOW_FRAME_US is counted as a stall. The count is reported beside its total time, so a long stall
// and many short ones cannot be confused with each other.

// A frame this long is reported as a stall. 50 ms is a third of the 20 fps the training cut timeline is
// authored at (run 10 read `get_TargetFps` as 20), so a frame past it is a frame the animation already lost.
const SLOW_FRAME_US: i64 = 50_000;
// The first frame after a load is not a frame the game drew; it is the gap the loader left behind. The very
// first gap is dropped for that reason, and the report says how many gaps it ever dropped.
const DROP_FIRST_GAP: usize = 1;

static ENABLED: AtomicUsize = AtomicUsize::new(0);
static LAST_US: AtomicI64 = AtomicI64::new(-1);
static GAPS: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);
static TOTAL_US: AtomicI64 = AtomicI64::new(0);
static WORST_US: AtomicI64 = AtomicI64::new(0);
static SLOW_GAPS: AtomicUsize = AtomicUsize::new(0);
static SLOW_US: AtomicI64 = AtomicI64::new(0);
static BUCKET_GAPS: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static BUCKET_US: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static BUCKET_SLOW: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static BUCKET_WORST_US: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static WORST_AT_US: AtomicI64 = AtomicI64::new(-1);
static STALLS_SHOWN: AtomicUsize = AtomicUsize::new(0);
// Run 11 labelled its `other` bucket with the first view that landed in it and printed `view 0`, which
// hid the 1,621 frames the cut-in probe had measured on a screen this probe had never named. A bucket
// now holds a bounded list of the views it saw and is labelled by the one that filled it most.
// Eight is a bound on the work done per frame, not a claim about the game: extra views are counted as
// `more` in the report instead of being dropped.
const VIEWS_TRACKED: usize = 8;
// A view id can legitimately be 0, so an empty slot holds a value no `ViewId` has.
const VIEW_SLOT_EMPTY: i32 = i32::MIN;
static BUCKET_VIEW_ID: [AtomicI32; BUCKET_COUNT * VIEWS_TRACKED] = [const { AtomicI32::new(VIEW_SLOT_EMPTY) }; BUCKET_COUNT * VIEWS_TRACKED];
static BUCKET_VIEW_FRAMES: [AtomicUsize; BUCKET_COUNT * VIEWS_TRACKED] = [const { AtomicUsize::new(0) }; BUCKET_COUNT * VIEWS_TRACKED];
static BUCKET_VIEWS_SEEN: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static LAST_REPORT_SEC: AtomicI64 = AtomicI64::new(-1);
static LAST_GAPS: AtomicUsize = AtomicUsize::new(0);

// The gap between two game ticks, or nothing when there is no earlier tick or the clock went backwards. The first
// gap of a run has no start, so it is dropped and counted instead of measured from the loader's last moment.
pub(crate) fn frame_gap_us(last: i64, now: i64) -> Option<i64> {
    if last < 0 || now - last < 0 {
        return None;
    }

    Some(now - last)
}

// One decision, spelled out so a change to it is a change to a test and not a silent edit to a hot path.
fn gap_is_slow(gap_us: i64) -> bool {
    gap_us > SLOW_FRAME_US
}

// Called from `GameSystem_Update` before anything else the detour does, so the gap it measures is the gap
// the game actually took between two of its own ticks.
pub fn observe_frame() {
    if ENABLED.load(atomic::Ordering::Relaxed) == 0 {
        return;
    }

    let now = ProbeSuite::now_us();

    if now < 0 {
        return;
    }

    let last = LAST_US.swap(now, atomic::Ordering::Relaxed);
    let Some(gap) = frame_gap_us(last, now) else {
        DROPPED.fetch_add(DROP_FIRST_GAP, atomic::Ordering::Relaxed);
        return;
    };

    // The view the game last asked for. `ChangeView` sets it, so no game call is made here.
    let view = SceneManager::requested_view_id();
    let bucket = bucket_for(view, SceneManager::is_race_scene_family()) as usize;

    GAPS.fetch_add(1, atomic::Ordering::Relaxed);
    TOTAL_US.fetch_add(gap, atomic::Ordering::Relaxed);
    BUCKET_GAPS[bucket].fetch_add(1, atomic::Ordering::Relaxed);
    BUCKET_US[bucket].fetch_add(gap, atomic::Ordering::Relaxed);

    record_worst(bucket, gap, now);

    if gap_is_slow(gap) {
        SLOW_GAPS.fetch_add(1, atomic::Ordering::Relaxed);
        SLOW_US.fetch_add(gap, atomic::Ordering::Relaxed);
        BUCKET_SLOW[bucket].fetch_add(1, atomic::Ordering::Relaxed);

        // A stall is only useful if a run can say where it happened, so the first ones are named with
        // their place in the session and the view they landed on. The cap is the same one every other
        // probe in this fork uses: a frame path must not format, so the reporting stops early and the
        // totals line keeps counting.
        let shown = STALLS_SHOWN.fetch_add(1, atomic::Ordering::Relaxed);

        if shown < PROBE_DETAIL_LIMIT {
            info!("Frame clock stall {}: {} ms at {} s on view {view} {}", shown + 1, gap / 1_000, now / 1_000_000, BUCKET_NAMES[bucket]);
        }
    }

    record_view(bucket, view);
    ProbeSuite::WATCH.observe(view, now);
}

// The session's worst frame and a screen's worst frame are two separate records. They were one write
// in the first version, which is how run 12 came to print `training screen ... slow 40 worst 0 ms`: a
// bucket only got a number on the frame that beat the whole session, so a screen that held 40 frames
// over 50 ms could still claim it had none.
fn record_worst(bucket: usize, gap_us: i64, now: i64) {
    if gap_us > WORST_US.load(atomic::Ordering::Relaxed) {
        WORST_US.store(gap_us, atomic::Ordering::Relaxed);
        WORST_AT_US.store(now, atomic::Ordering::Relaxed);
    }

    if gap_us > BUCKET_WORST_US[bucket].load(atomic::Ordering::Relaxed) {
        BUCKET_WORST_US[bucket].store(gap_us, atomic::Ordering::Relaxed);
    }
}

// One frame's view into the bounded per bucket list. The game thread is the only writer, the scan is
// over at most eight slots, and nothing allocates, which is what makes it safe to run every frame.
fn record_view(bucket: usize, view: i32) {
    let base = bucket * VIEWS_TRACKED;

    for slot in 0..VIEWS_TRACKED {
        let seen = BUCKET_VIEW_ID[base + slot].load(atomic::Ordering::Relaxed);

        if seen == view {
            BUCKET_VIEW_FRAMES[base + slot].fetch_add(1, atomic::Ordering::Relaxed);
            return;
        }

        if seen == VIEW_SLOT_EMPTY {
            BUCKET_VIEW_ID[base + slot].store(view, atomic::Ordering::Relaxed);
            BUCKET_VIEW_FRAMES[base + slot].store(1, atomic::Ordering::Relaxed);
            BUCKET_VIEWS_SEEN[bucket].fetch_add(1, atomic::Ordering::Relaxed);
            return;
        }
    }
}

// The view that filled this bucket most, and how many more views were seen besides it. Computed at
// report time, so the frame path above never searches.
fn dominant_view(bucket: usize) -> (i32, usize) {
    let base = bucket * VIEWS_TRACKED;
    let mut best_view = 0;
    let mut best_frames = 0;

    for slot in 0..VIEWS_TRACKED {
        let frames = BUCKET_VIEW_FRAMES[base + slot].load(atomic::Ordering::Relaxed);

        if frames > best_frames {
            best_frames = frames;
            best_view = BUCKET_VIEW_ID[base + slot].load(atomic::Ordering::Relaxed);
        }
    }

    let seen = BUCKET_VIEWS_SEEN[bucket].load(atomic::Ordering::Relaxed);

    (best_view, seen.saturating_sub(1))
}

// Armed by `umamusume::init`, which is where the config is already read. The switch is a plain integer
// because this runs once per frame and a config read per frame is the exact pattern AGENTS section 6
// lists as an overhead bug.
pub fn init() {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    ProbeSuite::start();
    ENABLED.store(1, atomic::Ordering::Relaxed);
    info!(
        "Frame clock probe: measuring the gap between GameSystem Update ticks in microseconds, a gap over {} ms is counted as a stall, and a view that stands for {} s gets one watchdog snapshot",
        SLOW_FRAME_US / 1_000,
        ProbeSuite::WATCHDOG_SECS
    );
}

// Called from the GameSystem update detour beside the other probe reports.
pub fn report_if_due() {
    if ENABLED.load(atomic::Ordering::Relaxed) == 0 {
        return;
    }

    let now = ProbeSuite::now_us();

    if let Some((view, stood_us)) = ProbeSuite::WATCH.take_due(now) {
        let doors = ProbeSuite::all_doors();
        info!("{}", ProbeSuite::watchdog_line(view, stood_us, &doors, now));
    }

    let gaps = GAPS.load(atomic::Ordering::Relaxed);
    let now_sec = now / 1_000_000;
    let last_sec = LAST_REPORT_SEC.load(atomic::Ordering::Relaxed);
    let last_gaps = LAST_GAPS.load(atomic::Ordering::Relaxed);

    if !report_due(now_sec, last_sec, gaps, last_gaps, REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_GAPS.store(gaps, atomic::Ordering::Relaxed);

    let total_us = TOTAL_US.load(atomic::Ordering::Relaxed);
    let mean_ms = match gaps {
        0 => 0.0,
        n => total_us as f64 / n as f64 / 1_000.0,
    };

    let mut buckets = String::new();

    for index in 0..BUCKET_COUNT {
        let count = BUCKET_GAPS[index].load(atomic::Ordering::Relaxed);

        if count == 0 {
            continue;
        }

        let (view, more) = dominant_view(index);
        // `+4 more` says the bucket is mixed rather than pretending one view owns it.
        let label = match more {
            0 => format!("view {view}"),
            n => format!("view {view} +{n} more"),
        };

        let _ = write!(
            buckets,
            " {}({}) frames {} over {} ms slow {} worst {} ms",
            BUCKET_NAMES[index],
            label,
            count,
            BUCKET_US[index].load(atomic::Ordering::Relaxed) / 1_000,
            BUCKET_SLOW[index].load(atomic::Ordering::Relaxed),
            BUCKET_WORST_US[index].load(atomic::Ordering::Relaxed) / 1_000
        );
    }

    let worst_at = match WORST_AT_US.load(atomic::Ordering::Relaxed) {
        at if at >= 0 => format!(" at {} s", at / 1_000_000),
        _ => String::new(),
    };

    info!(
        "Frame clock totals at {now_sec} s: frames {gaps} over {} ms mean {mean_ms:.2} ms worst {} ms{worst_at} slow frames over {} ms {} ({} ms), dropped {}",
        total_us / 1_000,
        WORST_US.load(atomic::Ordering::Relaxed) / 1_000,
        SLOW_FRAME_US / 1_000,
        SLOW_GAPS.load(atomic::Ordering::Relaxed),
        SLOW_US.load(atomic::Ordering::Relaxed) / 1_000,
        DROPPED.load(atomic::Ordering::Relaxed)
    );
    info!("Frame clock by screen:{buckets}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il2cpp::hook::umamusume::TrainingCuttProbe::ViewBucket;

    #[test]
    fn a_gap_is_only_a_frame_when_it_has_a_start() {
        // The shipped decision, the same function `observe_frame` calls. The first gap has no previous tick and a
        // negative gap is a clock that moved backwards, and neither is a frame.
        assert_eq!(frame_gap_us(-1, 500), None);
        assert_eq!(frame_gap_us(900, 500), None);
        assert_eq!(frame_gap_us(500, 500), Some(0));
        assert_eq!(frame_gap_us(5_000, 10_000), Some(5_000));
    }

    #[test]
    fn a_200_fps_frame_reads_as_its_own_5_ms() {
        // The millisecond clock this replaced read a 5 ms frame as 4, 5 or 6 ms depending on where the tick fell.
        assert_eq!(frame_gap_us(1_000_123, 1_005_123), Some(5_000));
    }

    #[test]
    fn a_bucket_is_labelled_by_the_view_that_filled_it() {
        // Each test works in its own bucket so the shared statics cannot interfere across a parallel run.
        let bucket = ViewBucket::Gacha as usize;

        for _ in 0..3 {
            record_view(bucket, 1621);
        }

        record_view(bucket, 0);

        assert_eq!(dominant_view(bucket), (1621, 1), "run 11 labelled a mixed bucket by its first frame");
    }

    #[test]
    fn a_bucket_that_saw_more_views_than_it_tracks_says_so() {
        let bucket = ViewBucket::StoryEvent as usize;
        let first = 7001;

        for index in 0..VIEWS_TRACKED {
            record_view(bucket, first + index as i32);
        }

        for index in 0..VIEWS_TRACKED + 2 {
            record_view(bucket, 8001 + index as i32);
        }

        let (view, more) = dominant_view(bucket);

        assert_eq!(view, first, "the first tracked view still owns the bucket");
        assert_eq!(more, VIEWS_TRACKED - 1, "the report tells the reader views were seen beyond the tracked list");
    }

    #[test]
    fn a_stall_is_counted_at_the_line_the_probe_prints() {
        // The real `gap_is_slow` from the module, so the line the report prints and the line that counts
        // are the same line.
        assert!(!gap_is_slow(50_000), "a frame exactly at the line is not a stall");
        assert!(gap_is_slow(50_001));
        assert!(gap_is_slow(20_000_000), "a load gap is a stall, and the report says so in the open");
    }

    #[test]
    fn a_bucket_keeps_its_own_worst_frame() {
        // Run 12 printed `training screen(view 1101 +3 more) ... slow 40 worst 0 ms`, which cannot be
        // true: 40 frames over 50 ms means that screen has a frame over 50 ms. The bucket number was
        // only written on the frame that also beat the session, so a screen holding stalls while another
        // screen held the session's worst reported zero.
        let training = ViewBucket::Training as usize;
        let other = ViewBucket::Other as usize;

        record_worst(training, 2_299_000, 13_000_000);
        record_worst(other, 90_000, 14_000_000);
        record_worst(other, 60_000, 15_000_000);
        record_worst(other, 30_000, 16_000_000);

        assert_eq!(WORST_US.load(atomic::Ordering::Relaxed), 2_299_000, "the session record keeps the largest frame");
        assert_eq!(BUCKET_WORST_US[training].load(atomic::Ordering::Relaxed), 2_299_000);
        assert_eq!(
            BUCKET_WORST_US[other].load(atomic::Ordering::Relaxed),
            90_000,
            "a stall that is not the session's worst still belongs to the screen it landed on"
        );
    }
}
