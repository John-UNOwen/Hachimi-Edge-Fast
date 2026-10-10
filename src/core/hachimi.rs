use std::{fs, path::{Path, PathBuf}, process, sync::{atomic::{self, AtomicBool, AtomicI32, AtomicPtr, AtomicUsize}, Arc, Mutex}, time::{Duration, Instant}};
use arc_swap::ArcSwap;
use fnv::{FnvHashMap, FnvHashSet};
use once_cell::sync::OnceCell;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use textwrap::wrap_algorithms::Penalties;

use crate::{core::{gui, plugin_api::Plugin, updater}, gui_impl, hachimi_impl, il2cpp::{self, hook::guard, hook::umamusume::{CySpringController::SpringUpdateMode, GameSystem}, sql::{CharacterData, SkillDataDesc, SkillInfo}}};

use super::{game::{Game, Region}, ipc, plurals, settings_preset::SettingsPreset, template, template_filters, tl_repo, utils, Error, Interceptor};

pub const REPO_PATH: &str = "kairusds/Hachimi-Edge";
pub const GITHUB_API: &str = "https://api.github.com/repos";
pub const CODEBERG_API: &str = "https://codeberg.org/api/v1/repos";
pub const WEBSITE_URL: &str = "https://hachimi.noccu.art";
pub const UMAPATCHER_UPDATER_DEEPLINK: &str = "umapatcher-edge://update-hachimi";
pub const RACE_MECHANICS_URL: &str = "https://docs.google.com/document/d/15VzW9W2tXBBTibBRbZ8IVpW6HaMX8H0RP03kq6Az7Xg";

/// How many times a shared lock was handed back despite its poison flag in this process. Bumped only
/// in the cold arm of `recover_lock` below, so a clean `lock()` pays nothing for it (AGENTS section
/// 6).
static POISONED_LOCK_RECOVERIES: AtomicUsize = AtomicUsize::new(0);

/// Acquire a shared lock so that a lock poisoned by a thread that died holding it hands **this**
/// call its data instead of a panic. The acquirer every shipped `extern "C"` frame under `src/` reads
/// a shared `Mutex` through (AGENTS section 6, C2), published here, next to the state it guards; the
/// recoveries written out in full in this file (`apply_retrieved_key`, `record_key_into`,
/// `store_plugin_init_callback`, `run_plugin_init_callbacks`) are the same rule, written before it
/// had a name.
///
/// Why `lock().unwrap()` is the wrong answer on this side of a hook boundary, in the order the data
/// moves:
///
/// 1. When a panic unwinds through a `MutexGuard`, its `Drop` sets the cell's poison flag. It does
///    not invalidate the data: every lock in this crate guards the mod's own state - a plugin list,
///    the GUI, a painter, an IME position, a translation cache - and `PoisonError::into_inner()` is
///    exactly the handle back to it.
/// 2. The flag is permanent. One accident makes every later acquirer of that cell a caller that gets
///    `Err(PoisonError)`, for the life of the process.
/// 3. `unwrap()` turns that value into a **new panic**, at the acquirer's frame. If that frame is an
///    `extern "C"` one - a MinHook detour, a subclassed window procedure, a swap-chain `Present`, an
///    exported plugin entry - it is `nounwind`, so nothing can unwind the panic out: the runtime
///    prints `panic in a function that cannot unwind` and aborts. `target\scratch\i4_poison_repro.rs`
///    measures both halves out of process, one shape per run: `unwrap` answers nothing and exits
///    `0xC0000409`; this acquirer answers three frame calls with the cell's data intact and the
///    process alive. And the abort is silent, because `windows/hachimi_impl.rs` has already killed
///    `UnityCrashHandler64.exe`.
///
/// What the recovery trades, said out loud: it hands back a value a panic may have left
/// half-updated, instead of ending the game. For these cells that is a wrong pixel, a stale cache
/// entry or one skipped translation, and the alternative is the process dying on the frame after it -
/// which is the choice AGENTS section 6 already makes for the sqlite key (`:155`, `:212`) and the
/// plugin queue (`:287-293`). A cell whose own invariant cannot survive a half-update must not be
/// acquired this way; none of the cells here is one, and each one's own doc says what it holds.
///
/// Cost on a path a detour runs per call: `#[inline]` over `lock()` with a cold `into_inner()` arm -
/// the same instructions `unwrap()` compiles on the success path. No allocation, no formatting, no
/// log, no lock taken twice (AGENTS section 6).
#[inline]
pub fn recover_lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|poisoned| {
        // Cold arm only: a clean `lock()` never reaches it, so this counter is not a per-call cost
        // (AGENTS section 6). It is what makes the recovery countable in a run instead of invisible.
        POISONED_LOCK_RECOVERIES.fetch_add(1, atomic::Ordering::Relaxed);
        poisoned.into_inner()
    })
}

/// How many times a shared lock was handed back despite its poison flag, across this process.
pub fn poisoned_lock_recoveries() -> usize {
    POISONED_LOCK_RECOVERIES.load(atomic::Ordering::Relaxed)
}

/// The line a run reads, assembled apart from the `info!` for the reason `guard` gives for splitting
/// `trip_report` from `report_trips`: the claim this item ends with is a claim about a log line, and
/// the test process installs no logger. One `String`, on the path that runs once per process.
pub fn poisoned_lock_report() -> String {
    format!(
        "Poisoned shared locks recovered: {} acquisition(s) handed their data instead of a panic across a hook boundary",
        poisoned_lock_recoveries()
    )
}

/// Printed at `DLL_PROCESS_DETACH` beside the barrier's own trip report. A clean run prints it with
/// `0` in it, which is the number that says the hazard never fired.
pub fn report_poisoned_lock_recoveries() {
    info!("{}", poisoned_lock_report());
}

/// Which door reached the take-down counts first. Three doors can say them: the detach branch in
/// `src/windows/main.rs`, the process's own exit call in `src/windows/hook.rs`, and the game window's
/// teardown message in `src/windows/wnd_hook.rs`. Every run so far ended before any of them printed
/// anything: no run reached the detach branch, and run 32 showed the exit call door standing on a path
/// this game does not take when it closes normally.
static TAKE_DOWN_CLAIMED: AtomicBool = AtomicBool::new(false);

/// True for the first caller only, so a session says the counts once whichever door gets there.
pub fn claim_take_down_report() -> bool {
    !TAKE_DOWN_CLAIMED.swap(true, atomic::Ordering::SeqCst)
}

/// The counts a run reads at the end of a session, said once whichever door got there first. Each
/// door names itself before calling this, so a run can see both doors and one set of counts. Cold
/// path, reached once per process: the reports assemble one `String` each and nothing here stands on
/// a path a detour runs.
pub fn report_take_down_once() {
    if !claim_take_down_report() {
        return;
    }

    crate::il2cpp::hook::guard::report_trips();
    report_poisoned_lock_recoveries();
}

type Sqlite3OpenV2Fn = extern "C" fn(filename: *const i8, pp_db: *mut *mut std::ffi::c_void, flags: i32, z_vfs: *const i8) -> i32;
type Sqlite3KeyFn = extern "C" fn(db: *mut std::ffi::c_void, p_key: *const std::ffi::c_void, n_key: i32) -> i32;

// The two detours below are hook boundaries: MinHook writes a jump into the game's
// `sqlite3_open_v2` / `sqlite3_key` in `libnative.dll`, and the frame that lands on the detour is the
// game's own sqlite call. Nothing may leave these bodies unwound, and nothing may call a target that
// was never published (AGENTS section 6, C2, C1).
//
// What the shape they carried got wrong, in the order the data moves:
//
// 1. `Interceptor::hook` is create **and arm** on this platform (`windows/interceptor_impl.rs`:
//    `create_hook` then `enable_hook`), and the original was only written *after* it returned. The
//    jump into the detour is live the moment `hook` returns, so a `sqlite3_key` call from any other
//    thread in that window reached `ORIG_SQLITE3_KEY.unwrap()` on a `None`.
// 2. `static mut Option<extern "C" fn>` is 16 bytes written by the thread that loaded `libnative.dll`
//    while the game's other threads read it on every sqlite call. Unsynchronised: a reader that sees
//    the discriminant before the pointer calls through whatever the old bytes held.
// 3. `RETRIEVED_RAW_KEY.lock().unwrap()` sat in both bodies. These two hooks are the only writers of
//    the key `il2cpp::sql::key_retrieved` reads, so one thread that died holding that lock turned
//    every later sqlite call the game makes into a panic across a boundary rustc refuses to unwind
//    through. Measured in `target/scratch/check_c2_sqlite` (a standalone mirror of both bodies built
//    with `rustc`, run four ways): the old body, called the way a trampoline calls it, exits
//    `0xC0000409` - `panic in a function that cannot unwind` - on both the unpublished original and a
//    poisoned lock; the fixed body on the same two inputs returns `SQLITE_ERROR` and the process lives.
//
// `AtomicUsize` is the shape that answers 1 and 2: a release store, an acquire load, and `0` meaning
// "no original published yet", which AGENTS section 2 keeps inert. A published original is still
// stored after `hook` returns - there is no create-without-arm entry on `Interceptor` - so the window
// a call can land in is the few instructions after arming, and what it now gets is a refusal, not a
// panic and not a jump through 0.
static ORIG_SQLITE3_OPEN_V2: AtomicUsize = AtomicUsize::new(0);
static ORIG_SQLITE3_KEY: AtomicUsize = AtomicUsize::new(0);

/// `SQLITE_ERROR`. The only answer a sqlite call this mod cannot forward gives: `SQLITE_OK` (0) would
/// be the mod inventing a database it never opened, or a key it never applied.
const SQLITE_ERROR: i32 = 1;

/// `n_key` is a length the game supplied. Copying an unbounded one is an out-of-bounds read (the C9
/// shape) or a multi-gigabyte allocation, and an allocation failure aborts instead of unwinding, so a
/// length past this is refused before either happens. The keys this game passes are tens of bytes.
const MAX_SQLITE_KEY_BYTES: usize = 4096;

/// One line per detour, never one per call (AGENTS section 6). Each latch starts as the address of
/// the code that owns it rather than as the same `false` bytes as its neighbours, because a linker
/// folding identical data folds latches onto one another - the same reason `guard::invented_answer`
/// takes its latch that way. A swap to null spends it. The five seeds are five different addresses so
/// no two of them are ever candidates for that folding.
static SQLITE3_OPEN_NO_ORIG: AtomicPtr<()> = AtomicPtr::new(sqlite3_open_v2_hook as *mut ());
static SQLITE3_KEY_NO_ORIG: AtomicPtr<()> = AtomicPtr::new(sqlite3_key_hook as *mut ());
static SQLITE3_APPLY_NO_ORIG: AtomicPtr<()> = AtomicPtr::new(apply_retrieved_key as *mut ());
static SQLITE3_KEY_LENGTH_REFUSED: AtomicPtr<()> = AtomicPtr::new(refuse_key_length as *mut ());

/// "A key is already in the record", and it is an `AtomicPtr` seeded with its own function for the
/// reason above: a `static AtomicBool = false` here is a latch whose bytes are identical to someone
/// else's, and the two would become one flag. Non-null means nothing captured yet; the capture spends
/// it, and a capture the barrier stopped leaves it unspent.
static SQLITE3_KEY_RECORDED: AtomicPtr<()> = AtomicPtr::new(record_key_into as *mut ());

#[cold]
fn claim_cold_report(latch: &AtomicPtr<()>) -> bool {
    !latch.swap(std::ptr::null_mut(), atomic::Ordering::AcqRel).is_null()
}

#[cold]
fn refuse_key_length(n_key: i32) {
    if claim_cold_report(&SQLITE3_KEY_LENGTH_REFUSED) {
        warn!("sqlite3_key reported a key of {} bytes; longer than {}, it is not captured", n_key, MAX_SQLITE_KEY_BYTES);
    }
}

#[inline]
fn sqlite3_open_v2_orig() -> Option<Sqlite3OpenV2Fn> {
    match ORIG_SQLITE3_OPEN_V2.load(atomic::Ordering::Acquire) {
        0 => None,
        addr => Some(unsafe { std::mem::transmute::<usize, Sqlite3OpenV2Fn>(addr) }),
    }
}

#[inline]
fn sqlite3_key_orig() -> Option<Sqlite3KeyFn> {
    match ORIG_SQLITE3_KEY.load(atomic::Ordering::Acquire) {
        0 => None,
        addr => Some(unsafe { std::mem::transmute::<usize, Sqlite3KeyFn>(addr) }),
    }
}

