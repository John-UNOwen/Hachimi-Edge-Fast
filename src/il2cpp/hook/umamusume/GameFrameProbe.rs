use std::sync::atomic::{self, AtomicI32, AtomicI64, AtomicUsize};
use std::sync::OnceLock;
use std::time::Instant;
use std::fmt::Write as _;

use crate::{
    core::Hachimi,
    il2cpp::hook::umamusume::{
        SceneManager,
        TrainingCuttProbe::{
            bucket_for, current_view_id, report_due, BUCKET_COUNT, BUCKET_NAMES, PROBE_DETAIL_LIMIT,
            REPORT_INTERVAL_SECS,
        },
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
// It is deliberately coarse. One `Instant` per frame and a few atomic adds, no allocation, no lock, no
// format string, and nothing at all when `debug_mode` is off. The clock is not high resolution on purpose:
// a frame gap is a millisecond measurement, and a measurement that cannot see a 40 us detour is the right
// one to trust about what the player waits for.
//
// A gap over SLOW_FRAME_MS is counted as a stall. Three runs of this fork have never measured a stall, so
// the number is a question rather than a finding, and it is reported as a count beside its total time so a
// long stall and many short ones cannot be confused with each other.

// A frame this long is reported as a stall. 50 ms is a third of the 20 fps the training cut timeline is
// authored at (run 10 read `get_TargetFps` as 20), so a frame past it is a frame the animation already lost.
const SLOW_FRAME_MS: i64 = 50;
// The first frame after a load is not a frame the game drew; it is the gap the loader left behind. The very
// first gap is dropped for that reason, and the report says how many gaps it ever dropped.
const DROP_FIRST_GAP: usize = 1;

static START: OnceLock<Instant> = OnceLock::new();
static ENABLED: AtomicUsize = AtomicUsize::new(0);
static LAST_MS: AtomicI64 = AtomicI64::new(-1);
static GAPS: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicUsize = AtomicUsize::new(0);
static TOTAL_MS: AtomicI64 = AtomicI64::new(0);
static WORST_MS: AtomicI64 = AtomicI64::new(0);
static SLOW_GAPS: AtomicUsize = AtomicUsize::new(0);
static SLOW_MS: AtomicI64 = AtomicI64::new(0);
static BUCKET_GAPS: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static BUCKET_MS: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static BUCKET_SLOW: [AtomicUsize; BUCKET_COUNT] = [const { AtomicUsize::new(0) }; BUCKET_COUNT];
static BUCKET_WORST: [AtomicI64; BUCKET_COUNT] = [const { AtomicI64::new(0) }; BUCKET_COUNT];
static WORST_AT_MS: AtomicI64 = AtomicI64::new(-1);
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

fn elapsed_ms() -> i64 {
    match START.get() {
        Some(start) => start.elapsed().as_millis() as i64,
        None => -1,
    }
}

// Called from `GameSystem_Update` before anything else the detour does, so the gap it measures is the gap
// the game actually took between two of its own ticks and not the gap this probe spent inside it.
pub fn observe_frame() {
    if ENABLED.load(atomic::Ordering::Relaxed) == 0 {
        return;
    }

    let now = elapsed_ms();

    if now < 0 {
        return;
    }

    let last = LAST_MS.swap(now, atomic::Ordering::Relaxed);
    let gap = now - last;

    if last < 0 || gap < 0 {
        DROPPED.fetch_add(DROP_FIRST_GAP, atomic::Ordering::Relaxed);
        return;
    }

    GAPS.fetch_add(1, atomic::Ordering::Relaxed);
    let view = current_view_id();
    let bucket = bucket_for(view, SceneManager::is_race_scene_family()) as usize;

    TOTAL_MS.fetch_add(gap, atomic::Ordering::Relaxed);
    BUCKET_GAPS[bucket].fetch_add(1, atomic::Ordering::Relaxed);
    BUCKET_MS[bucket].fetch_add(gap, atomic::Ordering::Relaxed);

    if gap > WORST_MS.load(atomic::Ordering::Relaxed) {
        WORST_MS.store(gap, atomic::Ordering::Relaxed);
        BUCKET_WORST[bucket].store(gap, atomic::Ordering::Relaxed);
        WORST_AT_MS.store(now, atomic::Ordering::Relaxed);
    }

    if gap_is_slow(gap) {
        SLOW_GAPS.fetch_add(1, atomic::Ordering::Relaxed);
        SLOW_MS.fetch_add(gap, atomic::Ordering::Relaxed);
        BUCKET_SLOW[bucket].fetch_add(1, atomic::Ordering::Relaxed);

        // A stall is only useful if a run can say where it happened, so the first ones are named with
        // their place in the session and the view they landed on. The cap is the same one every other
        // probe in this fork uses: a frame path must not format, so the reporting stops early and the
        // totals line keeps counting.
        let shown = STALLS_SHOWN.fetch_add(1, atomic::Ordering::Relaxed);

        if shown < PROBE_DETAIL_LIMIT {
            info!("Frame clock stall {}: {gap} ms at {} s on view {view} {}", shown + 1, now / 1000, BUCKET_NAMES[bucket]);
        }
    }

    record_view(bucket, view);
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

// One decision, spelled out so a change to it is a change to a test and not a silent edit to a hot path.
fn gap_is_slow(gap: i64) -> bool {
    gap > SLOW_FRAME_MS
}

// Armed by `umamusume::init`, which is where the config is already read. The switch is a plain integer
// because this runs once per frame and a config read per frame is the exact pattern AGENTS section 6
// lists as an overhead bug.
pub fn init() {
    if !Hachimi::instance().config.load().debug_mode {
        return;
    }

    let _ = START.set(Instant::now());
    ENABLED.store(1, atomic::Ordering::Relaxed);
    info!("Frame clock probe: measuring the gap between GameSystem Update ticks, a gap over {SLOW_FRAME_MS} ms is counted as a stall");
}

// Called from the GameSystem update detour beside the other probe reports.
pub fn report_if_due() {
    if ENABLED.load(atomic::Ordering::Relaxed) == 0 {
        return;
    }

    let gaps = GAPS.load(atomic::Ordering::Relaxed);
    let now_sec = elapsed_ms() / 1000;
    let last_sec = LAST_REPORT_SEC.load(atomic::Ordering::Relaxed);
    let last_gaps = LAST_GAPS.load(atomic::Ordering::Relaxed);

    if !report_due(now_sec, last_sec, gaps, last_gaps, REPORT_INTERVAL_SECS) {
        return;
    }

    LAST_REPORT_SEC.store(now_sec, atomic::Ordering::Relaxed);
    LAST_GAPS.store(gaps, atomic::Ordering::Relaxed);

    let total_ms = TOTAL_MS.load(atomic::Ordering::Relaxed);
    let mean = match gaps {
        0 => 0.0,
        n => total_ms as f64 / n as f64,
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
            BUCKET_MS[index].load(atomic::Ordering::Relaxed),
            BUCKET_SLOW[index].load(atomic::Ordering::Relaxed),
            BUCKET_WORST[index].load(atomic::Ordering::Relaxed)
        );
    }

    let worst_at = match WORST_AT_MS.load(atomic::Ordering::Relaxed) {
        at if at >= 0 => format!(" at {} s", at / 1000),
        _ => String::new(),
    };

    info!(
        "Frame clock totals at {now_sec} s: frames {gaps} over {total_ms} ms mean {mean:.1} ms worst {} ms{worst_at} slow frames over {SLOW_FRAME_MS} ms {} ({} ms), dropped {}",
        WORST_MS.load(atomic::Ordering::Relaxed),
        SLOW_GAPS.load(atomic::Ordering::Relaxed),
        SLOW_MS.load(atomic::Ordering::Relaxed),
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
        // The first gap has no previous tick to be measured against, and a negative gap is a clock that
        // moved backwards, which is not a frame either.
        assert_eq!(elapsed_ms_for(-1, 500), None);
        assert_eq!(elapsed_ms_for(900, 500), None);
        assert_eq!(elapsed_ms_for(500, 500), Some(0));
        assert_eq!(elapsed_ms_for(500, 542), Some(42));
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
        assert!(!gap_is_slow(50), "a frame exactly at the line is not a stall");
        assert!(gap_is_slow(51));
        assert!(gap_is_slow(20_000), "a load gap is a stall, and the report says so in the open");
    }

    // The decision about a gap that has no previous tick, spelled out because `observe_frame` cannot be
    // called from a test without the game clock it reads.
    fn elapsed_ms_for(last: i64, now: i64) -> Option<i64> {
        if last < 0 || now - last < 0 {
            return None;
        }

        Some(now - last)
    }
}
