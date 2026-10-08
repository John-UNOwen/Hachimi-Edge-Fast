//! Resolution of the il2cpp C API through libunity's `Il2CppApi` slot table.
//!
//! The Global Android build ships a hollowed `libil2cpp.so`: 2347 of its 2388 dynamic
//! symbol entries are zeroed, `.gnu.hash` describes only 16 symbols, and no `il2cpp_*`
//! name appears anywhere in the 190 MB file — so a plain `dlsym` against the on-disk image
//! cannot resolve the API. `libunity.so` carries a generated compat layer instead: its
//! init routine calls `dlsym(handle, name)` for all 234 names (the handle is the one
//! Unity's `libmain.so` bootstrap obtains for libil2cpp) and caches the results in a flat
//! table in libunity's BSS. Whether that underlying `dlsym` succeeds at runtime is
//! device-specific and is measured once by [`diagnostic`]; reading the cached table is the
//! route that works when it does not.
//!
//! This module reads that table in-process. It is injection-agnostic: it only needs
//! libunity to be loaded, which is true for any route that runs code inside the game.
//!
//! The offsets are version pinned to one game build (see
//! [`super::slot_table_generated`]). Before the table is trusted we verify the layout:
//! ELF magic, the init-routine and resolver fingerprints, the API context global, the
//! bare name strings of sampled slots, and that enough slots are populated. On any
//! mismatch the verdict is cached — callers fall back to the platform `dlsym` without
//! probing again per lookup — and a failed verdict is retried only a bounded number of
//! times, so a different game build degrades to the previous behaviour instead of
//! misreading memory.

#[cfg(unix)]
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use fnv::FnvHashMap;
use once_cell::sync::Lazy;

use super::slot_table_generated as gen;

/// ELF magic (`\x7fELF`) as a little-endian u32.
const ELF_MAGIC: u32 = 0x464c_457f;

/// `name -> slot index`, built once from the generated table.
static INDEX: Lazy<FnvHashMap<&'static str, u16>> =
    Lazy::new(|| gen::SLOTS.iter().map(|s| (s.name, s.slot)).collect());

/// The generated module must stay self-consistent.
const _: () = assert!(gen::SLOT_COUNT == gen::SLOTS.len());

/// Cached libunity load address; 0 means "not resolved yet".
static BASE: AtomicUsize = AtomicUsize::new(0);
/// Number of populated slots found during the last successful validation.
static POPULATED: AtomicUsize = AtomicUsize::new(0);
/// Whether the adoption message was already logged.
static LOGGED: AtomicBool = AtomicBool::new(false);
/// Full validations spent so far: one `find_libunity_base` walk of the loaded-object
/// list plus one `validate`.
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
/// The cached failure verdict. Once an attempt fails, [`table_base`] short-circuits on
/// this flag instead of walking and validating again for every caller — without it, all
/// 234 lookups of a [`diagnostic`] run re-ran the whole probe while the API context was
/// still empty.
static FAILED: AtomicBool = AtomicBool::new(false);

/// How many full validations one process may spend. The table is filled by the game's
/// own init routine, so a failure early in loading can legitimately turn into a success
/// later; [`retry_table_base`] spends one attempt at each lifecycle point where the
/// game's il2cpp state may have changed. That makes the retry bounded instead of one
/// full probe per lookup.
const MAX_VALIDATE_ATTEMPTS: u32 = 4;

/// Resolve one il2cpp API function through the libunity slot table.
///
/// Returns 0 when the name is not part of this build's table, when the table cannot be
/// validated yet (for example before the game's il2cpp initialisation ran), or when the
/// slot is NULL. Callers should treat 0 as "fall back / unavailable".
pub fn resolve(name: &str) -> usize {
    let Some(&slot) = INDEX.get(name) else {
        return 0;
    };
    let Some(base) = table_base() else {
        return 0;
    };
    let addr = unsafe { read_usize(base + gen::TABLE_OFFSET + slot as usize * size_of::<usize>()) };
    if addr == 0 {
        log::warn!("slot_table: {} is NULL in slot {}", name, slot);
    }
    addr
}

/// True once the slot table has been validated in this process.
pub fn is_available() -> bool {
    table_base().is_some()
}