extern "C" fn sqlite3_open_v2_hook(filename: *const i8, pp_db: *mut *mut std::ffi::c_void, flags: i32, z_vfs: *const i8) -> i32 {
    let Some(orig) = sqlite3_open_v2_orig() else {
        if claim_cold_report(&SQLITE3_OPEN_NO_ORIG) {
            warn!("sqlite3_open_v2 detour has no original published: the game's sqlite3_open_v2 was not forwarded and this open reports SQLITE_ERROR");
        }
        return SQLITE_ERROR;
    };

    // The game's own call to the game's own sqlite, deliberately outside the barrier: a fault inside
    // sqlite is the failure the game has with no mod installed, and this hook does not get to answer
    // it. Everything the mod adds after that call runs behind `detour_barrier`.
    let result = orig(filename, pp_db, flags, z_vfs);

    if result == 0 && !pp_db.is_null() {
        apply_retrieved_key(&crate::il2cpp::sql::RETRIEVED_RAW_KEY, pp_db);
    }

    result
}

extern "C" fn sqlite3_key_hook(db: *mut std::ffi::c_void, p_key: *const std::ffi::c_void, n_key: i32) -> i32 {
    if !p_key.is_null() {
        record_key_into(&crate::il2cpp::sql::RETRIEVED_RAW_KEY, &SQLITE3_KEY_RECORDED, p_key, n_key);
    }

    let Some(orig) = sqlite3_key_orig() else {
        if claim_cold_report(&SQLITE3_KEY_NO_ORIG) {
            warn!("sqlite3_key detour has no original published: the game's sqlite3_key was not forwarded and this call reports SQLITE_ERROR");
        }
        return SQLITE_ERROR;
    };

    orig(db, p_key, n_key)
}

/// The mod half of the open detour: hand the database this open just produced the key the game
/// already gave us. `AUTO_UNLOCK_NEXT_DB` is the one-shot `il2cpp::sql::MetaData::load_from_db` arms,
/// and the rule it implements is unchanged - one key, applied to the next database that opens.
///
/// The key is taken out as a copy, so the shared lock is **out of this frame** before anything foreign
/// is called. A `std::Mutex` is not reentrant, and the old body held it across `sqlite3_key`: that is
/// how this detour made `il2cpp::sql::key_retrieved` - the shipped meta-table guard's key read, on
/// asset-load paths - wait on a call into sqlite, and a fault inside that call would have abandoned
/// the lock with no `Drop` to release it (the C frame stops a fault by returning out of the frames it
/// wrapped, AGENTS section 6).
fn apply_retrieved_key(cell: &Mutex<Vec<u8>>, pp_db: *mut *mut std::ffi::c_void) {
    if !crate::il2cpp::sql::AUTO_UNLOCK_NEXT_DB.swap(false, atomic::Ordering::Relaxed) {
        return;
    }

    let key = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    if key.is_empty() {
        return;
    }

    let Some(key_orig) = sqlite3_key_orig() else {
        if claim_cold_report(&SQLITE3_APPLY_NO_ORIG) {
            warn!("sqlite3_key detour has no original published: the retrieved key was not applied to this database");
        }
        return;
    };

    // Behind the barrier, because this call is the mod's own - made on a handle read through a
    // pointer the caller supplied, with a key the mod captured. A fault here is answered as a key that
    // was not applied: no unwind into sqlite, no replay of the call that faulted, and no lock left
    // stranded, because the key lock already left this frame.
    let _ = guard::detour_barrier(|_answer| {
        let db = unsafe { *pp_db };

        if !db.is_null() {
            key_orig(db, key.as_ptr() as *const std::ffi::c_void, key.len() as i32);
        }
    });
}

/// The only writer of `RETRIEVED_RAW_KEY`, and the record `il2cpp::sql::key_retrieved` answers with
/// is unchanged: the first key the game hands to `sqlite3_key`, byte for byte, and nothing after it.
///
/// What changed is *where* the copy happens. `p_key` / `n_key` are values the game supplied, so the
/// copy runs behind the barrier with the length clamped: a bad length or a bad pointer is stopped as
/// "no key captured" instead of crossing a boundary that cannot unwind, and it is stopped before an
/// allocation that would abort rather than unwind. A fault inside the copy abandons the temporary copy
/// (a few kilobytes, on a trip path only) - the barrier returns out of the closure frames it wrapped,
/// so no `Drop` runs there.
///
/// The copy has to happen before this function can tell whether a key is already in hand, because
/// that is the half a bad pointer or a bad length can fault in, and a lock held across a fault the C
/// frame stops by returning out of is a lock no `Drop` ever releases. `recorded` is what keeps the
/// settled case free of all of it: once a key is in the record, a later `sqlite3_key` takes no lock,
/// allocates nothing and copies nothing (AGENTS section 6). A capture that faulted leaves it clear, so
/// a key arriving after one refused call is still captured.
///
/// The cell and the latch are parameters so a unit test drives this shape on state of its own; the
/// shipped writer is the one call that names the shared pair.
fn record_key_into(cell: &Mutex<Vec<u8>>, recorded: &AtomicPtr<()>, p_key: *const std::ffi::c_void, n_key: i32) {
    if recorded.load(atomic::Ordering::Acquire).is_null() {
        return;
    }

    let len = match usize::try_from(n_key) {
        Ok(len) if 0 < len && len <= MAX_SQLITE_KEY_BYTES => len,
        _ => return refuse_key_length(n_key),
    };

    let _ = guard::detour_barrier(|_answer| {
        let bytes = unsafe { std::slice::from_raw_parts(p_key as *const u8, len) }.to_vec();

        let mut key = cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if key.is_empty() {
            *key = bytes;
        }
        // Either this call put the first key in the record or it found one already there. Both end the
        // capture: what `il2cpp::sql::key_retrieved` answers with is the first key, and a second key
        // the game handed over is not information this mod uses.
        recorded.store(std::ptr::null_mut(), atomic::Ordering::Release);
    });
}


// C16 follow-up: the plugin `on_game_initialized` queue (`Hachimi::plugin_init_callbacks`) has one
// writer, `hachimi_register_on_game_initialized` in `core::plugin_api`, which a plugin calls from
// its `init()`, and `on_hooking_finished` runs that `init()` pass *after* the eager
// `GameSystem::on_game_initialized()` that claims the session latch. The only reader the queue had
// sat inside that body, so it always ran on an empty queue: a plugin that registered got `true`
// back and was never called. The two ends are owned here, next to the queue, and the dispatch runs
// after the writers instead of before them.
type PluginInitCallback = (usize, usize);

// Set by `on_hooking_finished` once the plugin `init()` pass has run. From that moment the game is
// initialised and the window in which a plugin could register has closed, so a registrant is fired
// by the call that registered it instead of queued for a pass that already happened.
static PLUGIN_INIT_WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);

// A callback is third-party code and it registers through the same queue, so a dispatch marks
// itself as in progress: a registrant that arrives while a callback is running is taken by the next
// round of that same dispatch rather than by a recursive one.
static PLUGIN_INIT_DISPATCHING: AtomicBool = AtomicBool::new(false);

/// The in-progress mark, released by `Drop` rather than by a line at the end of the body: a hook
/// barrier (C2) that catches a panic out of a dispatch must not leave the mark standing, or the
/// plugin callback API is shut for the rest of the session - the exact failure this change set is
/// fixing.
struct PluginInitDispatchGuard {
    flag: &'static AtomicBool,
}

impl PluginInitDispatchGuard {
    /// Take `flag`, or `None` when a dispatch already holds it on this call stack.
    fn claim(flag: &'static AtomicBool) -> Option<Self> {
        if flag.compare_exchange(false, true, atomic::Ordering::AcqRel, atomic::Ordering::Relaxed).is_err() {
            return None;
        }

        Some(PluginInitDispatchGuard { flag })
    }
}

impl Drop for PluginInitDispatchGuard {
    fn drop(&mut self) {
        self.flag.store(false, atomic::Ordering::Release);
    }
}

// How many batches one dispatch chases, so a callback that registers another callback on every
// firing cannot hold the thread that is starting the game in a loop.
const MAX_PLUGIN_INIT_DISPATCH_ROUNDS: usize = 8;

/// Where a registrant has to land: fired by the call that registered it, or left in the queue.
/// While the plugin `init()` pass is still running the answer is `false`, so a plugin's own
/// callback never fires inside its own `init()` before that `init()` has finished. Once the pass
/// has run the answer is `true`, which is the case C16's latch used to strand. It is `false` again
/// while a dispatch is in progress further up the same call stack, because that dispatch drains
/// what it fired.
fn plugin_init_callback_fires_on_arrival(window_closed: &AtomicBool, dispatching: &AtomicBool) -> bool {
    window_closed.load(atomic::Ordering::Acquire) && !dispatching.load(atomic::Ordering::Acquire)
}

/// Store one registrant. The lock is taken across an `extern "C"` boundary a plugin calls through,
/// and a plugin that faults poisons it, so both paths recover instead of panicking back into
/// third-party code (AGENTS section 6). The entry is stored before the caller looks at the window,
/// which is what makes a registrant that races the window closing impossible to lose.
fn store_plugin_init_callback(queue: &Mutex<Vec<PluginInitCallback>>, entry: PluginInitCallback) {
    match queue.lock() {
        Ok(mut slot) => slot.push(entry),
        Err(poisoned) => {
            warn!("plugin_init_callbacks mutex poisoned, recovering");
            poisoned.into_inner().push(entry);
        }
    }
}

/// Invoke everything the queue holds, plus everything those calls queued, in batches of at most
/// `MAX_PLUGIN_INIT_DISPATCH_ROUNDS`, and report how many callbacks ran. `0` when a dispatch already
/// holds `dispatching`: a registrant that arrives inside a callback belongs to that dispatch, which
/// drains what it fired, rather than to a second dispatch nested inside it.
///
/// The lock is held only long enough to take a batch out: every callback runs with the queue
/// unlocked, because the exported registration entry takes that same lock and a plugin that
/// registers from its own callback would otherwise lock against itself. A callback leaves the queue
/// when it is taken out of it, so a dispatch that is reached twice fires each callback once.
fn run_plugin_init_callbacks(queue: &Mutex<Vec<PluginInitCallback>>, dispatching: &'static AtomicBool) -> usize {
    let Some(_dispatching) = PluginInitDispatchGuard::claim(dispatching) else {
        return 0;
    };

    let mut fired = 0;

    for _ in 0..MAX_PLUGIN_INIT_DISPATCH_ROUNDS {
        let batch = match queue.lock() {
            Ok(mut slot) => std::mem::take(&mut *slot),
            Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
        };

        if batch.is_empty() { break; }

        for (callback, userdata) in batch {
            // An unresolved registrant stays inert: never call through address 0.
            if callback == 0 { continue; }

            let callback: crate::core::plugin_api::GameInitializedCallback = unsafe { std::mem::transmute(callback) };
            unsafe { callback(userdata as *mut std::ffi::c_void) };

            fired += 1;
        }
    }

    fired
}

pub struct Hachimi {
    // Hooking stuff
    pub interceptor: Interceptor,
    pub hooking_finished: AtomicBool,
    pub plugins: Mutex<Vec<Plugin>>,
    pub plugin_init_callbacks: Mutex<Vec<(usize, usize)>>,
    #[cfg(target_os = "windows")]
    pub present_callbacks: Mutex<Vec<(usize, usize)>>,

    // Translation repo manager
    pub tl_repo_manager: Mutex<tl_repo::RepoList>,

    // Localized data
    pub localized_data: ArcSwap<LocalizedData>,
    pub tl_updater: Arc<tl_repo::Updater>,
    pub tl_update_cmd: Mutex<Option<crossbeam_channel::Sender<()>>>,

    // Character data
    pub chara_data: ArcSwap<CharacterData>,
    // Untranslated skill info
    pub skill_info: ArcSwap<SkillInfo>,
    pub skill_data_desc: ArcSwap<SkillDataDesc>,

    // Shared properties
    pub game: Game,
    pub config: ArcSwap<Config>,
    pub template_parser: template::Parser,

    /// -1 = default
    pub target_fps: AtomicI32,

    #[cfg(target_os = "windows")]
    pub target_fps_unfocused: AtomicI32,

    #[cfg(target_os = "windows")]
    pub vsync_count: AtomicI32,

    #[cfg(target_os = "windows")]
    pub window_always_on_top: AtomicBool,

    #[cfg(target_os = "windows")]
    pub discord_rpc: AtomicBool,

    pub updater: Arc<updater::Updater>
}

static INSTANCE: OnceCell<Arc<Hachimi>> = OnceCell::new();

static SKILL_DATA_DESC_REBUILD_REQUESTED: AtomicBool = AtomicBool::new(false);

impl Hachimi {
    pub fn init() -> bool {
        if INSTANCE.get().is_some() {
            warn!("Hachimi should be initialized only once");
            return true;
        }

        let instance = match Self::new() {
            Ok(v) => v,
            Err(e) => {
                super::log::init(false, false); // early init to log error
                error!("Init failed: {}", e);
                return false;
            }
        };

        let config = instance.config.load();
        if config.disable_gui_once {
            let mut config = config.as_ref().clone();
            config.disable_gui_once = false;
            _ = instance.save_config(&config);

            config.disable_gui = true;
            instance.config.store(Arc::new(config));
        }

        super::log::init(config.debug_mode, config.enable_file_logging);

        info!("Hachimi {}", env!("HACHIMI_DISPLAY_VERSION"));
        info!("Game region: {}", instance.game.region);

        if let Err(e) = instance.repair_tl_repo_state() {
            error!("TL repo repair failed: {}", e);
        }

        instance.load_localized_data();

        INSTANCE.set(Arc::new(instance)).is_ok()
    }

