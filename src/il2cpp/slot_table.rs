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
//! mismatch the caller falls back to the platform `dlsym`, so a different game build
//! degrades to the previous behaviour instead of misreading memory.

#![allow(dead_code)]

#[cfg(unix)]
use std::os::raw::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
fn table_base() -> Option<usize> {
    let cached = BASE.load(Ordering::Acquire);
    if cached != 0 {
        return Some(cached);
    }

    let base = unsafe { find_libunity_base()? };
    if !unsafe { validate(base) } {
        return None;
    }

    BASE.store(base, Ordering::Release);
    let populated = POPULATED.load(Ordering::Relaxed);
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

/// Verify that `base` really is the libunity build this table was extracted from.
///
/// Failure is not cached: the table is filled by the game's own init routine, so this is
/// retried on later calls until the API is up.
unsafe fn validate(base: usize) -> bool {
    if read_u32(base) != ELF_MAGIC {
        return false;
    }
    if !bytes_match(base + gen::INIT_FN_OFFSET, &gen::INIT_FN_FINGERPRINT) {
        return false;
    }
    if !bytes_match(base + gen::RESOLVER_OFFSET, &gen::RESOLVER_FINGERPRINT) {
        return false;
    }
    // Set by the init routine just before it fills the table.
    if read_usize(base + gen::API_CTX_OFFSET) == 0 {
        return false;
    }

    // Sampled slots: the recorded name string must be where we expect it, and enough of
    // the sampled slots must be populated.
    let mut sampled_populated = 0usize;
    for &index in gen::VALIDATION_SLOTS.iter() {
        let Some(slot) = gen::SLOTS.get(index) else {
            return false;
        };
        if !name_string_matches(base + slot.name_off, slot.name) {
            return false;
        }
        if read_usize(base + gen::TABLE_OFFSET + slot.slot as usize * size_of::<usize>()) != 0 {
            sampled_populated += 1;
        }
    }
    if sampled_populated * 2 < gen::VALIDATION_SLOTS.len() {
        return false;
    }

    // Full count, reported for diagnostics (the live "feature matrix" of this VM).
    let populated = gen::SLOTS
        .iter()
        .filter(|s| read_usize(base + gen::TABLE_OFFSET + s.slot as usize * size_of::<usize>()) != 0)
        .count();
    POPULATED.store(populated, Ordering::Relaxed);
    true
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

/// Resolution diagnostic, run at two moments so that load order can be told apart from a
/// genuine resolution failure.
///
/// It answers, on the device and in one log line, the question that static analysis
/// cannot: does the platform `dlsym` find this build's il2cpp API at runtime, or is the
/// slot table the only route? Both are measured over the same name set, so the two
/// routes can be compared directly. `dlsym` winning means the slot table is redundant
/// on that build; `dlsym` losing is exactly the case the table exists for.
///
/// A build whose symbol table is rebuilt by its own protection layer only becomes
/// resolvable *after* that layer has run, so measuring once — at the instant `dlopen`
/// returns — would report a failure the game never actually hits. `phase` labels the run.
/// At most two runs are performed.
///
/// Only symbol lookups are performed — no address is ever called.
#[cfg(target_os = "android")]
pub unsafe fn diagnostic(handle: usize, phase: &str) {
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    if RUNS.fetch_add(1, Ordering::Relaxed) >= 2 {
        return;
    }

    if handle == 0 {
        log::warn!("diag[{phase}]: il2cpp handle is NULL — the dlopen hook never saw the il2cpp library");
        return;
    }

    let total = gen::SLOTS.len();
    let mut by_dlsym = 0usize;
    let mut by_table = 0usize;
    let mut both = 0usize;
    let mut misses: Vec<&'static str> = Vec::new();

    for slot in gen::SLOTS {
        let d = crate::symbols_impl::dlsym(handle as *mut c_void, slot.name);
        let t = resolve(slot.name);
        if d != 0 { by_dlsym += 1; }
        if t != 0 { by_table += 1; }
        if d != 0 && t != 0 { both += 1; }
        if d == 0 && t == 0 && misses.len() < 8 {
            misses.push(slot.name);
        }
    }

    log::info!(
        "diag[{}]: handle {:#x} — dlsym {}/{} slots, slot table {}/{} slots, both {}, libunity base {:?}",
        phase,
        handle,
        by_dlsym, total,
        by_table, total,
        both,
        table_base()
    );
    if !misses.is_empty() {
        log::warn!("diag[{}]: resolved by neither route (first {}): {}", phase, misses.len(), misses.join(", "));
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
}