/// `(populated, total)` slots as seen during the last successful validation.
pub fn populated_slots() -> Option<(usize, usize)> {
    if !is_available() {
        return None;
    }
    Some((POPULATED.load(Ordering::Relaxed), gen::SLOTS.len()))
}

/// Cached, on-demand libunity load address.
///
/// Success and failure are both cached: after one failed probe every later call is a
/// flag check, not a `dl_iterate_phdr` walk plus a full `validate`. Only
/// [`retry_table_base`] re-probes, at the lifecycle points where the game may have
/// filled the table since the cached verdict.
fn table_base() -> Option<usize> {
    let cached = BASE.load(Ordering::Acquire);
    if cached != 0 {
        return Some(cached);
    }
    if FAILED.load(Ordering::Relaxed) {
        return None;
    }
    probe_table_base()
}

/// Re-probe at a lifecycle point where the game's il2cpp state may have changed since
/// the cached verdict — the bounded retries are what `MAX_VALIDATE_ATTEMPTS` caps.
fn retry_table_base() -> Option<usize> {
    let cached = BASE.load(Ordering::Acquire);
    if cached != 0 {
        return Some(cached);
    }
    FAILED.store(false, Ordering::Relaxed);
    probe_table_base()
}

/// One probe: walk the loaded-object list, validate the layout, cache the verdict.
fn probe_table_base() -> Option<usize> {
    // Claim the attempt before walking so the total number of full probes is bounded
    // by `MAX_VALIDATE_ATTEMPTS` no matter how many callers arrive here.
    let attempt = loop {
        let spent = ATTEMPTS.load(Ordering::Relaxed);
        if spent >= MAX_VALIDATE_ATTEMPTS {
            FAILED.store(true, Ordering::Relaxed);
            return None;
        }
        if ATTEMPTS
            .compare_exchange_weak(spent, spent + 1, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            break spent + 1;
        }
    };

    let outcome = unsafe {
        match find_libunity_base() {
            None => Err("libunity.so is not loaded"),
            Some(base) => validate(base).map(|populated| (base, populated)),
        }
    };
    match outcome {
        Ok((base, populated)) => {
            // Publish the slot count before the base: a caller that sees the base (Acquire)
            // also sees what this validation counted.
            POPULATED.store(populated, Ordering::Relaxed);
            BASE.store(base, Ordering::Release);
            if !LOGGED.swap(true, Ordering::Relaxed) {
                log::info!(
                    "slot_table: resolving il2cpp API from libunity at {:#x} (build {} bytes, {}/{} slots populated)",
                    base,
                    gen::LIBUNITY_FILE_SIZE,
                    populated,
                    gen::SLOTS.len()
                );
            }
            Some(base)
        }
        Err(reason) => {
            // Cache the verdict: later lookups fall back to the platform `dlsym`
            // instead of making this whole probe again.
            FAILED.store(true, Ordering::Relaxed);
            log::warn!(
                "slot_table: validation attempt {attempt}/{MAX_VALIDATE_ATTEMPTS} failed ({reason}); verdict cached, lookups fall back to the platform dlsym until the next retry point",
            );
            None
        }
    }
}

/// Verify that `base` really is the libunity build this table was extracted from.
///
/// Returns the number of populated slots, or the reason the layout was rejected — the
/// caller caches that verdict; see [`table_base`] and `MAX_VALIDATE_ATTEMPTS`. The
/// table is filled by the game's own init routine, so an "empty table" verdict is not
/// final: it is retried a bounded number of times at [`retry_table_base`].
unsafe fn validate(base: usize) -> Result<usize, &'static str> {
    if read_u32(base) != ELF_MAGIC {
        return Err("no ELF image at the recorded base");
    }
    if !bytes_match(base + gen::INIT_FN_OFFSET, &gen::INIT_FN_FINGERPRINT) {
        return Err("init-routine fingerprint mismatch");
    }
    if !bytes_match(base + gen::RESOLVER_OFFSET, &gen::RESOLVER_FINGERPRINT) {
        return Err("resolver fingerprint mismatch");
    }
    // Set by the init routine just before it fills the table: zero means the game's
    // il2cpp init has not run yet — "not now", not "never".
    if read_usize(base + gen::API_CTX_OFFSET) == 0 {
        return Err("API context still empty, the table is not filled yet");
    }

    // Sampled slots: the recorded name string must be where we expect it, and enough of
    // the sampled slots must be populated.
    let mut sampled_populated = 0usize;
    for &index in gen::VALIDATION_SLOTS.iter() {
        let Some(slot) = gen::SLOTS.get(index) else {
            return Err("validation slot out of range");
        };
        if !name_string_matches(base + slot.name_off, slot.name) {
            return Err("sampled slot name mismatch");
        }
        if read_usize(base + gen::TABLE_OFFSET + slot.slot as usize * size_of::<usize>()) != 0 {
            sampled_populated += 1;
        }
    }
    if sampled_populated * 2 < gen::VALIDATION_SLOTS.len() {
        return Err("most sampled slots are NULL");
    }

    // Full count, reported for diagnostics (the live "feature matrix" of this VM).
    let populated = gen::SLOTS
        .iter()
        .filter(|s| read_usize(base + gen::TABLE_OFFSET + s.slot as usize * size_of::<usize>()) != 0)
        .count();
    Ok(populated)
}