    pub fn instance() -> Arc<Hachimi> {
        INSTANCE.get().unwrap_or_else(|| {
            error!("FATAL: Attempted to get Hachimi instance before initialization");
            process::exit(1);
        }).clone()
    }

    pub fn is_initialized() -> bool {
        INSTANCE.get().is_some()
    }

    fn new() -> Result<Hachimi, Error> {
        let game = Game::init();
        let config = Self::load_config(&game.data_dir, &game.region)?;

        config.language.set_locale();

        Ok(Hachimi {
            interceptor: Interceptor::default(),
            hooking_finished: AtomicBool::new(false),
            plugins: Mutex::default(),
            plugin_init_callbacks: Mutex::default(),
            #[cfg(target_os = "windows")]
            present_callbacks: Mutex::default(),

            tl_repo_manager: Mutex::new(tl_repo::RepoList::default()),

            // Don't load localized data initially since it might fail, logging the error is not possible here
            localized_data: ArcSwap::default(),
            tl_updater: Arc::default(),
            tl_update_cmd: Mutex::new(None),

            // Same with these
            chara_data: ArcSwap::default(),
            skill_info: ArcSwap::default(),
            skill_data_desc: ArcSwap::default(),

            game,
            template_parser: template::Parser::new(&template_filters::LIST),

            target_fps: AtomicI32::new(config.target_fps.unwrap_or(-1)),

            #[cfg(target_os = "windows")]
            target_fps_unfocused: AtomicI32::new(config.windows.target_fps_unfocused.unwrap_or(-1)),

            #[cfg(target_os = "windows")]
            vsync_count: AtomicI32::new(config.windows.vsync_count),

            #[cfg(target_os = "windows")]
            window_always_on_top: AtomicBool::new(config.windows.window_always_on_top),

            #[cfg(target_os = "windows")]
            discord_rpc: AtomicBool::new(config.windows.discord_rpc),

            updater: Arc::default(),

            config: ArcSwap::new(Arc::new(config))
        })
    }

    // region param is unused?
    fn load_config(data_dir: &Path, _region: &Region) -> Result<Config, Error> {
        let config_path = data_dir.join("config.json");
        if fs::metadata(&config_path).is_ok() {
            let json = fs::read_to_string(&config_path)?;
            match serde_json::from_str::<Config>(&json) {
                Ok(config) => Ok(config),
                Err(e) => {
                    eprintln!("Failed to parse config: {}", e);
                    gui::request_notification(gui::NotificationRequest::ConfigLoadError);
                    Ok(Config::default())
                }
            }
        }else {
            Ok(Config::default())
        }
    }