#[cfg(unix)]
unsafe fn find_libunity_base() -> Option<usize> {
    struct Search {
        base: usize,
    }

    unsafe extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        data: *mut c_void,
    ) -> libc::c_int {
        let info = &*info;
        if info.dlpi_name.is_null() {
            return 0;
        }
        let name = std::ffi::CStr::from_ptr(info.dlpi_name).to_bytes();
        if name.ends_with(b"libunity.so") {
            let search = &mut *(data as *mut Search);
            if search.base == 0 {
                // libunity's first PT_LOAD has vaddr 0, so the load bias is the image base.
                search.base = info.dlpi_addr as usize;
            }
            return 1; // stop iterating
        }
        0
    }

    let mut search = Search { base: 0 };
    libc::dl_iterate_phdr(Some(visit), &mut search as *mut Search as *mut c_void);
    if search.base == 0 {
        None
    } else {
        Some(search.base)
    }
}

#[cfg(not(unix))]
unsafe fn find_libunity_base() -> Option<usize> {
    None
}

unsafe fn read_u32(addr: usize) -> u32 {
    std::ptr::read_unaligned(addr as *const u32)
}

unsafe fn read_usize(addr: usize) -> usize {
    std::ptr::read_unaligned(addr as *const usize)
}

unsafe fn bytes_match(addr: usize, expected: &[u8]) -> bool {
    let actual = std::slice::from_raw_parts(addr as *const u8, expected.len());
    actual == expected
}

/// Compare a NUL-terminated C string in the game's address space against `name`.
unsafe fn name_string_matches(addr: usize, name: &str) -> bool {
    let len = name.len();
    let actual = std::slice::from_raw_parts(addr as *const u8, len + 1);
    &actual[..len] == name.as_bytes() && actual[len] == 0
}

/// How many per-name detail lines a diagnostic phase logs. The counters are the signal;
/// the names are the lead to follow.
const DETAIL_LIMIT: usize = 8;

/// Per-name comparison of the two resolution routes, filled by [`diagnostic`].
///
/// Counting how many names each route finds cannot tell a good table from a mis-pinned
/// one: `validate` pins the layout (two 16 byte prologues, the name strings at their
/// recorded rodata offsets, 5 of 9 sampled slots non-null) but never checks that the
/// pointer sitting in slot `N` belongs to the name recorded for slot `N`. A table adopted
/// on a libunity it was not extracted from therefore reads as a live pointer in every
/// slot, both routes score 234/234, and [`super::symbols::dlsym`] hands the caller the
/// wrong function in silence. `mismatch` is the count of names where the two routes
/// resolved to *different* addresses — the shape a wrong slot has.
///
/// The comparison only has two facts to compare where `dlsym` resolves. On the hollowed
/// Global build `dlsym` finds nothing, so `mismatch == 0` is expected and proves nothing
/// about the table. `mismatch > 0` means the two routes genuinely disagree: either the
/// table does not belong to this libunity build, or the functions moved after libunity
/// filled it — either way, reading the table hands back a different function than
/// `dlsym` does.
#[derive(Debug, Default)]
pub struct RouteTally {
    /// Names compared.
    pub total: usize,
    /// Names the platform `dlsym` resolved.
    pub by_dlsym: usize,
    /// Names the slot table resolved.
    pub by_table: usize,
    /// Names both routes resolved.
    pub both: usize,
    /// Names both routes resolved, to different addresses.
    pub mismatch: usize,
    /// First names neither route resolved.
    pub misses: Vec<&'static str>,
    /// First `(name, dlsym address, table address)` triples the two routes disagree on.
    pub conflicts: Vec<(&'static str, usize, usize)>,
}

impl RouteTally {
    /// Classify one name. `dlsym` and `table` are 0 when that route has no answer.
    pub fn record(&mut self, name: &'static str, dlsym: usize, table: usize) {
        self.total += 1;
        if dlsym != 0 { self.by_dlsym += 1; }
        if table != 0 { self.by_table += 1; }
        if dlsym != 0 && table != 0 {
            self.both += 1;
            if dlsym != table {
                self.mismatch += 1;
                if self.conflicts.len() < DETAIL_LIMIT {
                    self.conflicts.push((name, dlsym, table));
                }
            }
        }
        if dlsym == 0 && table == 0 && self.misses.len() < DETAIL_LIMIT {
            self.misses.push(name);
        }
    }

    /// The one-line summary of the run. `address mismatch n/m` is over the `m` names both
    /// routes resolved — the only ones where the two addresses can be compared at all.
    pub fn summary_line(&self, phase: &str, handle: usize, base: Option<usize>) -> String {
        let (total, both) = (self.total, self.both);
        format!(
            "diag[{phase}]: handle {handle:#x} — dlsym {}/{total} slots, slot table {}/{total} slots, both {both}, address mismatch {}/{both}, libunity base {base:?}",
            self.by_dlsym,
            self.by_table,
            self.mismatch,
        )
    }

    /// The loud report for a table whose populated slots do not hold the functions their
    /// recorded names name. `None` when the two routes agree wherever both resolved, which
    /// is also what a build whose `dlsym` finds nothing reports — see the note on
    /// [`RouteTally`].
    pub fn mismatch_line(&self, phase: &str) -> Option<String> {
        if self.mismatch == 0 {
            return None;
        }
        let both = self.both;
        let n = self.conflicts.len();
        let detail = self.conflicts
            .iter()
            .map(|(name, d, t)| format!("{name}: dlsym {d:#x} != table {t:#x}"))
            .collect::<Vec<String>>()
            .join(", ");
        Some(format!(
            "diag[{phase}]: {}/{both} slots the two routes both resolve point to DIFFERENT addresses — the adopted slot table answers these names with other functions and symbols::dlsym prefers it over the platform dlsym (not this libunity build's table, or the functions moved after libunity filled it). First {n}: {detail}",
            self.mismatch,
        ))
    }
}

/// Resolution diagnostic, run once per process — at the moment the game has finished
/// loading its il2cpp image, which is the earliest point where the answer is the one
/// the game actually lives with.
///
/// It answers, on the device and in one log line, the question that static analysis
/// cannot: does the platform `dlsym` find this build's il2cpp API at runtime, or is the
/// slot table the only route? Both are measured over the same name set, name by name, so
/// the two routes are compared by address and not just by how much each one found.
/// `dlsym` winning means the slot table is redundant on that build; `dlsym` losing is
/// exactly the case the table exists for; the two resolving the same name to different
/// addresses is a table that does not belong to this libunity showing up as a number
/// instead of passing unnoticed.
///
/// A build whose symbol table is rebuilt by its own protection layer only becomes
/// resolvable *after* that layer has run, so measuring at the instant `dlopen` returns
/// would report a failure the game never actually hits — that is why the sweep waits for
/// load completion, and why an earlier failed probe lives on as the cached verdict
/// inside `table_base` (retried at most `MAX_VALIDATE_ATTEMPTS` times in total) rather
/// than as a second full sweep. The sweep's 234 `resolve` calls then ride on that
/// cached verdict instead of each re-running the probe. `phase` labels the run. At most
/// one run is performed.
///
/// Only symbol lookups are performed — no address is ever called.
#[cfg(target_os = "android")]
pub unsafe fn diagnostic(handle: usize, phase: &str) {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    if RUNS.fetch_add(1, Ordering::Relaxed) >= 1 {
        return;
    }

    if handle == 0 {
        log::warn!("diag[{phase}]: il2cpp handle is NULL — the dlopen hook never saw the il2cpp library");
        return;
    }

    // Lifecycle point: if an earlier probe failed, the game's own init has now had its
    // chance to fill the table, so this is where one of the bounded retries is spent.
    let base = retry_table_base();

    let mut tally = RouteTally::default();
    for slot in gen::SLOTS {
        let d = crate::symbols_impl::dlsym(handle as *mut c_void, slot.name);
        let t = resolve(slot.name);
        tally.record(slot.name, d, t);
    }

    log::info!("{}", tally.summary_line(phase, handle, base));
    if !tally.misses.is_empty() {
        log::warn!("diag[{}]: resolved by neither route (first {}): {}", phase, tally.misses.len(), tally.misses.join(", "));
    }
    if let Some(line) = tally.mismatch_line(phase) {
        log::warn!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spot-check the generated table against the names Hachimi actually requests.
    #[test]
    fn generated_table_is_self_consistent() {
        assert_eq!(gen::SLOTS.len(), gen::SLOT_COUNT);

        let mut slots: Vec<u16> = gen::SLOTS.iter().map(|s| s.slot).collect();
        slots.sort_unstable();
        assert_eq!(slots, (0..gen::SLOT_COUNT as u16).collect::<Vec<u16>>());

        let mut names: Vec<&str> = gen::SLOTS.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), gen::SLOTS.len(), "duplicate names in table");

        for &index in gen::VALIDATION_SLOTS.iter() {
            assert!(index < gen::SLOTS.len(), "validation slot out of range");
        }
        assert!(gen::INIT_FN_FINGERPRINT.len() == gen::RESOLVER_FINGERPRINT.len());
    }