    pub fn reload_config(&self) {
        let new_config = match Self::load_config(&self.game.data_dir, &self.game.region) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to reload config: {}", e);
                return;
            }
        };

        new_config.language.set_locale();
        self.config.store(Arc::new(new_config));

        if Hachimi::is_initialized() && self.hooking_finished.load(atomic::Ordering::Relaxed) {
            Hachimi::instance().start_translation_updater_thread();
        }
    }

    pub fn save_config(&self, config: &Config) -> Result<(), Error> {
        fs::create_dir_all(&self.game.data_dir)?;
        let config_path = self.get_data_path("config.json");
        utils::write_json_file(config, &config_path)?;

        Ok(())
    }

    pub fn save_and_reload_config(&self, config: Config) -> Result<(), Error> {
        let old_id = self.config.load().selected_tl_repo_id;
        self.save_config(&config)?;

        config.language.set_locale();
        self.config.store(Arc::new(config));

        let new_config = self.config.load();
        if new_config.selected_tl_repo_id != old_id {
            self.load_localized_data();
            gui::request_notification(gui::NotificationRequest::TLRepoChanged);
        }

        if Hachimi::is_initialized() && self.hooking_finished.load(atomic::Ordering::Relaxed) {
            Hachimi::instance().start_translation_updater_thread();
        }

        Ok(())
    }

    pub fn get_active_tl_dir(&self) -> Option<PathBuf> {
        let id = self.config.load().selected_tl_repo_id?;
        Some(self.get_repo_dir(id))
    }

    pub fn load_localized_data(&self) {
        if self.tl_updater.progress().is_some() {
            warn!("Update in progress, not loading localized data");
            return;
        }

        let config = self.config.load();
        let ld_path = self.get_active_tl_dir().or_else(|| {
            config.localized_data_dir.as_ref().map(|p| self.game.data_dir.join(p))
        });

        let mut new_data = match LocalizedData::new(&self.config.load(), ld_path) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to load localized data: {}", e);
                return;
            }
        };

        if self.game.region == Region::Global {
            for id in 55..=66 {
                new_data.localize_dict.remove(&format!("Common{id:04}"));
            }
        }
        
        self.localized_data.store(Arc::new(new_data));

        if !self.skill_data_desc.load().descs.is_empty() {
            SKILL_DATA_DESC_REBUILD_REQUESTED.store(true, atomic::Ordering::Release);
        }
    }

    pub fn drain_skill_data_desc_rebuild(&self) {
        if !SKILL_DATA_DESC_REBUILD_REQUESTED.swap(false, atomic::Ordering::AcqRel) {
            return;
        }
        if self.skill_data_desc.load().descs.is_empty() {
            return;
        }
        let data = SkillDataDesc::load_from_db();
        if !data.descs.is_empty() {
            self.skill_data_desc.store(Arc::new(data));
        }
    }

    pub fn init_character_data(&self) {
        if self.chara_data.load().chara_ids.is_empty() {
            let data = CharacterData::load_from_db();
            self.chara_data.store(Arc::new(data));
            info!("Character database loaded successfully.");
        }
    }

    pub fn init_skill_info(&self) {
        if self.skill_info.load().skill_names.is_empty() {
            let data = SkillInfo::load_from_db();
            self.skill_info.store(Arc::new(data));
            info!("Skill info loaded successfully.");
        }
    }

    pub fn init_skill_data_desc(&self) {
        if self.skill_data_desc.load().descs.is_empty() {
            let data = SkillDataDesc::load_from_db();
            self.skill_data_desc.store(Arc::new(data));
            info!("Skill data descriptions loaded successfully.");
        }
    }

    pub fn on_dlopen(&self, filename: &str, handle: usize) -> bool {
        let filename_lower = filename.to_lowercase();

        // The writer side of the pair over the two detours: each one publishes the trampoline
        // `hook` hands back with a release store, and the detour reads it with an acquire load.
        // `Interceptor::hook` has already armed the target by the time it returns, so the store is
        // the first thing done after it - the narrower the window in which a call can reach the
        // detour with nothing published is, the better.

        #[cfg(target_os = "windows")]
        if filename_lower.contains("libnative.dll") {
            unsafe {
                use windows::Win32::System::LibraryLoader::GetProcAddress;
                use windows::core::PCSTR;

                let h_module = windows::Win32::Foundation::HMODULE(handle as _);
                let open_addr = GetProcAddress(h_module, PCSTR("sqlite3_open_v2\0".as_ptr()));
                let key_addr = GetProcAddress(h_module, PCSTR("sqlite3_key\0".as_ptr()));

                if let Some(addr) = open_addr {
                    if let Ok(orig) = self.interceptor.hook(addr as usize, sqlite3_open_v2_hook as *const () as usize) {
                        ORIG_SQLITE3_OPEN_V2.store(orig, atomic::Ordering::Release);
                        info!("Successfully hooked native sqlite3_open_v2 (Windows)");
                    }
                }
                if let Some(addr) = key_addr {
                    if let Ok(orig) = self.interceptor.hook(addr as usize, sqlite3_key_hook as *const () as usize) {
                        ORIG_SQLITE3_KEY.store(orig, atomic::Ordering::Release);
                        info!("Successfully hooked native sqlite3_key (Windows)");
                    }
                }
            }
        }

        #[cfg(target_os = "android")]
        if filename_lower.contains("libnative.so") {
            unsafe {
                let handle_ptr = handle as *mut libc::c_void;

                let open_sym = b"sqlite3_open_v2\0".as_ptr() as *const libc::c_char;
                let key_sym = b"sqlite3_key\0".as_ptr() as *const libc::c_char;

                let open_addr = libc::dlsym(handle_ptr, open_sym);
                let key_addr = libc::dlsym(handle_ptr, key_sym);

                if !open_addr.is_null() {
                    if let Ok(orig) = self.interceptor.hook(open_addr as usize, sqlite3_open_v2_hook  as *const () as usize) {
                        ORIG_SQLITE3_OPEN_V2.store(orig, atomic::Ordering::Release);
                        info!("Successfully hooked native sqlite3_open_v2 (Android)");
                    }
                }
                if !key_addr.is_null() {
                    if let Ok(orig) = self.interceptor.hook(key_addr as usize, sqlite3_key_hook as *const () as usize) {
                        ORIG_SQLITE3_KEY.store(orig, atomic::Ordering::Release);
                        info!("Successfully hooked native sqlite3_key (Android)");
                    }
                }
            }
        }

        // Prevent double initialization
        if self.hooking_finished.load(atomic::Ordering::Relaxed) { return false; }

        if hachimi_impl::is_il2cpp_lib(filename) {
            info!("Got il2cpp handle");
            il2cpp::symbols::set_handle(handle);
            false
        }
        else if hachimi_impl::is_criware_lib(filename) {
            self.on_hooking_finished();
            true
        }
        else {
            false
        }
    }

    /// Backs `hachimi_register_on_game_initialized`: record one plugin `on_game_initialized`
    /// callback. The queue and its dispatch belong here so the writer and the reader cannot drift
    /// apart again - a registrant that arrives after the plugin `init()` pass already ran is fired
    /// by this call, and one that arrives inside a callback is left to that callback's own dispatch.
    pub fn register_plugin_init_callback(&self, callback: usize, userdata: usize) {
        store_plugin_init_callback(&self.plugin_init_callbacks, (callback, userdata));

        if plugin_init_callback_fires_on_arrival(&PLUGIN_INIT_WINDOW_CLOSED, &PLUGIN_INIT_DISPATCHING) {
            self.dispatch_plugin_init_callbacks();
        }
    }

    /// Run every plugin `on_game_initialized` callback that is waiting, each one once. Safe to call
    /// from every arrival - the eager pass at the end of hook arming, the game's own `InitializeGame`
    /// finishing, a soft reset - because a callback is out of the queue before it runs.
    pub fn dispatch_plugin_init_callbacks(&self) {
        let fired = run_plugin_init_callbacks(&self.plugin_init_callbacks, &PLUGIN_INIT_DISPATCHING);

        if fired > 0 {
            info!("Plugin init callbacks: {} fired", fired);
        }
    }

    pub fn on_hooking_finished(&self) {
        self.hooking_finished.store(true, atomic::Ordering::Relaxed);

        info!("GameAssembly finished loading");
        // Re-measure resolution now that the game has finished loading its il2cpp image,
        // before anything is actually called through it.
        il2cpp::symbols::recheck();
        il2cpp::symbols::init();
        il2cpp::hook::init();

        // By the time it finished hooking the game will have already finished initializing
        GameSystem::on_game_initialized();

        let config = self.config.load();
        if !config.disable_gui {
            gui_impl::init();
        }

        if config.enable_ipc {
            ipc::start_http(config.ipc_listen_all);
        }

        hachimi_impl::on_hooking_finished(self);

        Hachimi::instance().start_translation_updater_thread();

        for plugin in recover_lock(&self.plugins).iter() {
            info!("Initializing plugin: {}", plugin.name);
            let res = plugin.init();
            if !res.is_ok() {
                info!("Plugin init failed");
            }
        }

        // This pass is where a plugin registers its `on_game_initialized` callback, and it runs
        // after the eager `GameSystem::on_game_initialized()` above, so the queue is dispatched
        // here: after every registrant has had its turn, on the thread that ran the initialization
        // pass, and the window is closed for anything that arrives later (C16 follow-up).
        PLUGIN_INIT_WINDOW_CLOSED.store(true, atomic::Ordering::Release);
        self.dispatch_plugin_init_callbacks();
    }

    pub fn get_data_path<P: AsRef<Path>>(&self, rel_path: P) -> PathBuf {
        self.game.data_dir.join(rel_path)
    }

    pub fn get_repo_dir(&self, id: u32) -> PathBuf {
        if id == 1 {
            let legacy = self.game.data_dir.join("localized_data");
            if legacy.is_dir() {
                return legacy;
            }
        }
        self.game.data_dir.join(format!("localized_data_{id}"))
    }

    fn repair_tl_repo_state(&self) -> Result<(), Error> {
        let repos_path = self.get_data_path(".tl_repos");
        let old_data_dir = self.game.data_dir.join("localized_data");
        let mut manager = recover_lock(&self.tl_repo_manager);

        if !repos_path.exists() && old_data_dir.is_dir() {
            info!("Found legacy 'localized_data' folder and no .tl_repos; migrating…");

            let config = self.config.load();
            if let Some(index) = &config.translation_repo_index {
                let id = manager.add(index.clone());
                manager.save(&repos_path)?;

                let mut new_config = (**config).clone();
                new_config.selected_tl_repo_id = Some(id);
                self.save_and_reload_config(new_config)?;
            } else {
                manager.save(&repos_path)?;
            }
        }

        *manager = if repos_path.exists() {
            tl_repo::RepoList::load(&repos_path).unwrap_or_else(|e| {
                warn!("Failed to load .tl_repos ({e}); starting fresh");
                tl_repo::RepoList::default()
            })
        } else {
            tl_repo::RepoList::default()
        };

        let config = self.config.load();
        let index = config.translation_repo_index.clone();
        let current_id = config.selected_tl_repo_id;

        let mut manager_dirty = false;

        match current_id {
            Some(id) => {
                if manager.find_by_id(id) != index.as_deref() {
                    warn!("TL repo ID {id} does not match index {index:?}; re-resolving");

                    let mut cleared = (**config).clone();
                    cleared.selected_tl_repo_id = None;
                    self.save_config(&cleared)?;
                    self.config.store(Arc::new(cleared));

                    if let Some(ref idx) = index {
                        let new_id = match manager.find_by_index(idx) {
                            Some(existing) => existing,
                            None => {
                                let nid = manager.add(idx.clone());
                                manager_dirty = true;
                                nid
                            }
                        };

                        let mut new_config = self.config.load().as_ref().clone();
                        new_config.selected_tl_repo_id = Some(new_id);
                        self.save_and_reload_config(new_config)?;
                    } else {
                        let data_dir = self.get_repo_dir(id);
                        if !data_dir.is_dir() {
                            warn!("TL repo data folder '{}' is missing, clearing localised data until next update...", data_dir.display());
                            self.localized_data.store(Arc::new(LocalizedData::default()));
                            gui::request_notification(gui::NotificationRequest::TLFolderMissing);
                        }
                    }
                }
            }

            None => {
                if let Some(ref idx) = index {
                    let id = match manager.find_by_index(idx) {
                        Some(existing) => existing,
                        None => {
                            let nid = manager.add(idx.clone());
                            manager_dirty = true;
                            nid
                        }
                    };
                    let mut new_config = (**config).clone();
                    new_config.selected_tl_repo_id = Some(id);
                    self.save_and_reload_config(new_config)?;
                }
            }
        }

        if manager_dirty {
            manager.save(&repos_path)?;
        }

        if let Some(id) = self.config.load().selected_tl_repo_id {
            let old_cache = self.get_data_path(".tl_repo_cache");
            if old_cache.exists() {
                let new_cache = self.get_data_path(format!(".tl_repo_cache_{}", id));
                info!("Migrating standalone legacy tl repo cache file to {}", new_cache.display());
                if let Err(e) = fs::rename(&old_cache, &new_cache) {
                    warn!("Failed to rename legacy tp repo cache file: {e}");
                }
            }
        }

        Ok(())
    }

    pub fn run_auto_update_check(&self) {
        if !self.config.load().disable_auto_update_check {
            // Check for hachimi updates first, then translations
            // Don't auto check for tl updates if it's not up to date
            self.updater.clone().check_for_updates(|new_update| {
                let hachimi = Hachimi::instance();
                if !new_update && !hachimi.config.load().translator_mode {
                    hachimi.tl_updater.clone().check_for_updates(false, false);
                }
            });
        }
    }

    pub fn start_translation_updater_thread(self: Arc<Self>) {
        let mut cmd_lock = recover_lock(&self.tl_update_cmd);

        // drop the old sender to signal the existing thread to exit.
        // Its recv_timeout will return Disconnected within 1 second.
        *cmd_lock = None;

        let config = self.config.load();
        if config.tl_auto_updater_mode == TLAutoUpdaterMode::Disabled
            || config.tl_auto_updater_interval_sec == 0
            || config.translator_mode
        {
            return;
        }

        let (tx, rx) = crossbeam_channel::bounded::<()>(1);
        *cmd_lock = Some(tx);
        drop(cmd_lock);

        let interval = Duration::from_secs(config.tl_auto_updater_interval_sec);

        std::thread::Builder::new()
            .name("translation_updater_thread".into())
            .spawn(move || {
                let mut next_check = Instant::now() + interval;
                let mut last_interval = interval;

                loop {
                    let config = self.config.load();
                    if config.tl_auto_updater_mode == TLAutoUpdaterMode::Disabled
                        || config.tl_auto_updater_interval_sec == 0
                        || config.translator_mode
                    {
                        break;
                    }

                    let interval = Duration::from_secs(config.tl_auto_updater_interval_sec);

                    // realign timer if interval changed
                    if interval != last_interval {
                        next_check = Instant::now() + interval;
                        last_interval = interval;
                    }

                    // don't re-check while user hasn't acted on the current update
                    if self.tl_updater.has_pending_update() {
                        next_check = Instant::now() + interval;
                        continue;
                    }

                    if Instant::now() >= next_check {
                        let silent = config.tl_auto_updater_mode == TLAutoUpdaterMode::Silent;
                        info!("Running translation updater check (Silent: {})...", silent);
                        self.tl_updater.clone().check_for_updates(false, silent);
                        next_check = Instant::now() + interval;
                    }

                    // interruptible sleep. wakes at least once/sec,
                    // exits immediately when sender is dropped (restart/stop).
                    let remaining = next_check.saturating_duration_since(Instant::now());
                    let sleep = remaining.min(Duration::from_secs(1));

                    match rx.recv_timeout(sleep) {
                        Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .expect("Failed to spawn translation updater thread");
    }
}

fn default_serde_instance<'a, T: Deserialize<'a>>() -> Option<T> {
    let empty_data = std::iter::empty::<((), ())>();
    let empty_deserializer = serde::de::value::MapDeserializer::<_, serde::de::value::Error>::new(empty_data);
    T::deserialize(empty_deserializer).ok()
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
pub enum TLAutoUpdaterMode {
    Disabled,
    Periodic,
    Silent
}

impl Default for TLAutoUpdaterMode {
    fn default() -> Self { Self::Disabled }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CaptionConfig {
    #[serde(default)]
    pub caption_enable: bool,
    #[serde(default = "CaptionConfig::default_lines_char_count")]
    pub caption_lines_char_count: i32,
    #[serde(default = "CaptionConfig::default_font_size")]
    pub caption_font_size: i32,
    #[serde(default = "CaptionConfig::default_color")]
    pub caption_color: String,
    #[serde(default = "CaptionConfig::default_outline_size")]
    pub caption_outline_size: String,
    #[serde(default = "CaptionConfig::default_outline_color")]
    pub caption_outline_color: String,
    #[serde(default = "CaptionConfig::default_bg_alpha")]
    pub caption_bg_alpha: f32,
    #[serde(default = "CaptionConfig::default_pos_x")]
    pub caption_pos_x: f32,
    #[serde(default = "CaptionConfig::default_pos_y")]
    pub caption_pos_y: f32,
}

impl Default for CaptionConfig {
    fn default() -> Self {
        Self {
            caption_enable: false,
            caption_lines_char_count: 26,
            caption_font_size: 50,
            caption_color: "White".to_owned(),
            caption_outline_size: "L".to_owned(),
            caption_outline_color: "Brown".to_owned(),
            caption_bg_alpha: 0.0,
            caption_pos_x: 0.0,
            caption_pos_y: -3.0,
        }
    }
}

impl CaptionConfig {
    fn default_lines_char_count() -> i32 { 26 }
    fn default_font_size() -> i32 { 50 }
    fn default_color() -> String { "White".to_owned() }
    fn default_outline_size() -> String { "L".to_owned() }
    fn default_outline_color() -> String { "Brown".to_owned() }
    fn default_bg_alpha() -> f32 { 0.0 }
    fn default_pos_x() -> f32 { 0.0 }
    fn default_pos_y() -> f32 { -3.0 }
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq)]
pub struct RaceStatHudCloneConfig {
    #[serde(default = "Config::default_race_stat_hud_drag_x")]
    pub drag_x: f32,
    #[serde(default = "Config::default_race_stat_hud_drag_y")]
    pub drag_y: f32,
    #[serde(default)]
    pub selected_character: usize,
    #[serde(default)]
    pub toggle_key: Option<i32>,
    #[serde(default)]
    pub open: bool
}

impl RaceStatHudCloneConfig {
    pub fn drag_pos(&self) -> Option<(f32, f32)> {
        if (0.0..=1.0).contains(&self.drag_x) && (0.0..=1.0).contains(&self.drag_y) {
            Some((self.drag_x, self.drag_y))
        } else {
            None
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Config {
    #[serde(default)]
    pub debug_mode: bool,
    #[serde(default)]
    pub enable_file_logging: bool,
    #[serde(default)]
    pub apply_atlas_workaround: bool,
    #[serde(default)]
    pub translator_mode: bool,
    #[serde(default)]
    pub disable_gui: bool,
    #[serde(default)]
    pub disable_gui_once: bool,
    // legacy fallback path. populated by old versions, new code uses selected_tl_repo_id + get_active_tl_dir() exclusively
    // do NOT write this in new code
    pub localized_data_dir: Option<String>,
    pub target_fps: Option<i32>,
    #[serde(default = "Config::default_open_browser_url")]
    pub open_browser_url: String,
    #[serde(default = "Config::default_virtual_res_mult")]
    pub virtual_res_mult: f32,
    #[serde(default)]
    pub selected_tl_repo_id: Option<u32>,
    pub translation_repo_index: Option<String>,
    #[serde(default)]
    pub skip_first_time_setup: bool,
    #[serde(default)]
    pub lazy_translation_updates: bool,
    #[serde(default)]
    pub etag_translation_updates: bool,
    #[serde(default)]
    pub disable_auto_update_check: bool,

    #[serde(default)]
    pub tl_auto_updater_mode: TLAutoUpdaterMode,
    #[serde(default = "Config::default_tl_auto_updater_interval_sec")]
    pub tl_auto_updater_interval_sec: u64,

    #[serde(default)]
    pub disable_translations: bool,
    #[serde(default = "Config::default_gui_scale")]
    pub gui_scale: f32,
    #[serde(default = "Config::default_ui_scale")]
    pub ui_scale: f32,
    #[serde(default = "Config::default_render_scale")]
    pub render_scale: f32,
    #[serde(default)]
    pub msaa: crate::il2cpp::hook::umamusume::GraphicSettings::MsaaQuality,
    #[serde(default)]
    pub aniso_level: crate::il2cpp::hook::UnityEngine_CoreModule::Texture::AnisoLevel,
    #[serde(default)]
    pub shadow_resolution: crate::il2cpp::hook::umamusume::CameraData::ShadowResolution,
    #[serde(default)]
    pub graphics_quality: crate::il2cpp::hook::umamusume::GraphicSettings::GraphicsQuality,
    #[serde(default)]
    pub shadow_distance: f32,
    #[serde(default)]
    pub soft_shadows: bool,
    #[serde(default)]
    pub soft_shadow_quality: crate::il2cpp::hook::Unity_RenderPipelines_Universal_Runtime::UniversalRenderPipelineAsset::SoftShadowQuality,
    #[serde(default)]
    pub shadow_depth_bias: Option<f32>,
    #[serde(default)]
    pub shadow_normal_bias: Option<f32>,
    #[serde(default)]
    pub force_chara_shadows: bool,
    #[serde(default)]
    pub story_shadow_type: crate::il2cpp::hook::umamusume::StoryTimelineBg3DClipData::ShadowType3d,
    #[serde(default = "Config::default_story_choice_auto_select_delay")]
    pub story_choice_auto_select_delay: f32,
    #[serde(default = "Config::default_story_tcps_multiplier")]
    pub story_tcps_multiplier: f32,
    #[serde(default)]
    pub enable_ipc: bool,
    #[serde(default)]
    pub ipc_listen_all: bool,
    #[serde(default)]
    pub force_allow_dynamic_camera: bool,
    #[serde(default)]
    pub live_theater_allow_same_chara: bool,
    #[serde(default = "Config::default_live_vocals_swap")]
    pub live_vocals_swap: [i32; 6],
    #[serde(default)]
    pub skill_info_dialog: bool,
    #[serde(default)]
    pub skill_data_desc: bool,
    #[serde(default)]
    pub old_config_editor: bool,
    #[serde(default)]
    pub homescreen_bgseason: crate::il2cpp::hook::umamusume::GameDefine::BgSeason,
    pub sugoi_url: Option<String>,
    #[serde(default)]
    pub auto_translate_stories: bool,
    #[serde(default)]
    pub auto_translate_localize: bool,
    #[serde(default)]
    pub disable_skill_name_translation: bool,
    #[serde(default)]
    pub disable_factor_name_translation: bool,
    #[serde(default)]
    pub hide_ingame_ui_hotkey: bool,
    #[serde(default)]
    pub race_stat_hud: bool,
    #[serde(default)]
    pub race_stat_hud_toggle_button: bool,
    #[serde(default)]
    pub race_stat_had_autoscroll_0: bool,
    #[serde(default)]
    pub race_stat_had_autoscroll_1: bool,
    #[serde(default)]
    pub race_stat_hud_draggable: bool,
    #[serde(default)]
    pub race_stat_hud_draggable_save: bool,
    #[serde(default)]
    pub race_stat_hud_resizable: bool,
    #[serde(default = "Config::default_race_stat_hud_drag_x")]
    pub race_stat_hud_drag_x: f32,
    #[serde(default = "Config::default_race_stat_hud_drag_y")]
    pub race_stat_hud_drag_y: f32,
    #[serde(default)]
    pub race_stat_hud_main_open: bool,
    #[serde(default)]
    pub race_stat_hud_clones: Vec<RaceStatHudCloneConfig>,
    #[serde(default)]
    pub race_stat_hud_selected_character: Option<usize>,
    #[serde(default)]
    pub race_stat_hud_persist_clones: bool,
    #[serde(default)]
    pub race_stat_hud_persist_selected_index: bool,
    #[serde(default = "Config::default_race_stat_hud_width_scale")]
    pub race_stat_hud_width_scale: f32,
    #[serde(default = "Config::default_race_stat_hud_height_scale")]
    pub race_stat_hud_height_scale: f32,
    #[serde(default = "Config::default_race_stat_hud_opacity_scale")]
    pub race_stat_hud_opacity_scale: f32,
    #[serde(default)]
    pub race_playback_slider: bool,
    #[serde(default = "Config::default_true")]
    pub race_playback_slider_always: bool,
    #[serde(default)]
    pub race_playback_button: bool,
    #[serde(default)]
    pub race_playback_key_enable: bool,
    #[serde(flatten)]
    pub caption: CaptionConfig,
    #[serde(default)]
    pub disable_tap_effect: bool,
    #[serde(default)]
    pub language: Language,
    #[serde(default = "Config::default_meta_index_url")]
    pub meta_index_url: String,
    #[serde(default)]
    pub ipv4_only: bool,
    pub physics_update_mode: Option<SpringUpdateMode>,
    #[serde(default)]
    pub cyspring_mono_uncap_frame_scale: bool,
    #[serde(default = "Config::default_ui_animation_scale")]
    pub ui_animation_scale: f32,
    #[serde(default = "Config::default_time_scale")]
    pub time_scale: f32,
    // Division factors for Gallop's own animation-duration constants.
    #[serde(default = "Config::default_animation_speed")]
    pub transition_speed: f32,
    #[serde(default = "Config::default_animation_speed")]
    pub result_screen_speed: f32,
    #[serde(default = "Config::default_animation_speed")]
    pub story_speed: f32,
    // The training stat plate cascade interval, on `TrainingParamChangeUI::InitializePlateList` alone. Runs 31 to
    // 33 closed a cascade at or above the interval the door was handed rather than that interval over the
    // multiplied tween clock, so this lever is the only one that reaches that completion (C58, ledger item 74).
    #[serde(default = "Config::default_animation_speed")]
    pub training_plate_speed: f32,
    // The training cut-in's own speed channel, on `SingleModeUtils::GetTrainingCutTimeScale` alone. It raises a
    // scale rather than dividing a duration, so its clamp is the time-scale one (`MIN_TIME_SCALE` to
    // `MAX_TIME_SCALE`) and what the door may hand the game stops at `MAX_TRAINING_CUT_TIME_SCALE`
    // (`AnimationSpeed.rs`, ledger item 77).
    #[serde(default = "Config::default_animation_speed")]
    pub training_cut_speed: f32,
    #[serde(default)]
    pub auto_skip_result_screens: bool,
    // Raises the story and training High Speed settings that the Global options screen does not
    // expose, using StoryManager's own max and save path.
    #[serde(default)]
    pub high_speed_settings: bool,
    // Asks the story timeline to run in the game's own high speed mode through
    // StoryTimelineController::SetHighSpeedType. Off never touches it.
    #[serde(default)]
    pub story_high_speed_mode: bool,
    // The settings a player switches between to measure the speed options against each other
    // (core::settings_preset). The name is what `Config snapshot:` prints, and the saved list is the
    // player's own snapshots the picker shows next to the three built-in arms.
    #[serde(default)]
    pub settings_preset_name: String,
    #[serde(default)]
    pub settings_presets: Vec<SettingsPreset>,
    #[serde(default)]
    pub trainer_live_landscape: bool,
    #[serde(default)]
    pub live_slider_always_show: bool,
    #[serde(default)]
    pub live_playback_loop: bool,
    #[serde(default)]
    pub champions_live_show_text: bool,
    #[serde(default = "Config::default_champions_live_resource_id")]
    pub champions_live_resource_id: i32,
    #[serde(default = "Config::default_champions_live_year")]
    pub champions_live_year: i32,
    #[serde(default)]
    pub hide_now_loading: bool,
    #[serde(default)]
    pub replace_to_builtin_font: bool,
    pub custom_font_file: Option<String>,
    #[serde(default)]
    pub custom_font_file_warning: bool,
    #[serde(default)]
    pub disabled_hooks: FnvHashSet<String>,

    // theme settings
    #[serde(default = "Config::default_ui_accent")]
    pub ui_accent_color: egui::Color32,
    #[serde(default = "Config::default_window_fill")]
    pub ui_window_fill: egui::Color32,
    #[serde(default = "Config::default_panel_fill")]
    pub ui_panel_fill: egui::Color32,
    #[serde(default = "Config::default_extreme_bg")]
    pub ui_extreme_bg_color: egui::Color32,
    #[serde(default = "Config::default_text_color")]
    pub ui_text_color: egui::Color32,
    #[serde(default = "Config::default_window_rounding")]
    pub ui_window_rounding: f32,

    #[cfg(target_os = "windows")]
    #[serde(flatten)]
    pub windows: hachimi_impl::Config,

    #[cfg(target_os = "android")]
    #[serde(flatten)]
    pub android: hachimi_impl::Config
}

impl Config {
    fn default_open_browser_url() -> String { "https://www.google.com/".to_owned() }
    fn default_virtual_res_mult() -> f32 { 1.0 }
    fn default_ui_scale() -> f32 { 1.0 }
    fn default_render_scale() -> f32 { 1.0 }
    fn default_gui_scale() -> f32 { 1.0 }
    fn default_story_choice_auto_select_delay() -> f32 { 1.2 }
    fn default_story_tcps_multiplier() -> f32 { 3.0 }
    fn default_meta_index_url() -> String { "https://gitlab.com/umatl/hachimi-meta/-/raw/main/meta.json".to_owned() }
    fn default_ui_animation_scale() -> f32 { 1.0 }
    fn default_time_scale() -> f32 { 1.0 }
    fn default_animation_speed() -> f32 { 1.0 }
    fn default_live_vocals_swap() -> [i32; 6] { [0; 6] }
    fn default_champions_live_resource_id() -> i32 { 15 }
    fn default_champions_live_year() -> i32 { 2025 }
    pub fn default_ui_accent() -> egui::Color32 { egui::Color32::from_rgb(100, 150, 240) }
    pub fn default_window_fill() -> egui::Color32 { egui::Color32::from_rgba_premultiplied(27, 27, 27, 220) }
    pub fn default_panel_fill() -> egui::Color32 { egui::Color32::from_rgba_premultiplied(27, 27, 27, 220) }
    pub fn default_extreme_bg() -> egui::Color32 { egui::Color32::from_rgb(15, 15, 15) }
    pub fn default_text_color() -> egui::Color32 { egui::Color32::from_gray(170) }
    pub fn default_window_rounding() -> f32 { 10.0 }
    fn default_tl_auto_updater_interval_sec() -> u64 { 3600 }
    fn default_race_stat_hud_drag_x() -> f32 { -1.0 }
    fn default_race_stat_hud_drag_y() -> f32 { -1.0 }
    fn default_race_stat_hud_width_scale() -> f32 { 1.0 }
    fn default_race_stat_hud_height_scale() -> f32 { 1.0 }
    fn default_race_stat_hud_opacity_scale() -> f32 { 1.0 }
    fn default_true() -> bool { true }
}

impl Default for Config {
    fn default() -> Self {
        default_serde_instance().expect("default instance")
    }
}

#[derive(Deserialize, Default, Clone)]
pub struct OsOption<T> {
    #[cfg(target_os = "android")]
    android: Option<T>,

    #[cfg(target_os = "windows")]
    windows: Option<T>
}

impl<T> OsOption<T> {
    pub fn as_ref(&self) -> Option<&T> {
        #[cfg(target_os = "android")]
        return self.android.as_ref();

        #[cfg(target_os = "windows")]
        return self.windows.as_ref();
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Deserialize, Serialize)]
#[allow(non_camel_case_types)]
pub enum Language {
    #[serde(rename = "en")]
    English,

    #[serde(rename = "zh-tw")]
    TChinese,

    #[serde(rename = "zh-cn")]
    SChinese,

    #[serde(rename = "vi")]
    Vietnamese,

    #[serde(rename = "id")]
    Indonesian,

    #[serde(rename = "es")]
    Spanish,

    #[serde(rename = "pt-br")]
    BPortuguese,

    #[serde(rename = "fil")]
    Filipino,

    #[serde(rename = "ru")]
    Russian,

    #[serde(rename = "ko")]
    Korean
}

impl Default for Language {
    fn default() -> Self {
        let locale = sys_locale::get_locale().as_deref().unwrap_or("en").to_lowercase();
        if locale.contains("zh-hk") || locale.contains("zh-tw") || locale.contains("zh-hant") {
            Self::TChinese
        } else if locale.contains("zh") {
            Self::SChinese
        } else if locale.starts_with("vi") {
            Self::Vietnamese
        } else if locale.starts_with("id") {
            Self::Indonesian
        } else if locale.starts_with("es") {
            Self::Spanish
        } else if locale.starts_with("pt-br") {
            Self::BPortuguese
        } else if locale.starts_with("fil") {
            Self::Filipino
        } else if locale.starts_with("ru") {
            Self::Russian
        } else if locale.starts_with("ko") {
            Self::Korean
        } else {
            Self::English
        }
    }
}

impl Language {
    pub const CHOICES: &[(Self, &'static str)] = &[
        Self::English.choice(),
        Self::TChinese.choice(),
        Self::SChinese.choice(),
        Self::Vietnamese.choice(),
        Self::Indonesian.choice(),
        Self::Spanish.choice(),
        Self::BPortuguese.choice(),
        Self::Filipino.choice(),
        Self::Russian.choice(),
        Self::Korean.choice()
    ];

    pub fn set_locale(&self) {
        rust_i18n::set_locale(self.locale_str());
    }

    pub const fn locale_str(&self) -> &'static str {
        match self {
            Language::English => "en",
            Language::TChinese => "zh-tw",
            Language::SChinese => "zh-cn",
            Language::Vietnamese => "vi",
            Language::Indonesian => "id",
            Language::Spanish => "es",
            Language::BPortuguese => "pt-br",
            Language::Filipino => "fil",
            Language::Russian => "ru",
            Language::Korean => "ko"
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Language::English => "English",
            Language::TChinese => "繁體中文",
            Language::SChinese => "简体中文",
            Language::Vietnamese => "Tiếng Việt",
            Language::Indonesian => "Bahasa Indonesia",
            Language::Spanish => "Español (ES)",
            Language::BPortuguese => "Português (Brasil)",
            Language::Filipino => "Filipino",
            Language::Russian => "Русский",
            Language::Korean => "한국어"
        }
    }

    pub const fn choice(self) -> (Self, &'static str) {
        (self, self.name())
    }
}

#[derive(Default)]
pub struct LocalizedData {
    pub config: LocalizedDataConfig,
    path: Option<PathBuf>,

    pub localize_dict: FnvHashMap<String, String>,
    pub hashed_dict: FnvHashMap<u64, String>,
    pub text_data_dict: FnvHashMap<i32, FnvHashMap<i32, String>>, // {"category": {"index": "text"}}
    pub character_system_text_dict: FnvHashMap<i32, FnvHashMap<i32, String>>, // {"character_id": {"voice_id": "text"}}
    pub race_jikkyo_comment_dict: FnvHashMap<i32, String>, // {"id": "text"}
    pub race_jikkyo_message_dict: FnvHashMap<i32, String>, // {"id": "text"}
    pub skill_data_desc_dict: FnvHashMap<String, String>, // {"skill_data_desc.<key>": "text"}
    assets_path: Option<PathBuf>,

    pub plural_form: plurals::Resolver,
    pub ordinal_form: plurals::Resolver,

    pub wrapper_penalties: Penalties
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CustomRubyBlock {
    pub block_index: i32,
    pub rubies: Vec<CustomRubyDef>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CustomRubyDef {
    pub char_x: f32,
    pub char_y: f32,
    pub ruby_text: String,
}

impl LocalizedData {
    fn new(config: &Config, ld_path: Option<PathBuf>) -> Result<LocalizedData, Error> {
        if config.disable_translations {
            return Ok(LocalizedData::default());
        }

        let path = ld_path;
        let config: LocalizedDataConfig = if let Some(ref p) = path {
            // Create .nomedia
            #[cfg(target_os = "android")]
            { _ = fs::OpenOptions::new().create_new(true).write(true).open(p.join(".nomedia")); }

            let ld_config_path = p.join("config.json");
            if fs::metadata(&ld_config_path).is_ok() {
                let json = fs::read_to_string(&ld_config_path)?;
                serde_json::from_str(&json)?
            }
            else {
                warn!("Localized data config not found");
                LocalizedDataConfig::default()
            }
        }
        else {
            LocalizedDataConfig::default()
        };

        let plural_form = Self::parse_plural_form_or_default(&config.plural_form)?;
        let ordinal_form = Self::parse_plural_form_or_default(&config.ordinal_form)?;

        let wrapper_penalties = Self::parse_wrap_penalties_or_default(&config.wrapper_penalties);

        Ok(LocalizedData {
            localize_dict: Self::load_dict_static(&path, config.localize_dict.as_ref()).unwrap_or_default(),
            hashed_dict: Self::load_dict_static(&path, config.hashed_dict.as_ref()).unwrap_or_default(),
            text_data_dict: Self::load_dict_static(&path, config.text_data_dict.as_ref()).unwrap_or_default(),
            character_system_text_dict: Self::load_dict_static(&path, config.character_system_text_dict.as_ref()).unwrap_or_default(),
            race_jikkyo_comment_dict: Self::load_dict_static(&path, config.race_jikkyo_comment_dict.as_ref()).unwrap_or_default(),
            race_jikkyo_message_dict: Self::load_dict_static(&path, config.race_jikkyo_message_dict.as_ref()).unwrap_or_default(),
            skill_data_desc_dict: Self::load_dict_static_ex(&path, Some("skill_data_desc_dict.json"), true).unwrap_or_default(),
            assets_path: path.as_ref()
                .map(|p| config.assets_dir.as_ref()
                    .map(|dir| p.join(dir))
                )
                .unwrap_or_default(),

            plural_form,
            ordinal_form,

            wrapper_penalties,

            config,
            path
        })
    }

    fn load_dict_static_ex<T: DeserializeOwned, P: AsRef<Path>>(ld_path_opt: &Option<PathBuf>, rel_path_opt: Option<P>, silent_fs_error: bool) -> Option<T> {
        let Some(ld_path) = ld_path_opt else {
            return None;
        };
        let Some(rel_path) = rel_path_opt else {
            return None;
        };

        let path = ld_path.join(rel_path);
        let json = match fs::read_to_string(&path) {
            Ok(v) => v,
            Err(e) => {
                if !silent_fs_error {
                    error!("Failed to read '{}': {}", path.display(), e);
                }
                return None;
            }
        };

        let dict = match serde_json::from_str::<T>(&json) {
            Ok(v) => v,
            Err(e) => {
                error!("Failed to parse '{}': {}", path.display(), e);
                return None;
            }
        };

        Some(dict)
    }

    fn load_dict_static<T: DeserializeOwned, P: AsRef<Path>>(ld_path_opt: &Option<PathBuf>, rel_path_opt: Option<P>) -> Option<T> {
        Self::load_dict_static_ex(ld_path_opt, rel_path_opt, false)
    }

    pub fn load_dict<T: DeserializeOwned, P: AsRef<Path>>(&self, rel_path_opt: Option<P>) -> Option<T> {
        Self::load_dict_static(&self.path, rel_path_opt)
    }

    pub fn load_assets_dict<T: DeserializeOwned, P: AsRef<Path>>(&self, rel_path_opt: Option<P>) -> Option<T> {
        Self::load_dict_static_ex(&self.assets_path, rel_path_opt, true)
    }

    fn parse_plural_form_or_default(opt: &Option<String>) -> Result<plurals::Resolver, Error> {
        if let Some(plural_form) = opt {
            Ok(plurals::Resolver::Expr(plurals::Ast::parse(plural_form)?))
        }
        else {
            Ok(plurals::Resolver::Function(|_| 0))
        }
    }

    fn parse_wrap_penalties_or_default(opt: &Option<PenaltiesConfig>) -> Penalties {
        let Some(cfg) = opt else {
            return Penalties::new()
        };
        Penalties {
            nline_penalty: cfg.nline_penalty,
            overflow_penalty: cfg.overflow_penalty,
            short_last_line_fraction: cfg.short_last_line_fraction,
            short_last_line_penalty: cfg.short_last_line_penalty,
            hyphen_penalty: cfg.hyphen_penalty
        }
    }

    pub fn get_assets_path<P: AsRef<Path>>(&self, rel_path: P) -> Option<PathBuf> {
        self.assets_path.as_ref().map(|p| p.join(rel_path))
    }

    pub fn get_data_path<P: AsRef<Path>>(&self, rel_path: P) -> Option<PathBuf> {
        self.path.as_ref().map(|p| p.join(rel_path))
    }

    pub fn load_asset_metadata<P: AsRef<Path>>(&self, rel_path: P) -> AssetMetadata {
        let mut path = rel_path.as_ref().to_owned();
        path.set_extension("json");
        self.load_assets_dict(Some(path)).unwrap_or_else(|| AssetInfo::<()>::default()).metadata()
    }

    pub fn load_asset_info<P: AsRef<Path>, T: DeserializeOwned>(&self, rel_path: P) -> AssetInfo<T> {
        let mut path = rel_path.as_ref().to_owned();
        path.set_extension("json");
        self.load_assets_dict(Some(path)).unwrap_or_else(|| AssetInfo::default())
    }

    pub fn load_custom_story_ruby(&self, ast_ruby_name: &str) -> Option<Vec<CustomRubyBlock>> {
        // let filename = ast_ruby_name.split('/').last().unwrap_or(ast_ruby_name);
        let filename = ast_ruby_name.split('/').next_back().unwrap_or(ast_ruby_name);

        let filename_no_ext = filename.strip_suffix(".asset").unwrap_or(filename);

        let id_str = filename_no_ext.strip_prefix("ast_ruby_")?;

        if id_str.len() < 6 { return None; }

        let category_id = &id_str[0..2];
        let story_id = &id_str[2..6];

        let path = format!("story/data/{}/{}/{}.json", category_id, story_id, filename_no_ext);

        self.load_assets_dict(Some(path))
    }
}

#[derive(Deserialize, Clone)]
pub struct LocalizedDataConfig {
    pub localize_dict: Option<String>,
    pub hashed_dict: Option<String>,
    pub text_data_dict: Option<String>,
    pub character_system_text_dict: Option<String>,
    pub race_jikkyo_comment_dict: Option<String>,
    pub race_jikkyo_message_dict: Option<String>,
    pub assets_dir: Option<String>,
    #[serde(default)]
    pub extra_asset_bundle: OsOption<String>,
    pub replacement_font_name: Option<String>,

    pub plural_form: Option<String>,
    pub ordinal_form: Option<String>,
    #[serde(default)]
    pub ordinal_types: Vec<String>,
    #[serde(default)]
    pub months: Vec<String>,
    pub month_text_format: Option<String>,

    #[serde(default)]
    pub use_text_wrapper: bool,
    // Predefined line widths are counts of cjk characters.
    // 1 cjk char = 2 columns, so setting this value to 2 replicates the default behaviour.
    pub line_width_multiplier: Option<f32>,
    #[serde(default)]
    pub systext_cue_lines: FnvHashMap<String, i32>,
    pub wrapper_penalties: Option<PenaltiesConfig>,

    #[serde(default)]
    pub auto_adjust_story_clip_length: bool,
    pub story_line_count_offset: Option<i32>,
    pub text_frame_line_spacing_multiplier: Option<f32>,
    pub text_frame_font_size_multiplier: Option<f32>,
    pub choice_btn_line_spacing_multiplier: Option<f32>,
    #[serde(default)]
    pub skill_formatting: SkillFormatting,
    #[serde(default)]
    pub text_common_allow_overflow: bool,
    #[serde(default)]
    pub text_common_best_fit: bool,
    #[serde(default)]
    pub now_loading_comic_title_ellipsis: bool,

    #[serde(default)]
    pub remove_ruby: bool,
    pub character_note_top_gallery_button: Option<UITextConfig>,
    pub character_note_top_talk_gallery_button: Option<UITextConfig>,

    pub news_url: Option<String>,

    // RESERVED
    #[serde(default)]
    pub _debug: i32
}

#[derive(Deserialize, Clone)]
pub struct UITextConfig {
    pub text: Option<String>,
    pub font_size: Option<i32>,
    pub line_spacing: Option<f32>
}

impl Default for LocalizedDataConfig {
    fn default() -> Self {
        default_serde_instance().expect("default instance")
    }
}

#[derive(Deserialize)]
pub struct AssetInfo<T> {
    #[cfg(target_os = "android")]
    #[serde(default)]
    android: AssetMetadata,

    #[cfg(target_os = "windows")]
    #[serde(default)]
    windows: AssetMetadata,

    pub data: Option<T>
}

// Can't derive(Default), see rust-lang/rust#26925
impl<T> Default for AssetInfo<T> {
    fn default() -> Self {
        Self {
            #[cfg(target_os = "android")]
            android: Default::default(),

            #[cfg(target_os = "windows")]
            windows: Default::default(),

            data: None
        }
    }
}

impl<T> AssetInfo<T> {
    pub fn metadata(self) -> AssetMetadata {
        #[cfg(target_os = "android")]
        return self.android;

        #[cfg(target_os = "windows")]
        return self.windows;
    }

    pub fn metadata_ref(&self) -> &AssetMetadata {
        #[cfg(target_os = "android")]
        return &self.android;

        #[cfg(target_os = "windows")]
        return &self.windows;
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct AssetMetadata {
    pub bundle_name: Option<String>
}

#[derive(Deserialize, Clone)]
pub struct PenaltiesConfig {
    nline_penalty: usize,
    overflow_penalty: usize,
    short_last_line_fraction: usize,
    short_last_line_penalty: usize,
    hyphen_penalty: usize
}

#[derive(Deserialize, Clone)]
pub struct SkillFormatting {
    #[serde(default = "SkillFormatting::default_length")]
    pub name_length: i32,
    #[serde(default = "SkillFormatting::default_length")]
    pub desc_length: i32,
    #[serde(default = "SkillFormatting::default_lines")]
    pub name_short_lines: i32,

    #[serde(default = "SkillFormatting::default_mult")]
    pub name_short_mult: f32,
    #[serde(default = "SkillFormatting::default_mult")]
    pub name_sp_mult: f32,
}
impl SkillFormatting {
    fn default_length() -> i32 { 18 }
    fn default_lines() -> i32 { 1 }
    fn default_mult() -> f32 { 1.0 }
}

impl Default for SkillFormatting {
    fn default() -> Self {
        SkillFormatting {
            name_length: 13,
            desc_length: 18,
            name_short_lines: 1,
            name_short_mult: 1.0,
            name_sp_mult: 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // C16 follow-up: the queue a plugin fills through `hachimi_register_on_game_initialized`,
    // driven through the same functions the shipped `on_hooking_finished` and
    // `GameSystem::on_game_initialized` call. The callbacks are function pointers, so what they
    // move has to be process state; each test keeps its own.

    /// C2 / item 4: the chain that the 35 shipped `unwrap()`s stood on, driven end to end on one
    /// cell. A writer dies holding a shared lock -> the flag persists and the data does not -> the
    /// next acquirer is a frame a trampoline lands on -> `unwrap()` makes *that* call panic, and a
    /// panic in a `nounwind` frame aborts the process (`0xC0000409`, measured out of process in
    /// `target\scratch\i4_poison_repro.rs`). The frame here is the shipped shape: a lock taken
    /// inside an `extern "C" fn`, reached through an address.
    #[test]
    fn a_poisoned_shared_lock_hands_the_next_frame_its_data_instead_of_a_panic() {
        static CELL: Mutex<Vec<i32>> = Mutex::new(Vec::new());

        /// The wrapper shape: `MAP.lock().unwrap()...` inside the frame, now read through
        /// `recover_lock`. The answer it has to give is the cell's own data, not a refusal.
        extern "C" fn frame_len() -> i32 {
            recover_lock(&CELL).len() as i32
        }

        // The mod's state, before the accident.
        recover_lock(&CELL).extend([10, 20, 30]);

        // Step 1: a thread dies holding the lock. `join` keeping this process alive is the half a
        // detached thread gets for free; a hook frame gets no such half (step 3).
        let writer = std::thread::spawn(|| {
            let _held = recover_lock(&CELL);
            panic!("a mod thread died holding the shared lock");
        });
        assert!(writer.join().is_err(), "the writer was expected to panic while holding the lock");

        // Step 2: the flag is what persists, not the damage. Read it without an unwrap and the cell
        // still hands over the three items it held.
        match CELL.lock() {
            Ok(guard) => panic!("the cell was expected to be poisoned; it answered Ok with {} items", guard.len()),
            Err(poisoned) => assert_eq!(poisoned.into_inner().len(), 3, "the poisoning took the cell's data with it"),
        }

        // Step 3: the game calls the hook again, three times the way a per-frame hook calls it,
        // through an address rather than as an inline call, so the boundary is a real `extern "C"`
        // one. Each call answers the cell's data and none of them unwinds out of the frame.
        let counted_before = poisoned_lock_recoveries();

        let addr = frame_len as *const () as usize;
        let through_trampoline: extern "C" fn() -> i32 = unsafe { std::mem::transmute(addr) };

        for call in 0..3 {
            assert_eq!(through_trampoline(), 3, "frame call {call} after a poisoning did not hand back the cell's data");
        }

        // And the acquirer poisons nothing itself: after three frame calls the next acquirer still
        // gets the cell. `unwrap()` on the third of those calls would have been the abort.
        assert_eq!(recover_lock(&CELL).len(), 3, "a frame call left the cell unusable for the next acquirer");

        // Four poisoned acquisitions (three frame calls plus this one) are counted, which is what
        // makes the detach line a number a run can read rather than a claim. `>=`: other tests run
        // in this same process and share the counter.
        let recovered = poisoned_lock_recoveries() - counted_before;
        assert!(recovered >= 4, "the {recovered} recoveries after the poisoning did not cover the four acquirers");
        assert!(poisoned_lock_report().contains("Poisoned shared locks recovered:"), "the line this item ends with is not the one a run reads");
    }

    /// The other shipped shape, at the sites that hold the GUI behind an `Option`:
    /// `Gui::instance().map(|m| m.lock().unwrap())` with a `let ... else` that hands the call to the
    /// game. The rule this pins is which event takes that branch: a **missing** cell does, a
    /// **poisoned** one does not - the frame keeps working with the cell's own data, and the game's
    /// own answer is not used as a cover for a poisoned lock.
    #[test]
    fn the_games_own_answer_is_for_a_missing_cell_and_not_for_a_poisoned_one() {
        static PRESENT: OnceCell<Mutex<Vec<i32>>> = OnceCell::new();
        static MISSING: OnceCell<Mutex<Vec<i32>>> = OnceCell::new();

        /// -1 is what the shipped `let ... else` answers with: the window procedure's
        /// `orig_fn(hwnd, umsg, ..)`, the game's own handling of the message.
        extern "C" fn frame_answer(cell: &OnceCell<Mutex<Vec<i32>>>) -> i32 {
            let Some(mut gui) = cell.get().map(|m| recover_lock(m)) else {
                return -1;
            };
            gui.push(7);
            gui.len() as i32
        }

        PRESENT.set(Mutex::new(Vec::new())).ok();

        // Poison the present cell the way a per-frame panic does it.
        let writer = std::thread::spawn(|| {
            let _held = PRESENT.get().unwrap().lock().unwrap();
            panic!("a frame died holding the GUI lock");
        });
        assert!(writer.join().is_err(), "the writer was expected to panic while holding the lock");

        // A poisoned cell is still the GUI: the frame takes its branch, not the game's.
        assert_eq!(frame_answer(&PRESENT), 1, "a poisoned GUI cell sent the message to the game instead of the GUI");
        assert_eq!(frame_answer(&PRESENT), 2, "the second frame call lost what the first one put in the cell");

        // A cell that is genuinely absent still answers the game's way - the inert branch is
        // unchanged by this item, and it is only ever reached by an absent cell.
        assert_eq!(frame_answer(&MISSING), -1, "a missing cell stopped answering the game's own handling");
    }

    #[test]
    fn a_plugin_that_registers_after_the_initialization_pass_still_gets_its_callback() {
        static QUEUE: Mutex<Vec<PluginInitCallback>> = Mutex::new(Vec::new());
        static SEEN: Mutex<Vec<i32>> = Mutex::new(Vec::new());
        static DISPATCHING: AtomicBool = AtomicBool::new(false);
        static TAG_FIRST: i32 = 11;
        static TAG_SECOND: i32 = 22;

        unsafe extern "C" fn record_first(userdata: *mut std::ffi::c_void) {
            recover_lock(&SEEN).push(unsafe { *(userdata as *const i32) });
        }
        unsafe extern "C" fn record_second(userdata: *mut std::ffi::c_void) {
            recover_lock(&SEEN).push(unsafe { *(userdata as *const i32) });
        }

        // The order `on_hooking_finished` actually runs in: the eager `GameSystem::on_game_initialized()`
        // dispatch, then the plugin `init()` pass - which is where a plugin registers - then the
        // dispatch that closes the window. Before the fix the only reader sat inside the first of
        // those three, so it ran on an empty queue and the two registrations below were never called.
        let eager_pass = run_plugin_init_callbacks(&QUEUE, &DISPATCHING);

        store_plugin_init_callback(&QUEUE, (record_first as usize, &TAG_FIRST as *const i32 as usize));
        store_plugin_init_callback(&QUEUE, (record_second as usize, &TAG_SECOND as *const i32 as usize));

        let after_plugin_init_pass = run_plugin_init_callbacks(&QUEUE, &DISPATCHING);

        assert_eq!(eager_pass, 0, "the eager pass called a callback a plugin had not registered yet");
        assert_eq!(after_plugin_init_pass, 2, "a plugin that registered after the eager pass was never called");
        assert_eq!(*SEEN.lock().unwrap(), vec![11, 22], "the callbacks ran out of registration order");

        // A callback leaves the queue when it runs, so every later arrival - the game's own
        // `InitializeGame` finishing, a re-init, a soft reset - dispatches nothing. Each callback
        // runs once per session, which is what C16's latch was for.
        assert!(QUEUE.lock().unwrap().is_empty(), "a fired callback stayed in the queue");
        assert_eq!(run_plugin_init_callbacks(&QUEUE, &DISPATCHING), 0, "a later arrival ran a callback again");
        assert_eq!(run_plugin_init_callbacks(&QUEUE, &DISPATCHING), 0, "a third arrival ran a callback again");
        assert_eq!(SEEN.lock().unwrap().len(), 2, "a callback ran more than once");

        // And the dispatch left its own mark behind: a later registrant must still be able to fire.
        assert!(!DISPATCHING.load(atomic::Ordering::Acquire), "a finished dispatch left the queue shut");
    }

    #[test]
    fn a_registrant_that_takes_the_queue_from_inside_a_callback_proves_the_lock_is_released() {
        static QUEUE: Mutex<Vec<PluginInitCallback>> = Mutex::new(Vec::new());
        static DISPATCHING: AtomicBool = AtomicBool::new(false);
        static FIRED: AtomicUsize = AtomicUsize::new(0);
        static NESTED_DISPATCH_CALLS: AtomicUsize = AtomicUsize::new(0);
        static NESTED_DISPATCHES_THAT_RAN: AtomicUsize = AtomicUsize::new(0);

        // Exactly what a plugin does when it calls `hachimi_register_on_game_initialized` from its
        // own `on_game_initialized` callback: it takes the queue lock. If the dispatch still held
        // that lock while it called the callback, this registration would block forever, so this
        // test finishing at all is the proof that callbacks run with the queue unlocked. It also
        // tries to start a second dispatch from inside the callback, which must decline.
        unsafe extern "C" fn re_registering_callback(userdata: *mut std::ffi::c_void) {
            FIRED.fetch_add(1, atomic::Ordering::Relaxed);
            store_plugin_init_callback(&QUEUE, (re_registering_callback as usize, userdata as usize));

            let nested = run_plugin_init_callbacks(&QUEUE, &DISPATCHING);
            NESTED_DISPATCH_CALLS.fetch_add(1, atomic::Ordering::Relaxed);
            if nested != 0 { NESTED_DISPATCHES_THAT_RAN.fetch_add(1, atomic::Ordering::Relaxed); }
        }

        store_plugin_init_callback(&QUEUE, (re_registering_callback as usize, 0));

        let fired = run_plugin_init_callbacks(&QUEUE, &DISPATCHING);

        // Each round took the registration the round before it queued, up to the ceiling, and the
        // dispatch stopped there instead of chasing a callback that registers forever.
        assert_eq!(fired, MAX_PLUGIN_INIT_DISPATCH_ROUNDS, "the re-entrant registrations were not dispatched");
        assert_eq!(FIRED.load(atomic::Ordering::Relaxed), MAX_PLUGIN_INIT_DISPATCH_ROUNDS, "a dispatched callback did not run");
        assert_eq!(NESTED_DISPATCH_CALLS.load(atomic::Ordering::Relaxed), MAX_PLUGIN_INIT_DISPATCH_ROUNDS, "a callback never tried to nest a dispatch");
        assert_eq!(NESTED_DISPATCHES_THAT_RAN.load(atomic::Ordering::Relaxed), 0, "a dispatch nested inside a dispatch ran");

        // What the last round queued is still in the queue for the next arrival rather than lost,
        // and the queue is open again afterwards.
        assert_eq!(QUEUE.lock().unwrap().len(), 1, "the bounded dispatch dropped a queued registrant");
        assert!(!DISPATCHING.load(atomic::Ordering::Acquire), "a finished dispatch left the queue shut");

        // A later arrival takes the registrant the ceiling stopped on, and the ceiling holds again
        // instead of the chase going on forever.
        assert_eq!(run_plugin_init_callbacks(&QUEUE, &DISPATCHING), MAX_PLUGIN_INIT_DISPATCH_ROUNDS,
            "the registrant the ceiling stopped on was lost");
        assert_eq!(QUEUE.lock().unwrap().len(), 1, "the second bounded dispatch lost the tail of the queue");
    }

    #[test]
    fn an_unresolved_registrant_stays_inert() {
        static QUEUE: Mutex<Vec<PluginInitCallback>> = Mutex::new(Vec::new());
        static DISPATCHING: AtomicBool = AtomicBool::new(false);
        static FIRED: AtomicUsize = AtomicUsize::new(0);

        unsafe extern "C" fn counted_callback(_: *mut std::ffi::c_void) {
            FIRED.fetch_add(1, atomic::Ordering::Relaxed);
        }

        store_plugin_init_callback(&QUEUE, (0, 0));
        store_plugin_init_callback(&QUEUE, (counted_callback as usize, 0));

        assert_eq!(run_plugin_init_callbacks(&QUEUE, &DISPATCHING), 1, "a zero callback address was called through");
        assert_eq!(FIRED.load(atomic::Ordering::Relaxed), 1, "the real callback beside it did not run");
        assert!(QUEUE.lock().unwrap().is_empty(), "an inert registrant stayed queued");
    }

    #[test]
    fn the_registration_window_decides_whether_a_registrant_is_fired_on_arrival() {
        static WINDOW_CLOSED: AtomicBool = AtomicBool::new(false);
        static DISPATCHING: AtomicBool = AtomicBool::new(false);

        // The plugin `init()` pass is still running: the registrant is queued for the dispatch that
        // closes the window, so a plugin's own callback never fires inside its own `init()`.
        assert!(!plugin_init_callback_fires_on_arrival(&WINDOW_CLOSED, &DISPATCHING),
            "a registrant was fired in the middle of the plugin init pass");

        // The pass has run, which is the case C16's latch stranded: the pass that would have taken
        // this registrant already happened, so the call that registered it fires it.
        WINDOW_CLOSED.store(true, atomic::Ordering::Release);
        assert!(plugin_init_callback_fires_on_arrival(&WINDOW_CLOSED, &DISPATCHING),
            "a registrant that arrived after the initialization pass was queued for a pass that already ran");

        // A dispatch is in progress further up the same call stack - a callback that registers -
        // and that dispatch drains what it fired, so this one is left to it instead of nesting.
        DISPATCHING.store(true, atomic::Ordering::Release);
        assert!(!plugin_init_callback_fires_on_arrival(&WINDOW_CLOSED, &DISPATCHING),
            "a dispatch nested inside a dispatch was started");
    }

    #[test]
    fn a_poisoned_queue_still_stores_and_dispatches_instead_of_panicking() {
        static DISPATCHING: AtomicBool = AtomicBool::new(false);
        static FIRED: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn counted_callback(_: *mut std::ffi::c_void) {
            FIRED.fetch_add(1, atomic::Ordering::Relaxed);
        }

        let queue: Mutex<Vec<PluginInitCallback>> = Mutex::new(Vec::new());

        // A plugin that faults while the queue is held poisons it. The exported registration entry
        // and the dispatch both reach this queue from an `extern "C"` boundary, so neither may turn
        // a later call into a panic back into third-party code (AGENTS section 6).
        let poisoned = std::thread::scope(|s| {
            let faulted = s.spawn(|| {
                let _held = queue.lock().unwrap();
                panic!("a plugin faulted while the queue was held");
            });
            faulted.join().is_err()
        });

        assert!(poisoned && queue.is_poisoned(), "the queue was not poisoned by the fault");

        store_plugin_init_callback(&queue, (counted_callback as usize, 0));
        assert_eq!(run_plugin_init_callbacks(&queue, &DISPATCHING), 1, "a poisoned queue stopped the plugin API working");
        assert_eq!(FIRED.load(atomic::Ordering::Relaxed), 1, "the callback recovered from a poisoned queue was not called");
    }

    // C2's leftovers: the two native sqlite detours. A test process has no game sqlite to call
    // (AGENTS section 4), so these drive the boundary itself - the `extern "C" fn` a MinHook
    // trampoline lands on - with the original published, unpublished, or pointed at a stand-in the
    // test wrote, and on key cells of their own rather than the process-wide `RETRIEVED_RAW_KEY`
    // `il2cpp::sql::tests` owns. No test here ever puts a key into the shared lock.
    //
    // They do touch the two slots those detours read and the one-shot unlock flag, and the harness
    // runs tests on several threads, so they take one turn between them: a stand-in published by one
    // test must never be what another test is asserting is absent.
    static SQLITE_TURN: Mutex<()> = Mutex::new(());

    /// `Interceptor::hook` is create-and-arm (`windows/interceptor_impl.rs`), so a call can reach a
    /// detour before this file has stored the address it forwards to. That window is where the old
    /// body ran `ORIG_SQLITE3_*.unwrap()` on a `None`.
    #[test]
    fn a_native_sqlite_detour_with_no_original_published_stays_inert() {
        let _turn = SQLITE_TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ORIG_SQLITE3_OPEN_V2.store(0, atomic::Ordering::Relaxed);
        ORIG_SQLITE3_KEY.store(0, atomic::Ordering::Relaxed);

        let key_hook: extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_void, i32) -> i32 = sqlite3_key_hook;
        assert_eq!(key_hook(std::ptr::null_mut(), std::ptr::null(), 0), SQLITE_ERROR,
            "a sqlite3_key call with no published original was forwarded through 0 instead of refused");

        let mut db: *mut std::ffi::c_void = std::ptr::null_mut();
        let open_hook: extern "C" fn(*const i8, *mut *mut std::ffi::c_void, i32, *const i8) -> i32 = sqlite3_open_v2_hook;
        assert_eq!(open_hook(std::ptr::null(), &mut db as *mut *mut std::ffi::c_void, 0, std::ptr::null()), SQLITE_ERROR,
            "an open nobody performed reported SQLITE_OK - the mod inventing a database");
    }

    #[test]
    fn a_published_original_is_what_the_sqlite_detours_forward_to() {
        let _turn = SQLITE_TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        static FORWARDED: AtomicUsize = AtomicUsize::new(0);

        extern "C" fn fake_sqlite3_key(_db: *mut std::ffi::c_void, _p_key: *const std::ffi::c_void, n_key: i32) -> i32 {
            FORWARDED.fetch_add(n_key as usize, atomic::Ordering::Relaxed);
            0
        }

        ORIG_SQLITE3_KEY.store(fake_sqlite3_key as *const () as usize, atomic::Ordering::Release);

        // `p_key` is null, so nothing is captured: this test proves the forwarding half and leaves the
        // shared key lock exactly as it found it.
        let key_hook: extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_void, i32) -> i32 = sqlite3_key_hook;
        assert_eq!(key_hook(std::ptr::null_mut(), std::ptr::null(), 3), 0,
            "the detour did not hand the game's sqlite3_key call to its original");
        assert_eq!(FORWARDED.load(atomic::Ordering::Relaxed), 3, "the original was called with something other than the game's own arguments");

        ORIG_SQLITE3_KEY.store(0, atomic::Ordering::Relaxed);
    }

    #[test]
    fn the_key_capture_keeps_the_first_key_and_refuses_a_length_it_cannot_copy() {
        static KEYS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        // A latch of its own, seeded with this test's own address so it is never a candidate for the
        // linker folding the shipped latches avoid; its value is only ever compared against null.
        static RECORDED: AtomicPtr<()> =
            AtomicPtr::new(the_key_capture_keeps_the_first_key_and_refuses_a_length_it_cannot_copy as *mut ());

        let first: [u8; 4] = [b'a', b'b', b'c', b'd'];
        let second: [u8; 3] = [b'x', b'y', b'z'];

        // `n_key` is a length the game supplied. Both of these are refused before anything is copied:
        // one is an out-of-bounds read, one is not a length at all, and a zero has nothing to copy.
        record_key_into(&KEYS, &RECORDED, first.as_ptr() as *const std::ffi::c_void, i32::MAX);
        record_key_into(&KEYS, &RECORDED, first.as_ptr() as *const std::ffi::c_void, -8);
        record_key_into(&KEYS, &RECORDED, std::ptr::null(), 0);
        assert!(KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty(),
            "a key length this mod cannot copy was written into the record");
        assert!(!RECORDED.load(atomic::Ordering::Acquire).is_null(),
            "a refused capture spent the latch, so a key arriving after it could never be recorded");

        record_key_into(&KEYS, &RECORDED, first.as_ptr() as *const std::ffi::c_void, first.len() as i32);
        assert_eq!(*KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), first.to_vec(),
            "the key the game handed over first is not the key recorded");

        // Once a key is in hand the record is settled, and a settled capture reads nothing, copies
        // nothing and takes no lock: not for the game's next key, and not for a pointer that is not a
        // key at all. That last one is the C9 shape arriving after the record is closed.
        record_key_into(&KEYS, &RECORDED, second.as_ptr() as *const std::ffi::c_void, second.len() as i32);
        record_key_into(&KEYS, &RECORDED, 0x1000usize as *const std::ffi::c_void, 64);
        assert_eq!(*KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), first.to_vec(),
            "a later key replaced the one already in hand");
        assert!(RECORDED.load(atomic::Ordering::Acquire).is_null(), "a settled record never spent its latch");

        // The cap is what the record is bounded by, and the key the open detour applies is a length
        // read back out of it.
        assert!(MAX_SQLITE_KEY_BYTES >= 64, "the key cap is smaller than a sqlite key this game can pass");
    }

    #[test]
    fn a_poisoned_key_lock_is_recovered_at_the_sqlite_boundaries() {
        static KEYS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        static RECORDED: AtomicPtr<()> =
            AtomicPtr::new(a_poisoned_key_lock_is_recovered_at_the_sqlite_boundaries as *mut ());

        let poisoned = std::thread::scope(|s| {
            let faulted = s.spawn(|| {
                let _held = KEYS.lock().unwrap();
                panic!("a thread died holding the shared key lock");
            });
            faulted.join().is_err()
        });

        assert!(poisoned && KEYS.is_poisoned(), "the key lock was not poisoned by the fault");

        // The old shape made every later sqlite call the game made panic across the boundary. The
        // recovery keeps the record working, which is what `il2cpp::sql::key_retrieved` reads.
        let key: [u8; 6] = [1, 2, 3, 4, 5, 6];
        record_key_into(&KEYS, &RECORDED, key.as_ptr() as *const std::ffi::c_void, key.len() as i32);
        assert_eq!(*KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), key.to_vec(),
            "a poisoned key lock stopped the capture instead of recovering");
    }

    #[test]
    fn the_open_detour_holds_the_key_lock_across_no_call_into_sqlite() {
        let _turn = SQLITE_TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        static KEYS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        static KEY_CALLS: AtomicUsize = AtomicUsize::new(0);
        static LOCK_STILL_HELD: AtomicUsize = AtomicUsize::new(0);

        // The stand-in for the other end of the trampoline. It answers the one question the lock order
        // raises: is the key lock free here? `std::Mutex` is not reentrant, so the honest version of
        // this check would be a deadlock; `try_lock` reports the same fact without hanging the suite.
        extern "C" fn fake_sqlite3_key(_db: *mut std::ffi::c_void, _p_key: *const std::ffi::c_void, _n_key: i32) -> i32 {
            KEY_CALLS.fetch_add(1, atomic::Ordering::Relaxed);

            match KEYS.try_lock() {
                Ok(_free) => {}
                Err(std::sync::TryLockError::WouldBlock) => {
                    LOCK_STILL_HELD.fetch_add(1, atomic::Ordering::Relaxed);
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {}
            }

            0
        }

        let key: [u8; 5] = [9, 8, 7, 6, 5];
        *KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = key.to_vec();
        crate::il2cpp::sql::AUTO_UNLOCK_NEXT_DB.store(true, atomic::Ordering::Relaxed);
        ORIG_SQLITE3_KEY.store(fake_sqlite3_key as *const () as usize, atomic::Ordering::Release);

        // A handle this test invented. The stand-in never dereferences it, and `apply_retrieved_key`
        // only reads the slot it is handed.
        let mut pp_db: *mut std::ffi::c_void = 0x1234usize as *mut std::ffi::c_void;
        apply_retrieved_key(&KEYS, &mut pp_db as *mut *mut std::ffi::c_void);

        assert_eq!(KEY_CALLS.load(atomic::Ordering::Relaxed), 1, "the retrieved key was not applied to the database this open produced");
        assert_eq!(LOCK_STILL_HELD.load(atomic::Ordering::Relaxed), 0,
            "the shared key lock was still held across the call into sqlite3_key");
        assert!(!crate::il2cpp::sql::AUTO_UNLOCK_NEXT_DB.load(atomic::Ordering::Relaxed),
            "the one-shot unlock flag outlived the open it was armed for");

        // And with no original published, the same call applies nothing rather than calling 0.
        ORIG_SQLITE3_KEY.store(0, atomic::Ordering::Relaxed);
        *KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = key.to_vec();
        crate::il2cpp::sql::AUTO_UNLOCK_NEXT_DB.store(true, atomic::Ordering::Relaxed);
        apply_retrieved_key(&KEYS, &mut pp_db as *mut *mut std::ffi::c_void);
        assert_eq!(KEY_CALLS.load(atomic::Ordering::Relaxed), 1, "a call through an unpublished original reached sqlite");

        crate::il2cpp::sql::AUTO_UNLOCK_NEXT_DB.store(false, atomic::Ordering::Relaxed);
    }

    /// The capture reads a pointer the game handed it (the C9 shape) standing on a boundary that
    /// cannot unwind. On a target with no SEH frame the barrier cannot see a fault, so this is the
    /// Windows half of the claim (AGENTS section 4).
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    #[test]
    fn a_key_pointer_that_is_not_a_key_is_taken_by_the_barrier() {
        static KEYS: Mutex<Vec<u8>> = Mutex::new(Vec::new());
        static RECORDED: AtomicPtr<()> =
            AtomicPtr::new(a_key_pointer_that_is_not_a_key_is_taken_by_the_barrier as *mut ());

        let _turn = guard::barrier_turn();
        let faults_before = guard::fault_trip_count();
        let panics_before = guard::panic_trip_count();

        // Inside the length clamp, and straight at an unmapped page.
        let bogus = 0x1000usize as *const std::ffi::c_void;
        record_key_into(&KEYS, &RECORDED, bogus, 64);

        assert!(KEYS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty(),
            "a capture that faulted wrote a key anyway");
        assert!(!RECORDED.load(atomic::Ordering::Acquire).is_null(),
            "a capture the barrier stopped closed the record, so no later key could ever be taken");
        assert_eq!(guard::fault_trip_count(), faults_before + 1, "the fault at the key capture was not taken by the barrier");
        assert_eq!(guard::last_fault_code(), 0xC0000005, "what the barrier stopped was not an access violation");
        assert_eq!(guard::panic_trip_count(), panics_before, "the capture was stopped as a panic, not a fault");

        // The shipped detour, through the pointer shape a trampoline uses: the same fault, answered as
        // no key captured and a refused call, and the process is still here to assert it.
        let _sqlite_turn = SQLITE_TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        ORIG_SQLITE3_KEY.store(0, atomic::Ordering::Relaxed);
        let key_hook: extern "C" fn(*mut std::ffi::c_void, *const std::ffi::c_void, i32) -> i32 = sqlite3_key_hook;
        assert_eq!(key_hook(std::ptr::null_mut(), bogus, 64), SQLITE_ERROR,
            "the key detour answered a fault at its own boundary with something other than a refusal");
        assert_eq!(guard::fault_trip_count(), faults_before + 2, "the fault at the shipped detour was not taken by the barrier");
    }

    /// The take-down counts have two doors now, the detach branch and the process exit call, and a
    /// session may only say them once. Only the doors claim them in shipped code, so this test owns
    /// the latch for the whole test process.
    #[test]
    fn the_take_down_counts_belong_to_the_first_door_that_reaches_them() {
        assert!(claim_take_down_report(), "no door had reached the counts, so the first one owns them");
        assert!(!claim_take_down_report(), "the second door has nothing left to add");
        assert!(!claim_take_down_report(), "and a third says nothing either");
    }
}