    #[test]
    fn index_maps_every_name_to_its_own_slot() {
        assert_eq!(INDEX.len(), gen::SLOTS.len());
        for slot in gen::SLOTS {
            assert_eq!(INDEX.get(slot.name).copied(), Some(slot.slot));
        }
    }

    /// The reproduction of the defect. A table adopted from the right build and a table
    /// adopted on a libunity it was not extracted from produce *identical*
    /// `by_dlsym`/`by_table`/`both` counts — 234/234 on both routes — so the old tally
    /// reported the mis-pinned case as the healthiest possible outcome. Comparing the two
    /// routes per name separates them: the good table mismatches nothing, the mis-pinned
    /// one mismatches every name both routes resolve.
    #[test]
    fn counts_alone_cannot_tell_a_mispinned_table_from_a_good_one() {
        // Run every real name in the table through both routes, with the table's address
        // offset from dlsym's by `shift` (0 = the table this build was extracted from).
        let measure = |shift: usize| {
            let mut tally = RouteTally::default();
            for (n, slot) in gen::SLOTS.iter().enumerate() {
                tally.record(slot.name, 0x1000 + n, 0x1000 + n + shift);
            }
            tally
        };

        let good = measure(0);
        let mispinned = measure(0x8000);
        assert_eq!(
            (good.by_dlsym, good.by_table, good.both, good.misses.len()),
            (mispinned.by_dlsym, mispinned.by_table, mispinned.both, mispinned.misses.len()),
            "the counters the diagnostic used to report are identical for both cases"
        );
        assert_eq!(good.by_dlsym, gen::SLOT_COUNT);
        assert_eq!(good.mismatch, 0, "a matching table must not be reported as a mismatch");
        assert!(good.conflicts.is_empty());
        assert_eq!(mispinned.mismatch, gen::SLOT_COUNT, "every slot resolving the wrong function must count");
        assert_eq!(mispinned.conflicts.len(), DETAIL_LIMIT, "the log keeps a bounded lead");
        assert_eq!(mispinned.conflicts[0], ("il2cpp_init", 0x1000, 0x9000));

        // And it is visible in the logged lines, not just in a field nobody prints.
        let (good_summary, bad_summary) =
            (good.summary_line("post-load", 0x7abc0000, Some(0x12340000)), mispinned.summary_line("post-load", 0x7abc0000, Some(0x12340000)));
        assert!(good_summary.contains("dlsym 234/234 slots, slot table 234/234 slots, both 234, address mismatch 0/234"), "{good_summary}");
        assert!(bad_summary.contains("dlsym 234/234 slots, slot table 234/234 slots, both 234, address mismatch 234/234"), "{bad_summary}");

        assert!(good.mismatch_line("post-load").is_none(), "a table that agrees is not noise");
        let line = mispinned.mismatch_line("post-load").expect("a mis-pinned table must be reported");
        assert!(line.contains("234/234 slots the two routes both resolve point to DIFFERENT addresses"), "{line}");
        assert!(line.contains("First 8: il2cpp_init: dlsym 0x1000 != table 0x9000"), "{line}");
    }

    /// The routes disagreeing is only a mismatch when both found an address. `dlsym`
    /// finding nothing (the hollowed Global build) and the table answering is the case
    /// the table exists for, and neither-route is the existing miss report.
    #[test]
    fn route_agreement_is_measured_only_where_both_resolved() {
        let mut tally = RouteTally::default();
        tally.record("il2cpp_init", 0x1234, 0x1234);
        tally.record("il2cpp_domain_get", 0, 0x5678);
        tally.record("il2cpp_profiler_install", 0, 0);

        assert_eq!((tally.total, tally.by_dlsym, tally.by_table, tally.both), (3, 1, 2, 1));
        assert_eq!(tally.mismatch, 0);
        assert!(tally.conflicts.is_empty());
        assert_eq!(tally.misses, vec!["il2cpp_profiler_install"]);
    }

    /// Names this build does not implement (and unknown names) must not resolve, so the
    /// caller keeps its previous behaviour instead of reading the wrong slot.
    #[test]
    fn unimplemented_names_are_absent() {
        assert_eq!(INDEX.get("il2cpp_profiler_install").copied(), None);
        assert_eq!(INDEX.get("il2cpp_not_a_real_function").copied(), None);
        assert_eq!(resolve("il2cpp_not_a_real_function"), 0);
    }

    /// Without libunity (host test binaries) the resolver is inert.
    #[cfg(not(target_os = "android"))]
    #[test]
    fn resolve_is_inert_without_libunity() {
        assert!(!is_available());
        assert_eq!(resolve("il2cpp_init"), 0);
        assert_eq!(populated_slots(), None);
    }

    /// The reproduction of the probe-repeat defect. While `validate` fails, every
    /// lookup used to re-run `find_libunity_base` + `validate`, and `diagnostic` made
    /// all 234 lookups per run, twice. A failed verdict must be cached: 234 lookups on
    /// top of it add zero walks.
    #[test]
    fn failed_validation_is_cached_not_re_walked_per_lookup() {
        // Test binaries have no libunity, so every probe fails. Spend the bounded
        // retry budget first: once it is used up, no caller walks the loaded-object
        // list any more, which makes the counting below race-free against the other
        // tests' lookups.
        while ATTEMPTS.load(Ordering::Relaxed) < MAX_VALIDATE_ATTEMPTS {
            assert_eq!(retry_table_base(), None);
        }
        assert_eq!(BASE.load(Ordering::Relaxed), 0);

        // What one `diagnostic` sweep does — a lookup for every name in the table —
        // must not add a single full probe.
        for slot in gen::SLOTS {
            assert_eq!(resolve(slot.name), 0);
        }
        assert_eq!(table_base(), None);
        assert_eq!(
            ATTEMPTS.load(Ordering::Relaxed),
            MAX_VALIDATE_ATTEMPTS,
            "{} lookups on a cached failure verdict must not re-probe",
            gen::SLOT_COUNT
        );
    }

    /// The retry is bounded: once the budget is spent the resolver stays inert (and
    /// cheap) for the life of the process instead of re-probing on every call.
    #[test]
    fn retry_budget_is_bounded() {
        let mut probes = 0;
        while probes < MAX_VALIDATE_ATTEMPTS && retry_table_base().is_none() {
            probes += 1;
        }
        assert!(probes <= MAX_VALIDATE_ATTEMPTS);
        for _ in 0..gen::SLOT_COUNT {
            assert_eq!(table_base(), None);
            assert_eq!(retry_table_base(), None);
        }
        assert!(ATTEMPTS.load(Ordering::Relaxed) <= MAX_VALIDATE_ATTEMPTS);
    }
}
