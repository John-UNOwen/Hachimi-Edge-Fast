use std::{
    collections::hash_map,
    os::raw::c_void,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
    sync::{Mutex, MutexGuard},
};

use fnv::FnvHashMap;

use crate::interceptor_impl;

use super::Error;

// How many times the hook set has changed since the process started.
//
// `get_orig_fn!` used to ask the map below for the answer on every call the game made through an
// armed hook (C33): a lock, a hash, and an `unwrap()` waiting inside an `extern "C"` frame, on a
// path several hooks walk once per frame. What a detour needs was decided when its hook was
// created, and it only changes when a hook is created or taken away - so the detour keeps the
// address (see `CachedTrampoline`) and this counter is what keeps that copy honest: a wrapper
// compares it with the stamp on its own copy, and only a hook set that moved since then sends the
// wrapper back to the registry.
//
// It starts at 1 because 0 is the stamp a copy that has never read anything carries, and that must
// never look current. A bump is one store on the install/removal side; reads never leave a cache
// line. Installs happen while hooking, removals a handful of times per session, and a copy that
// finds its stamp behind pays one registry read - not one per call after that.
static INSTALL_GENERATION: AtomicUsize = AtomicUsize::new(1);

fn invalidate_trampoline_caches() {
    INSTALL_GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// Every registry method takes its lock through here. The registry is read from inside detours,
/// and a lock an earlier panic poisoned must not turn every later hooked call into a panic across
/// FFI (C2). `into_inner` hands the map back, which is the honest reading of it: the entries it
/// holds were created by the backend and are real whether or not the thread that panicked while
/// holding the lock left them consistent.
fn shared_lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Default)]
pub struct Interceptor {
    hook_map: Mutex<FnvHashMap<usize, HookHandle>>,
    // Targets created while arming was batched, armed together by finish_batch.
    queued: Mutex<Vec<usize>>,

    // Barrier item 2 (C2): the take-downs whose backend half waits for a point that is not a frame of
    // the hook being taken down - the coroutine doors, and only them.
    //
    // `guard::coroutine_trip` answers a coroutine door that tripped before the game answered by asking
    // for that door to come down, and it does so from inside the door's own `extern "C"` frame - the
    // frame the game reached through the very method being unhooked, mid-call on the game's `MoveNext`
    // out of the door's own trampoline. The registry half of that is ours to take there. The backend
    // half is `MH_DisableHook` plus `MH_RemoveHook` on the game's method: it writes the method's bytes
    // back and lets the trampoline go. Both are wanted there and neither can run there: the arm is
    // still going to call through that trampoline (`coroutine answer -> bool [bail { ... }]` publishes
    // the game's answer behind the barrier), and the bytes belong to a method this very frame is
    // standing in. So the backend half waits in this queue, and `drain_deferred_unhooks` runs it on the
    // game tick, which is where this fork already does its writing (`GameSystem::GameSystem_Update`).
    //
    // The other shape of take-down is not queued, because queueing it does something worse than the
    // hazard it avoids: it leaves the game's target armed - still routing into the mod's wrapper -
    // while this map no longer holds the wrapper, and the wrappers it covers ask `get_trampoline_addr`
    // for their original from inside their own detour (`windows::hook.rs`'s LoadLibraryW,
    // `android::hook.rs`'s dlopen pair and RegisterNatives, `windows::wnd_hook.rs`'s SetWindowLongPtr
    // pair, and a plugin doing the same through `interceptor_get_trampoline_addr`). For them 0 is a
    // call through 0 (C1), and nothing is armed to catch it: none of those wrappers is a
    // `def_detour!` wrapper, so no barrier stands on them. `release_waits_for_a_safe_point` is the
    // decision, and `il2cpp::symbols::is_coroutine_door_target` is the fact it rests on.
    deferred: Mutex<Vec<DeferredUnhook>>,
    // How many entries `deferred` holds. It is a hint to skip the lock: a relaxed load here may lag a
    // take-down not yet queued on another thread by a frame, and the queue itself is ordered by its
    // mutex. The frame path pays this one load and nothing else (AGENTS section 6).
    //
    // The relationship the readers rely on, established at the only writer: the count goes up under
    // the queue's own lock *before* the entry is pushed (`defer_backend_unhook`), and every
    // subtraction is for entries taken out under that same lock. So the hint may lag a take-down no
    // one has queued - the benign direction named above - but it never reads below what the queue
    // holds, never wraps under a `fetch_sub`, and never reads 0 while the mutex-backed queue holds
    // an entry. Both bad directions are load-bearing: a wrapped hint makes every later tick pay the
    // lock the relaxed load exists to skip, and a 0 read over a held entry makes `unhook_all`'s
    // drain miss a take-down whose registry half is already gone.
    deferred_waiting: AtomicUsize,
    // Door take-downs put on the queue since the process started. Monotonic, and the counter the
    // first-N line naming a queued take-down latches on.
    takedowns_deferred: AtomicUsize,
    // Take-downs that ran whole in the frame that asked for them, because nothing about them needs a
    // safe point. Monotonic, and the counter the other first-N line latches on.
    takedowns_completed_now: AtomicUsize,
    // Queue entries that reached the backend. The registry half is proven by unit tests; this is the
    // half only a game run reads (AGENTS section 4), so a run can tell "queued" from "released".
    backends_released: AtomicUsize,

    batching: AtomicBool
}

/// One waiting take-down: the handle the backend half runs on, and the wrapper address the game reached
/// it through. The wrapper address is kept because it is the key the registry used, and it is what
/// `reread_trampoline` matches a still-armed detour against while its removal waits.
struct DeferredUnhook {
    wrapper: usize,
    handle: HookHandle,
}

/// How many hook take-downs name themselves in the log, and the rule `announce_takedown` applies.
const TAKEDOWN_LOG_LIMIT: usize = 8;

/// Which take-down's backend half has to wait for a point that is not a frame of the hook it takes
/// down, and it is one: the coroutine door.
///
/// The door is the only take-down this fork makes from inside the target's own live call - `guard::
/// coroutine_trip` runs in the door's `extern "C"` frame while that frame is standing in the game's
/// `MoveNext` out of the door's own trampoline, and the arm may call through that trampoline again
/// after the trip - and the game-thread tick is a point at which that frame has certainly returned
/// (Unity drives a live coroutine on the thread that runs `GameSystem.Update`, and this fork already
/// writes its game values there).
///
/// Every other take-down runs where it was asked for. For a vtable hook that is not merely allowed,
/// it is the only sensible reading: its backend half is a word written back into the vtable slot with
/// the pointer the slot carried before the hook, so no code is patched and nothing is freed, and a
/// thread that is inside the wrapper at that moment is executing this module, not the slot. The
/// native detours (kernel32's LoadLibraryW, user32's SetWindowLongPtr pair, libc/linker's dlopen pair,
/// libart's RegisterNatives, a plugin's own hook) are function hooks, and their target is a function
/// any thread may be calling; no point this process owns, the tick included, can promise that such a
/// function is quiet, so waiting buys nothing while an armed target with no registry entry is a live
/// hazard. What the residual race at those sites is, and what splitting the restore from the free
/// would need, is recorded in the C2 concept.
///
/// Cold: a take-down is a handful per session, never a per-call path.
fn release_waits_for_a_safe_point(hook: &HookHandle) -> bool {
    matches!(hook.hook_type, HookType::Function)
        && crate::il2cpp::symbols::is_coroutine_door_target(hook.orig_addr)
}

/// The first-N latch behind the take-down log: take-downs `1..=TAKEDOWN_LOG_LIMIT` each get a line at
/// the moment they happen, `TAKEDOWN_LOG_LIMIT + 1` gets the line that says the rest are only counted,
/// and nothing after that says anything. Pure, so the pattern is testable the way the probes' counters
/// are; the barrier's `invented_answer` latch is the same shape on an atomic instead of a counter.
fn announce_takedown(n: usize) -> bool {
    n <= TAKEDOWN_LOG_LIMIT || n == TAKEDOWN_LOG_LIMIT + 1
}

#[derive(Clone, Copy)]
pub struct HookHandle {
    pub orig_addr: usize,
    pub trampoline_addr: usize,
    pub hook_type: HookType
}

impl HookHandle {
    unsafe fn unhook(&self) -> Result<(), Error> {
        match self.hook_type {
            HookType::Function => interceptor_impl::unhook(self),
            HookType::Vtable => interceptor_impl::unhook_vtable(self)
        }
    }
}

#[derive(Clone, Copy)]
pub enum HookType {
    Function,
    Vtable
}

/// One detour's own copy of the trampoline address it was created with (C33).
///
/// A detour is reached through its own trampoline and nothing else, so the address it jumps to is
/// fixed for as long as its hook exists. Keeping it here turns the walk to the original method -
/// the hottest path in the mod, paid on every hooked call - into three atomic loads and two
/// compares: no mutex, no hash, no `unwrap()`, and no `Hachimi::instance()`, which clones an `Arc`
/// on the way to the map.
///
/// `hook` is the wrapper's own address, written into the copy at compile time by `trampoline_cache!`
/// and checked on every read. It is what stops one copy from ever answering for two hooks. Two
/// copies made for two different hooks then start with different bytes, so a linker folding
/// identical data has nothing to fold - and if storage were shared anyway the copy refuses to answer
/// for a hook it was not made for and reads the registry instead. That is not a hypothetical: an
/// early build of this file folded two identically initialised copies into one address, and a detour
/// handed another hook's trampoline jumps to the wrong method.
///
/// The copy is only trusted while `stamp` matches `INSTALL_GENERATION`, so an `unhook` - or a
/// re-hook, the shape `windows/gui_impl/render_hook.rs` runs when the overlay is rebuilt - sends the
/// wrapper back to the registry instead of letting it jump into a trampoline the backend has
/// released. While a take-down's backend half is still waiting in the queue the cold read answers the
/// trampoline that is *still armed* rather than 0 (`reread_trampoline`), and the drain that releases it
/// bumps the generation again before its backend runs, so no copy outlives the memory it names.
///
/// `0` means "this hook is not installed", which is what `get_orig_fn!` documented before (C1): the
/// caller is handed address 0 rather than a call through a target the registry does not have. That
/// is only safe for a detour - see `resolve_or_none`, which is what every other call site has to ask.
pub struct CachedTrampoline {
    // The wrapper's own address, kept as a pointer because a function address cannot be cast to an
    // integer in a const context, and this field has to be filled in at compile time.
    hook: AtomicPtr<c_void>,
    // The address is written before the stamp, and the pairing a reader uses is the stamp: an
    // acquire load of a stamp that matches the registry's generation has therefore acquired the
    // address written before the release store of that stamp.
    addr: AtomicUsize,
    stamp: AtomicUsize,
    // Set the first time this copy answers 0 for a call site that asked not to be handed a bare
    // address (`resolve_or_none`). It rides on the copy rather than on a separate static because
    // the copy is already unique per call site - it is tagged with the wrapper it belongs to - so
    // this adds a byte to a cell the linker cannot fold across call sites (the trap above).
    unresolved_reported: AtomicBool
}

impl CachedTrampoline {
    pub const fn new(hook_addr: *mut c_void) -> Self {
        Self {
            hook: AtomicPtr::new(hook_addr),
            addr: AtomicUsize::new(0),
            stamp: AtomicUsize::new(0),
            unresolved_reported: AtomicBool::new(false)
        }
    }

    /// The hot side, and the whole reason for this type: `Some(address)` when this copy is current
    /// for `hook_addr`, `None` when it is not the copy for that hook or the hook set moved since it
    /// was read. `Some(0)` is current and means the hook is not installed.
    #[inline]
    pub fn cached(&self, hook_addr: *mut c_void) -> Option<usize> {
        if self.hook.load(Ordering::Relaxed) != hook_addr {
            return None;
        }

        if self.stamp.load(Ordering::Acquire) != INSTALL_GENERATION.load(Ordering::Acquire) {
            return None;
        }

        Some(self.addr.load(Ordering::Acquire))
    }

    /// The cold side: one registry read, stamped with the generation read *before* the lookup, so a
    /// hook that moved while the lookup was in flight leaves this copy behind its stamp and is read
    /// again rather than trusted.
    pub fn reread(&self, interceptor: &Interceptor, hook_addr: *mut c_void) -> usize {
        let generation = INSTALL_GENERATION.load(Ordering::Acquire);
        let addr = interceptor.reread_trampoline(hook_addr as usize);

        self.hook.store(hook_addr, Ordering::Relaxed);
        self.addr.store(addr, Ordering::Relaxed);
        self.stamp.store(generation, Ordering::Release);

        addr
    }

    /// What `get_orig_fn!` hands a detour: the copy, or one registry read when the copy is behind
    /// the hook set. Only the cold branch touches the singleton, and only a detour - which cannot
    /// run before `Hachimi::init` - reaches this at all.
    #[inline]
    pub fn resolve(&self, hook_addr: *mut c_void) -> usize {
        if let Some(addr) = self.cached(hook_addr) {
            return addr;
        }

        let hachimi = crate::core::Hachimi::instance();
        self.reread(&hachimi.interceptor, hook_addr)
    }

    /// The answer for a call site that is **not** the body of the detour it names (C1).
    ///
    /// `resolve` may answer 0, and 0 is harmless only to a detour: a detour is reached through its
    /// own trampoline, so while its hook is in the registry the answer is that trampoline, and once
    /// the hook is gone nothing routes into the code that would jump through the answer. A helper
    /// an ordinary feature calls, or a second hook's body, has no such guarantee. It reaches the
    /// answer on a GUI action, a translation pass, the SQL reader, the free camera, or a window
    /// procedure, and a call through 0 there is an access violation on a path no detour barrier is
    /// standing on. The C1 audit found 11 such call sites, plus 12 wrappers that Rust code calls
    /// directly and so cannot rely on being reachable only through their own trampoline.
    ///
    /// `Some(address)` is a trampoline to call. `None` means this hook is not installed and the
    /// caller must stay inert - which is the same answer `def_method_wrapper_fn!` and
    /// `impl_addr_wrapper_fn!` already give their callers.
    #[inline]
    pub fn resolve_or_none(&self, hook_addr: *mut c_void, name: &str) -> Option<usize> {
        let addr = self.resolve(hook_addr);

        if addr != 0 {
            return Some(addr);
        }

        if self.claim_unresolved_report() {
            warn!("{name}: no trampoline, the hook is not installed and the call is skipped");
        }

        None
    }

    /// True the first time this copy answers "no original to call", false every time after. The
    /// one place the reason for staying inert is ever said, so a wrapper whose target never
    /// resolved costs one swap on its first call and one load after that, never a log line
    /// (AGENTS section 6: nothing on a hot path formats or logs per call).
    #[cold]
    pub fn claim_unresolved_report(&self) -> bool {
        !self.unresolved_reported.swap(true, Ordering::AcqRel)
    }

    /// The read side of the claim above: has this copy already said why it stays inert?
    #[inline]
    pub fn unresolved_reported(&self) -> bool {
        self.unresolved_reported.load(Ordering::Relaxed)
    }
}

impl Interceptor {
    pub fn hook(&self, orig_addr: usize, hook_addr: usize) -> Result<usize, Error> {
        // A target this queue is still holding comes back before anything is created on it: the backend
        // refuses a second create on a target it still holds, so a door re-armed by the next coroutine
        // that needs it would fail. This is not the faulting frame of the hook being released -
        // installs come from `init` and from the wrappers that hand a coroutine over, never from the
        // body of the door they arm.
        self.take_down_deferred_for_target(orig_addr);

        let batched = self.batching.load(Ordering::Acquire);

        match shared_lock(&self.hook_map).entry(hook_addr) {
            hash_map::Entry::Occupied(e) => Ok(e.get().trampoline_addr),
            hash_map::Entry::Vacant(e) => {
                let trampoline_addr = unsafe {
                    if batched {
                        interceptor_impl::create_hook(orig_addr, hook_addr)?
                    }
                    else {
                        interceptor_impl::hook(orig_addr, hook_addr)?
                    }
                };

                if batched {
                    shared_lock(&self.queued).push(orig_addr);
                }

                e.insert(
                    HookHandle {
                        orig_addr,
                        trampoline_addr,
                        hook_type: HookType::Function
                    }
                );

                // A copy cached an address for this wrapper before the hook existed (an `unhook` and
                // a re-hook of the same wrapper), or caches one on its first call after this.
                // Neither may keep the old answer.
                invalidate_trampoline_caches();

                Ok(trampoline_addr)
            },
        }
    }

    // Arm everything created between this and finish_batch in one backend call instead of
    // one per hook. Only safe where nothing calls through a trampoline in the meantime: the
    // trampoline is built to step over the bytes the detour occupies, and those bytes are
    // only written when the hook is armed. `il2cpp::hook::init` is the one place that
    // qualifies, because its macros install and never call.
    pub fn begin_batch(&self) {
        self.batching.store(true, Ordering::Release);
    }

    // Returns how many hooks were armed. A failed batch arms each target on its own rather
    // than leaving the build unarmed.
    pub fn finish_batch(&self) -> usize {
        self.batching.store(false, Ordering::Release);

        let queued = std::mem::take(&mut *shared_lock(&self.queued));
        if queued.is_empty() {
            return 0;
        }

        if let Err(e) = unsafe { interceptor_impl::enable_all_hooks() } {
            error!("Batch arming failed: {e}, arming {} hooks one by one", queued.len());

            for orig_addr in &queued {
                if let Err(e) = unsafe { interceptor_impl::enable_hook(*orig_addr) } {
                    error!("Failed to arm hook {orig_addr:#016x}: {e}");
                }
            }
        }

        queued.len()
    }

    pub fn hook_vtable(&self, vtable: *mut usize, vtable_index: usize, hook_addr: usize) -> Result<usize, Error> {
        // Same rule as `hook`: a vtable entry whose take-down is waiting is put back before a new
        // entry is written over it, or the waiting removal would restore a vtable slot nobody owns any
        // more. `hook_vtable` keys its handle's `orig_addr` on the vtable slot, so the match below is on
        // the same number the removal writes back.
        self.take_down_deferred_for_target(unsafe { vtable.add(vtable_index) as usize });

        match shared_lock(&self.hook_map).entry(hook_addr) {
            hash_map::Entry::Occupied(e) => Ok(e.get().trampoline_addr),
            hash_map::Entry::Vacant(e) => {
                let hook_handle = unsafe { interceptor_impl::hook_vtable(vtable, vtable_index, hook_addr)? };
                let trampoline_addr = hook_handle.trampoline_addr;
                e.insert(hook_handle);
                invalidate_trampoline_caches();
                Ok(trampoline_addr)
            }
        }
    }

    /// The registry's answer: `0` when this map holds no entry for that wrapper.
    ///
    /// That is the answer a call site that is not the hook's own detour must stay inert on (C1). A hook
    /// the registry has lost is not necessarily taken down yet - see `reread_trampoline`, which is what
    /// the copy a detour was built with falls back to while the backend half of that take-down waits.
    pub fn get_trampoline_addr(&self, hook_addr: usize) -> usize {
        if let Some(hook) = shared_lock(&self.hook_map).get(&hook_addr) {
            hook.trampoline_addr
        }
        else {
            warn!("Attempted to get invalid hook: {}", hook_addr);
            0
        }
    }

    /// The cold read a `CachedTrampoline` falls back to when its stamp is behind the hook set.
    ///
    /// The registry first. Then the take-down queue, because a hook the registry already lost is *still
    /// armed* until its backend half runs, and answering the 0 `get_trampoline_addr` gives is the exact
    /// chain C1 exists to close: `get_orig_fn!` answers 0, the body calls through it, the barrier takes
    /// an access violation on a coroutine the game is still driving. While the removal waits there is a
    /// trampoline to hand over, it belongs to this wrapper, and calling it is what calling the original
    /// means. Nothing here keeps an answer alive past the release: `drain_deferred_unhooks` and
    /// `take_down_deferred_for_target` invalidate every copy *before* their backend halves run, so a
    /// copy refreshed during the window goes back to the registry, where the entry is gone and the
    /// queue entry is taken, and is handed 0 - never the trampoline the backend is about to let go.
    pub fn reread_trampoline(&self, hook_addr: usize) -> usize {
        let live = shared_lock(&self.hook_map).get(&hook_addr).map(|hook| hook.trampoline_addr);

        if let Some(addr) = live {
            return addr;
        }

        let waiting = {
            let deferred = shared_lock(&self.deferred);
            deferred.iter().find(|entry| entry.wrapper == hook_addr)
                .map(|entry| entry.handle.trampoline_addr)
        };

        if let Some(addr) = waiting {
            return addr;
        }

        warn!("Attempted to get invalid hook: {}", hook_addr);
        0
    }

    /// Take a hook down.
    ///
    /// The registry half happens here, in the frame that asked, and the handle is handed back with it.
    /// The backend half - the bytes and the trampoline - goes one of two ways, decided by
    /// `release_waits_for_a_safe_point`, and the way matters to the caller:
    ///
    /// - A coroutine door's backend half waits in `deferred` until `drain_deferred_unhooks` runs it on
    ///   the game tick. This is the shape barrier item 2 was written for: the take-down is asked for
    ///   from inside the detour of the method being taken down, mid-call out of that door's own
    ///   trampoline, and the arm still means to call through that trampoline after the trip. Until the
    ///   release the game method keeps its jump and the door keeps answering, so the handle this call
    ///   returns and `get_trampoline_addr`'s 0 for it describe a hook that is out of the registry but
    ///   not yet off the game's method. A re-install on that target (`hook`, `hook_vtable`), the tick,
    ///   and the detach (`unhook_all`) are the three points that run it.
    /// - Everything else is taken down before this call returns, which is what it meant before barrier
    ///   item 2 and what every caller other than the door rule - `plugin_api::interceptor_unhook`,
    ///   `render_hook`'s swap-chain pair, `wnd_hook::uninit`, the self-unhooking LoadLibraryW and
    ///   dlopen detours - is written against. Deferring those would leave a target armed with no
    ///   registry entry to answer it, and their wrappers read `get_trampoline_addr` from inside their
    ///   own detour: 0 there is a call through 0 (C1) with no barrier standing on it. A vtable
    ///   take-down is one aligned word written back into the slot with the pointer the slot itself
    ///   carried - it patches no code and frees nothing - so it has no safe point to wait for, and the
    ///   tick is not the swap chain's thread's safe point anyway (`render_hook`'s Present wrapper posts
    ///   its own work to `Thread::main_thread().schedule`).
    pub fn unhook(&self, hook_addr: usize) -> Option<HookHandle> {
        let hook = shared_lock(&self.hook_map).remove(&hook_addr)?;

        // Out of the registry, and behind its stamp, first: a copy that had not been read yet is sent
        // back to the registry, where this entry is gone.
        invalidate_trampoline_caches();

        if release_waits_for_a_safe_point(&hook) {
            self.defer_backend_unhook(hook_addr, hook);
        }
        else {
            self.complete_backend_unhook_now(hook_addr, &hook);
        }

        Some(hook)
    }

    /// Put one door take-down's backend half on the queue, and say it.
    ///
    /// Before this, a door that came down said nothing at the moment it came down: the only trace was a
    /// count on the detach path, and a door armed once in `init` and never re-armed (`CutStateProbe`'s
    /// `PlayTrainingCutStateMachine_MoveNext` is one) was simply gone with no line to mark when. The
    /// first-N pattern is what names it: `TAKEDOWN_LOG_LIMIT` take-downs each get a line at the moment
    /// they happen, one more says the rest are only counted, and a trip that repeats after the queueing
    /// says nothing at all - it never reaches here, because the registry no longer has the entry.
    ///
    /// The line says what is waiting and where it is released, because that is the half a run has to be
    /// able to read: the game method still has its jump until the tick runs it.
    #[cold]
    fn defer_backend_unhook(&self, wrapper: usize, handle: HookHandle) {
        let n = self.takedowns_deferred.fetch_add(1, Ordering::AcqRel) + 1;

        // Counted before it is visible. The hint goes up under the queue's own lock, ahead of the
        // push; both subtractors can reach the entry - and `fetch_sub` for it - only under that same
        // lock, so neither can ever subtract for an entry the hint has not counted. That keeps the
        // hint at or above what the queue holds: no wrapped `fetch_sub`, no drain fast path paying
        // the lock on a wrapped value, and no early-out over an entry the queue holds while its
        // registry half is already gone.
        {
            let mut deferred = shared_lock(&self.deferred);
            self.deferred_waiting.fetch_add(1, Ordering::AcqRel);
            deferred.push(DeferredUnhook { wrapper, handle });
        }

        if announce_takedown(n) {
            if n <= TAKEDOWN_LOG_LIMIT {
                warn!(
                    "Hook take-down {}: the coroutine door at {:#016x} is out of the registry now; its game method at {:#016x} keeps its jump, and its trampoline stays alive for this arm to call through, until drain_deferred_unhooks runs the take-down on the game tick",
                    n,
                    wrapper,
                    handle.orig_addr
                );
            }
            else {
                warn!(
                    "Hook take-downs after {} are queued the same way and released the same way; only the count is kept",
                    TAKEDOWN_LOG_LIMIT
                );
            }
        }
    }

    /// Run one take-down's backend half here, in the frame that asked for it, and say it once.
    ///
    /// This is the shape `unhook` had before barrier item 2, and the shape every caller that is not the
    /// door rule is written against: when this call returns the target is back to its own code and its
    /// own bytes, so the wrapper is never reached again with nothing in the registry to answer it. The
    /// first-N line is here so a run can see which shape a take-down took without a line per
    /// take-down; on the other side, `drain_deferred_unhooks` names the door take-downs it put back, and
    /// `take_down_deferred_for_target` - the re-install's forced release - is counted and logged only
    /// when the backend refuses it.
    #[cold]
    fn complete_backend_unhook_now(&self, wrapper: usize, handle: &HookHandle) {
        let n = self.takedowns_completed_now.fetch_add(1, Ordering::AcqRel) + 1;

        if announce_takedown(n) {
            if n <= TAKEDOWN_LOG_LIMIT {
                info!(
                    "Hook take-down {}: the detour at {:#016x} is taken down where it was asked for; {} at {:#016x} is not a coroutine door, so nothing about it waits for the game tick",
                    n,
                    wrapper,
                    match handle.hook_type {
                        HookType::Function => "the function",
                        HookType::Vtable => "the vtable slot"
                    },
                    handle.orig_addr
                );
            }
            else {
                info!(
                    "Hook take-downs after {} are completed the same way; only the count is kept",
                    TAKEDOWN_LOG_LIMIT
                );
            }
        }

        if let Err(e) = unsafe { handle.unhook() } {
            error!("Failed to unhook {}: {}", handle.orig_addr, e);
        }
    }

    /// Release the take-downs waiting on one target, now, and return how many.
    ///
    /// Only `hook` and `hook_vtable` ask, because a re-install on a target the backend still holds is
    /// the one thing that cannot wait for the game tick. The entries are split out under the lock and
    /// the backend halves run after it is dropped, the order `unhook_all` keeps: no backend call is made
    /// while a lock of this registry is held. And every copy is invalidated before its backend half
    /// runs, the order `unhook` keeps: no copy stays current for a trampoline this is about to free.
    fn take_down_deferred_for_target(&self, orig_addr: usize) -> usize {
        let run = {
            let mut deferred = shared_lock(&self.deferred);

            if deferred.is_empty() {
                return 0;
            }

            let mut keep: Vec<DeferredUnhook> = Vec::new();
            let mut run: Vec<DeferredUnhook> = Vec::new();

            for entry in std::mem::take(&mut *deferred) {
                if entry.handle.orig_addr == orig_addr {
                    run.push(entry);
                }
                else {
                    keep.push(entry);
                }
            }

            // Take the count down by what was taken out, rather than writing what this split saw: a
            // take-down queued on another thread while the split was running stays counted. And an
            // entry can only appear in `run` after it was counted under this very lock - the
            // increment is taken before the push - so this subtraction lands on 0 or above.
            self.deferred_waiting.fetch_sub(run.len(), Ordering::AcqRel);
            *deferred = keep;

            run
        };

        let count = run.len();

        // Before the backend, not after: a copy refreshed by `reread_trampoline` while this take-down
        // waited is stamped with the current generation and answers the trampoline about to be let go.
        // Sending every copy back first is what keeps the ordering promise `unhook` made when it queued
        // them - no copy is ever current for a trampoline the registry has already handed to the backend.
        if count > 0 {
            invalidate_trampoline_caches();
        }

        self.backends_released.fetch_add(count, Ordering::AcqRel);

        for entry in &run {
            if let Err(e) = unsafe { entry.handle.unhook() } {
                error!("Failed to unhook {}: {}", entry.handle.orig_addr, e);
            }
        }

        count
    }

    /// Run the backend halves of the door take-downs the queue is holding, and return how many.
    ///
    /// `GameSystem::GameSystem_Update` calls this once per game tick: a point that is a frame of the
    /// game's own `Update`, and not a frame of the door whose bytes go back here. Nothing else is ever in
    /// this queue - `release_waits_for_a_safe_point` puts only doors in it - which is what makes this a
    /// safe point for everything it releases. `unhook_all` calls it before it takes the map, because a hook
    /// left armed while this module is being unloaded is a jump into unmapped code.
    ///
    /// The queue is taken out before the backend runs, the way `finish_batch` takes the arming queue: an
    /// entry cannot be released twice, and a removal that re-enters the game method it restores does not
    /// find the same entry still waiting.
    pub fn drain_deferred_unhooks(&self) -> usize {
        // The per-frame ask, and the only thing the frame path pays while nothing is waiting: one
        // relaxed load, no lock (AGENTS section 6).
        if self.deferred_waiting.load(Ordering::Relaxed) == 0 {
            return 0;
        }

        let deferred = std::mem::take(&mut *shared_lock(&self.deferred));
        let count = deferred.len();

        // Behind every copy's stamp before the backend runs, the order `unhook` and `unhook_all` keep.
        // The registry half of each of these take-downs already bumped the generation, and
        // `reread_trampoline` answers an arm standing in that window from the queue, so a copy can be
        // stamped with the current generation and hold exactly the trampoline this loop lets go. This
        // is the one point in the process where a trampoline is freed while another thread may still
        // be asking for it - the queue exists precisely because other arms may be calling through that
        // door - so invalidating here, not after the backend, is what keeps a warmed copy from being
        // handed a freed trampoline for the whole time the backend runs. A copy sent back after this
        // bump re-reads: the registry entry is gone and the queue entry is taken, so it is handed 0.
        if count > 0 {
            invalidate_trampoline_caches();
        }

        // What this `mem::take` moved out was counted under this lock before it was pushed, so the
        // subtraction lands on 0 or above, and a hint that reads 0 after it describes a queue this
        // lock has just emptied.
        self.deferred_waiting.fetch_sub(count, Ordering::AcqRel);
        self.backends_released.fetch_add(count, Ordering::AcqRel);

        for entry in &deferred {
            if let Err(e) = unsafe { entry.handle.unhook() } {
                error!("Failed to unhook {}: {}", entry.handle.orig_addr, e);
            }
        }

        if count > 0 {
            // The other half of the pair a run reads: the line above says a take-down was asked for from
            // inside a frame that could not run it, this one says the game method was put back at a point
            // that could.
            info!("Hook take-down: {count} game method(s) restored on the game tick");
        }

        count
    }

    /// Test-only: how many take-downs are waiting, how many have reached the backend through a drain,
    /// and how many ran where they were asked for. The three numbers are the whole claim barrier item 2
    /// makes after its scope is fixed - a door's backend half is queued and released later, everything
    /// else is released now - so a test can read them apart.
    #[cfg(test)]
    pub fn deferred_unhook_count(&self) -> usize {
        self.deferred_waiting.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn backend_release_count(&self) -> usize {
        self.backends_released.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn completed_now_count(&self) -> usize {
        self.takedowns_completed_now.load(Ordering::Relaxed)
    }


    pub fn unhook_all(&self) {
        // Whatever is waiting goes first. A take-down the queue still holds is a game method whose jump
        // still points into this module, and the module is being unloaded: the next call on that method
        // would be a call into unmapped code. A detach is as close to a quiescent point as this process
        // has, which is why this one path runs the halves on the spot instead of queueing them.
        let waiting = self.drain_deferred_unhooks();

        if waiting > 0 {
            info!("Released {waiting} hook take-down(s) that were waiting for a game tick");
        }

        // The same order as `unhook`, for the whole map at once: `mem::take` moves it out (the shape
        // `finish_batch` uses for the queue) so no entry is still in the registry when its generation
        // goes stale, and every detour that reads after this point is handed 0.
        let hooks = std::mem::take(&mut *shared_lock(&self.hook_map));
        invalidate_trampoline_caches();

        for (_, hook) in hooks {
            if let Err(e) = unsafe { hook.unhook() } {
                error!("Failed to unhook {}: {}", hook.orig_addr, e);
            }
        }
    }

    pub fn get_vtable_from_instance(instance_addr: usize) -> *mut usize {
        unsafe { interceptor_impl::get_vtable_from_instance(instance_addr) }
    }

    pub fn find_symbol_by_name(module: &str, symbol: &str) -> Result<usize, Error> {
        unsafe { interceptor_impl::find_symbol_by_name(module, symbol) }
    }
}

/// The registry halves of `hook` and `unhook` on their own, for tests.
///
/// `pub(crate)` and not tucked into this module's `mod tests`, because the half of the hook boundary
/// that has to prove it takes a hook *down* is not here: `il2cpp/hook/mod.rs`'s barrier answers a
/// coroutine door that cannot answer by removing that door from this map, and its test drives the
/// shipped `unhook` against a registry it filled through `record_hook`. What that test can reach is the
/// registry half - which is precisely the half barrier item 2 says is proven - and the backend half
/// `unhook` now queues is asserted here, where the queue lives.
#[cfg(test)]
impl Interceptor {
    // The registry half of `hook`, without the backend call.
    pub(crate) fn record_hook(&self, hook_addr: usize, trampoline_addr: usize) {
        self.record_hook_at(hook_addr, hook_addr, trampoline_addr, HookType::Function);
    }

    /// The registry half of `hook` for a hook whose target is a different address from its wrapper,
    /// which is what every real hook is: the key is the wrapper the game reaches, `orig_addr` is the
    /// method or slot the backend was armed on. `release_waits_for_a_safe_point` reads `orig_addr`, so
    /// a test that wants to say "this take-down is a door" has to name the door's method, and a test
    /// that wants to say "this one is not" has to name a target nothing recorded.
    pub(crate) fn record_hook_at(&self, wrapper: usize, orig_addr: usize, trampoline_addr: usize, hook_type: HookType) {
        shared_lock(&self.hook_map).insert(
            wrapper,
            HookHandle { orig_addr, trampoline_addr, hook_type }
        );
        invalidate_trampoline_caches();
    }

    // The registry half of `unhook`, and the registry half only: it queues nothing, so a test that
    // drives a `CachedTrampoline` against it sees the strict "no entry, no trampoline" answer rather
    // than the one a take-down whose backend half has not run yet gives. The shipped `unhook`'s queue
    // is what `a_take_down_releases_the_registry_half_now_and_runs_the_backend_half_on_the_drain`
    // drives.
    pub(crate) fn drop_hook(&self, hook_addr: usize) -> Option<HookHandle> {
        let hook = shared_lock(&self.hook_map).remove(&hook_addr)?;
        invalidate_trampoline_caches();
        Some(hook)
    }
}

// The cache half of `get_orig_fn!`: one `CachedTrampoline` per expansion, tagged with the wrapper it
// belongs to, so no call site pays the registry for the address it was created with and no copy can
// answer for a hook other than its own.
macro_rules! trampoline_cache {
    ($hook:ident) => (
        {
            static CACHED: $crate::core::interceptor::CachedTrampoline
                = $crate::core::interceptor::CachedTrampoline::new($hook as *const () as *mut ::std::os::raw::c_void);

            &CACHED
        }
    )
}

/// The original method a detour hands its call to: `transmute` of whatever the registry answers
/// for this wrapper, which is 0 when the hook is not installed (C1).
///
/// Legal for the body of the detour it names, and for that alone. The registry holds an entry for
/// an installed hook, a detour is reached through its own trampoline and nothing else, and the
/// hook cannot be taken away from under a call already running on that trampoline. Anywhere else,
/// the 0 this expands into is a callable function pointer and the call is an access violation:
/// ask `get_orig_fn_guarded!` instead.
macro_rules! get_orig_fn {
    ($hook:ident, $type:tt) => (
        unsafe {
            ::std::mem::transmute::<usize, $type>(
                trampoline_cache!($hook).resolve($hook as *const () as *mut ::std::os::raw::c_void)
            )
        }
    )
}

/// `get_orig_fn!` for a call site that is not the body of its own detour (C1): an unresolved hook
/// yields `None`, so the caller returns its inert value instead of calling through address 0.
/// This is what `def_method_wrapper_fn!` and `impl_addr_wrapper_fn!` already do for their
/// callers, on the hook map instead of on an address `init` resolved.
///
/// ```ignore
/// let Some(orig) = get_orig_fn_guarded!(GetText, GetTextFn) else { return null_mut() };
/// orig(this, idx)
/// ```
///
/// Same cache as `get_orig_fn!`, so the guard reads a value the hot path already loaded, and the
/// reason for staying inert is said once per call site on the branch that is already skipping the
/// call - never on the way to a target that did resolve.
macro_rules! get_orig_fn_guarded {
    ($hook:ident, $type:tt) => (
        trampoline_cache!($hook).resolve_or_none(
            $hook as *const () as *mut ::std::os::raw::c_void,
            stringify!($hook)
        ).map(|addr| unsafe { ::std::mem::transmute::<usize, $type>(addr) })
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    // One wrapper, standing in for a hook address the way the registry really keys them. No test
    // calls through it: a unit test cannot build a trampoline (AGENTS section 4), so the tests write
    // the entry the backend would have created and drive everything that reads it - the lookup, the
    // copy a detour keeps, and the generation that keeps the copy honest. A test that needs a second
    // hook key builds one the linker cannot merge onto this one; a release build folded two
    // identical wrappers onto the same address.
    extern "C" fn armed_wrapper(_this: *mut u8) {}

    // The wrapper address, in the pointer form a copy keeps its tag in. The registry keys a hook by
    // the same address as an integer, so every registry half of a call below casts it.
    fn armed() -> *mut c_void { armed_wrapper as *const () as *mut c_void }

    // A hook key the linker cannot merge with `armed()`, with no wrapper behind it: the registry has
    // no entry for it, which is the uninstalled case.
    fn other_hook() -> *mut c_void { (armed() as usize + 16) as *mut c_void }

    // The generation counter and the barrier's trip counters are both process wide, and barrier item 2
    // welded the two families together: the door rule's take-down (`coroutine_trip_taken_down` ->
    // `Interceptor::unhook`), `record_hook` and `drain_deferred_unhooks` all bump `INSTALL_GENERATION`,
    // and the copy tests in this module assert against that stamp. So both families take the barrier's
    // one turn, the way the probe counters' tests share a single lock.
    //
    // Two locks that did not exclude each other are what made `cargo test --lib` racy. A door test that
    // bumped the generation while holding only the barrier's turn landed that bump between another
    // test's `reread` and its `cached` assertion, that assertion answered `None` where it expected a
    // trampoline, and the same test's `resolve` then fell into the cold branch that reaches
    // `Hachimi::instance()` - `process::exit(1)` at `src/core/hachimi.rs:427` - which ends the test
    // binary and hides every other test's result. The mechanism is pinned below, as a decision run in
    // one thread inside one turn, rather than as a timing window nobody can reproduce on purpose.
    //
    // One lock, not two: `take_turns` and `guard::barrier_turn` are the same mutex, so a test that took
    // both would deadlock on a lock `std::sync::Mutex` does not re-enter.
    fn take_turns() -> MutexGuard<'static, ()> {
        crate::il2cpp::hook::guard::barrier_turn()
    }

    #[test]
    fn an_uninstalled_hook_yields_address_zero() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        // The documented behaviour (C1), through the registry...
        assert_eq!(interceptor.get_trampoline_addr(other_hook() as usize), 0);

        // ... and through the copy a detour now keeps. 0 stays 0: the wrapper is handed address 0
        // rather than a call through a target that does not exist.
        let cache = CachedTrampoline::new(other_hook());
        assert_eq!(cache.cached(other_hook()), None, "a copy that has not read anything is not current");
        assert_eq!(cache.reread(&interceptor, other_hook()), 0);
        assert_eq!(cache.cached(other_hook()), Some(0));
    }

    #[test]
    fn a_resolved_trampoline_needs_the_registry_once() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();
        interceptor.record_hook(armed() as usize, 0x1000);

        let cache = CachedTrampoline::new(armed());
        assert_eq!(cache.cached(armed()), None);
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);

        // The claim C33 is about, proven: after the one read the answer no longer comes from the
        // registry. The Interceptor is gone here and the hot side still answers - it cannot have
        // locked a mutex or hashed a key to do that.
        drop(interceptor);
        assert_eq!(cache.cached(armed()), Some(0x1000));
        assert_eq!(cache.cached(armed()), Some(0x1000));
    }

    #[test]
    fn a_hook_the_registry_loses_is_not_jumped_through() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();
        interceptor.record_hook(armed() as usize, 0x1000);

        let cache = CachedTrampoline::new(armed());
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);

        assert!(interceptor.drop_hook(armed() as usize).is_some());

        // The stamp is behind the hook set, and re-reading finds no entry: the wrapper is handed 0,
        // never the trampoline the backend has taken back.
        assert_eq!(cache.cached(armed()), None);
        assert_eq!(cache.reread(&interceptor, armed()), 0);
        assert_eq!(cache.cached(armed()), Some(0));
    }

    // Barrier item 2 (C2), with its scope settled: the take-down whose backend half has to wait for a
    // point that is not a frame of the hook it takes down is the coroutine door's, and it is the only
    // one. The registry half of every take-down happens in the frame that asked - it is this crate's own
    // data. The backend half is `MH_DisableHook` plus `MH_RemoveHook` on the game's method, which puts
    // the method's bytes back and lets the trampoline go. For a door it is queued - the trip runs inside
    // the door's own frame, mid-call out of that door's own trampoline, and the arm still means to call
    // through it - and `drain_deferred_unhooks`, which the game tick runs, is where it happens. For
    // anything else it runs before `unhook` returns, because queueing it leaves a target armed with
    // nothing in the registry to answer the wrapper still reachable through it.
    //
    // `record_hook_at` writes the entry the backend would have created and names the target the way a
    // real hook names it - a key that is the wrapper, and an `orig_addr` that is the method or slot -
    // because `release_waits_for_a_safe_point` reads the target. `symbols::record_coroutine_door_target`
    // is the fact it consults. The drain then hands an entry to the backend, which answers "no such
    // hook" for an address it never made. That answer is the point these tests read: they prove *when the
    // call was made*, not what the game did with it. What MinHook really restores needs a run
    // (AGENTS section 4).
    const GAME_DOOR_METHOD: usize = 0x4000;
    const NATIVE_TARGET: usize = 0x8000;
    const VTABLE_SLOT: usize = 0x10;

    fn arm_door(interceptor: &Interceptor, wrapper: usize, trampoline_addr: usize) {
        crate::il2cpp::symbols::record_coroutine_door_target(GAME_DOOR_METHOD);
        interceptor.record_hook_at(wrapper, GAME_DOOR_METHOD, trampoline_addr, HookType::Function);
    }

    #[test]
    fn a_take_down_releases_the_registry_half_now_and_runs_the_backend_half_on_the_drain() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();
        arm_door(&interceptor, armed() as usize, 0x1000);

        let cache = CachedTrampoline::new(armed());
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);

        let released_before = interceptor.backend_release_count();

        // This is the call `guard::coroutine_trip` makes, from the door's own frame.
        assert!(interceptor.unhook(armed() as usize).is_some());

        // The registry half happened here: the entry is gone, and the copy is behind the hook set.
        assert_eq!(interceptor.get_trampoline_addr(armed() as usize), 0);
        assert_eq!(cache.cached(armed()), None);

        // The backend half did not: the take-down is waiting and nothing has reached the backend.
        assert_eq!(interceptor.deferred_unhook_count(), 1);
        assert_eq!(interceptor.completed_now_count(), 0,
            "a door that tripped was taken down inside the frame that tripped it");
        assert_eq!(interceptor.backend_release_count(), released_before,
            "the game method was patched back and its trampoline let go inside the frame that asked");

        // While the removal waits the hook is still armed, so its own detour is handed the trampoline it
        // was created with. Handing it 0 instead is the C1 chain: `get_orig_fn!` answers it, the body
        // calls it, and the barrier eats an access violation on a coroutine the game is still driving.
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);
        assert_eq!(cache.cached(armed()), Some(0x1000));

        // The drain is the point that is not that frame.
        assert_eq!(interceptor.drain_deferred_unhooks(), 1);
        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(interceptor.backend_release_count(), released_before + 1);

        // A copy refreshed during the window does not outlive the trampoline it read.
        assert_eq!(cache.cached(armed()), None, "a released trampoline was still current for its wrapper");
        assert_eq!(cache.reread(&interceptor, armed()), 0);
        assert_eq!(cache.cached(armed()), Some(0));
    }

    #[test]
    fn a_take_down_that_is_not_a_door_is_whole_before_the_call_that_asked_returns() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        // The shape of `windows::hook.rs`'s LoadLibraryW, `wnd_hook::uninit`'s SetWindowLongPtr pair and
        // a plugin's `interceptor_unhook`: a function hook on a target no class table ever published.
        interceptor.record_hook_at(armed() as usize, NATIVE_TARGET, 0x1000, HookType::Function);

        let released_before = interceptor.backend_release_count();

        assert!(interceptor.unhook(armed() as usize).is_some());

        // Nothing waits for the tick: the take-down reached the backend in the frame that asked for it,
        // and the queue the game tick drains holds nothing for it. Queueing this shape is what left the
        // target armed - still routing into the wrapper - with no registry entry to answer the wrapper's
        // own ask for its original.
        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(interceptor.completed_now_count(), 1);
        assert_eq!(interceptor.drain_deferred_unhooks(), 0, "the game tick had nothing left to release");
        assert_eq!(interceptor.backend_release_count(), released_before);

        // With the target back to its own bytes the wrapper is not reached again, so it is handed nothing
        // to call: 0 here is the honest answer, not an armed hook nobody recorded.
        assert_eq!(interceptor.reread_trampoline(armed() as usize), 0);
    }

    #[test]
    fn a_vtable_take_down_waits_for_no_point_because_it_frees_nothing() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        // `render_hook`'s IDXGISwapChain::Present / ResizeBuffers pair: `hook_vtable` recorded the slot
        // address as `orig_addr` and the pointer that slot carried as `trampoline_addr`, and the take-down
        // is that pointer written back. 0x10 is a slot no process commits, so the backend is refused and
        // what the test reads is the moment it was asked to run.
        interceptor.record_hook_at(other_hook() as usize, VTABLE_SLOT, 0x2000, HookType::Vtable);

        assert!(interceptor.unhook(other_hook() as usize).is_some());

        // Queued, this pair waited for a Unity main-thread tick that is not the thread calling
        // `Present` (render_hook posts its own work to `Thread::main_thread().schedule`). Waiting bought
        // nothing - it patches no code and frees no trampoline - and cost the caller a completion it was
        // promised on the spot.
        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(interceptor.completed_now_count(), 1);
        assert_eq!(interceptor.drain_deferred_unhooks(), 0);
    }

    #[test]
    fn the_shape_that_waits_is_the_door_the_record_names_and_no_other_hook_type() {
        let door = HookHandle { orig_addr: GAME_DOOR_METHOD, trampoline_addr: 0x1000, hook_type: HookType::Function };
        let native = HookHandle { orig_addr: NATIVE_TARGET, trampoline_addr: 0x1000, hook_type: HookType::Function };
        let slot = HookHandle { orig_addr: GAME_DOOR_METHOD, trampoline_addr: 0x1000, hook_type: HookType::Vtable };

        crate::il2cpp::symbols::record_coroutine_door_target(GAME_DOOR_METHOD);

        assert!(release_waits_for_a_safe_point(&door), "a door's bytes went back inside the frame the game reached them through");
        assert!(!release_waits_for_a_safe_point(&native), "a native target was made to wait for a tick that cannot quiet it");

        // A vtable half writes one word back into a slot. It frees nothing, so it waits for nothing, and
        // an address that happens to be in the door record does not change that.
        assert!(!release_waits_for_a_safe_point(&slot));
    }

    #[test]
    fn a_take_down_is_released_once_and_an_empty_queue_costs_one_load() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        // The per-frame ask with nothing waiting: 0, and no queue lock on the way to saying so.
        assert_eq!(interceptor.drain_deferred_unhooks(), 0);
        assert_eq!(interceptor.backend_release_count(), 0);

        arm_door(&interceptor, armed() as usize, 0x1000);
        assert!(interceptor.unhook(armed() as usize).is_some());
        arm_door(&interceptor, other_hook() as usize, 0x2000);
        assert!(interceptor.unhook(other_hook() as usize).is_some());

        assert_eq!(interceptor.deferred_unhook_count(), 2);
        assert_eq!(interceptor.drain_deferred_unhooks(), 2);
        assert_eq!(interceptor.deferred_unhook_count(), 0);

        // The queue is emptied before the backend runs, so no entry is released twice and no later tick
        // re-patches a method it already restored.
        assert_eq!(interceptor.drain_deferred_unhooks(), 0);
        assert_eq!(interceptor.backend_release_count(), 2);
    }

    // The hint/queue relationship the test set never drove: `deferred_waiting` is what both
    // subtractors - `drain_deferred_unhooks` and `take_down_deferred_for_target` - fetch_sub against,
    // and `defer_backend_unhook` is the only writer. Driven here through all three mutators on two
    // door targets, with the queue's real occupancy read back through the queue itself
    // (`reread_trampoline` answers a wrapper whose removal is waiting from its queued handle), so
    // every step compares the hint with what the queue demonstrably holds. A hint reading below
    // that occupancy is a hint that early-outs a drain over an entry the queue holds - the shape
    // that lets `unhook_all` miss a take-down whose registry half is already gone.
    #[test]
    fn the_hint_tracks_the_queue_it_summarises_through_all_three_mutators() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        const SECOND_DOOR_METHOD: usize = 0x4100;

        // How many of the six wrappers the queue still answers from its own entries: the occupancy
        // the hint summarises, measured by the mutex-backed lookup rather than by the hint itself.
        fn waiting_count(interceptor: &Interceptor) -> usize {
            let mut seen = 0;
            for n in 0..6 {
                if interceptor.reread_trampoline(armed() as usize + n * 16) != 0 {
                    seen += 1;
                }
            }
            seen
        }

        for n in 0..3 {
            crate::il2cpp::symbols::record_coroutine_door_target(GAME_DOOR_METHOD);
            interceptor.record_hook_at(armed() as usize + n * 16, GAME_DOOR_METHOD, 0x1000 + n * 16, HookType::Function);
            assert!(interceptor.unhook(armed() as usize + n * 16).is_some());
        }

        assert_eq!(interceptor.deferred_unhook_count(), 3);
        assert_eq!(waiting_count(&interceptor), 3, "the hint and the queue disagreed on how many take-downs wait");

        // The re-install's forced release subtracts exactly what its split took out; the entries it
        // left keep their count.
        assert_eq!(interceptor.take_down_deferred_for_target(GAME_DOOR_METHOD), 3);
        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(waiting_count(&interceptor), 0);

        crate::il2cpp::symbols::record_coroutine_door_target(SECOND_DOOR_METHOD);
        for n in 3..6 {
            interceptor.record_hook_at(armed() as usize + n * 16, SECOND_DOOR_METHOD, 0x1000 + n * 16, HookType::Function);
            assert!(interceptor.unhook(armed() as usize + n * 16).is_some());
        }

        assert_eq!(interceptor.deferred_unhook_count(), 3);
        assert_eq!(waiting_count(&interceptor), 3, "the hint and the queue disagreed after the re-install stretch");

        // The drain subtracts what it took, and an empty queue then reads 0 rather than a wrapped
        // usize: the arithmetic both subtractors have to respect.
        assert_eq!(interceptor.drain_deferred_unhooks(), 3);
        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(waiting_count(&interceptor), 0);
        assert_eq!(interceptor.drain_deferred_unhooks(), 0, "a drain over a queue the hint no longer matched still released something");
    }

    // The half no single-threaded sequence can drive, and the half the hint got wrong: a drain or a
    // re-install's split that takes an entry out of the queue *before* `defer_backend_unhook` has
    // incremented the hint subtracts from a count that never counted that entry. `AtomicUsize`
    // wraps, a wrapped hint is never 0 again, and every later tick pays the queue lock plus
    // `std::mem::take` the per-frame path exists to avoid (AGENTS section 6) - or, read the other
    // way, an entry the hint has not counted can sit in the queue while the hint reads 0, the
    // relaxed load early-outs over it, and a take-down whose registry half is already gone is missed.
    //
    // Three of the queue's actors run at once: a thread deferring door take-downs the way the
    // barrier's door rule makes them, the game-tick `drain_deferred_unhooks` racing the defer, and
    // `take_down_deferred_for_target` - the re-install `hook` asks before re-arming a door -
    // splitting the same queue. What the test reads is the accounting at quiescence: every queued
    // take-down reaches the backend exactly once, and the hint lands on 0. A hint that
    // over-subtracted cannot land on 0, and an entry the hint left at 0 in the queue is never
    // released. Whether a particular run lands inside the buggy window is a sampling property (a
    // unit test proves a decision, not a measured crash, AGENTS section 4); the decision proved
    // here is that the shipped functions keep the hint and the queue in step under contention.
    // Every queued take-down must reach the backend exactly once, the hint must land on 0, and -
    // the witness the buggy order can leave and the fixed one cannot: a relaxed read of the hint
    // mid-race may never see a value above the number of defers issued at all. A subtract for an
    // entry the hint had not counted wraps `AtomicUsize` to ~usize::MAX, and at that value every
    // game tick's early-out is false: the per-frame path pays the queue lock plus `std::mem::take`
    // the relaxed load exists to skip (AGENTS section 6). The monitor samples the hint while the
    // race runs; whether a run lands inside the buggy window is a sampling property (AGENTS
    // section 4), the structural claim is that on the fixed order the witness is unreachable.
    #[test]
    fn a_drain_racing_the_defer_never_undersubtracts_the_hint_it_lands_on_zero() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        const QUEUED: usize = 2_500;
        // Counted takes never exceed defers issued; anything above this ceiling is a wrapped
        // subtract, not a backlog.
        const HINT_CEILING: usize = QUEUED + 16;

        crate::il2cpp::symbols::record_coroutine_door_target(GAME_DOOR_METHOD);

        let released_before = interceptor.backend_release_count();
        let deferring_done = AtomicBool::new(false);
        let wrapped_hint = AtomicBool::new(false);

        let (drain_released, re_installer_released) = std::thread::scope(|scope| {
            // The deferrer is its own thread: the other two actors must be able to cut into a
            // defer in flight, which an actor on the deferring thread never can.
            let deferrer = scope.spawn(|| {
                let mut queued = 0usize;
                while queued < QUEUED {
                    // The take-down as the door rule asks for it: registry half in the asking
                    // frame, handle pushed onto the queue behind it.
                    interceptor.record_hook_at(armed() as usize + queued * 16, GAME_DOOR_METHOD, 0x1000 + queued * 16, HookType::Function);
                    assert!(interceptor.unhook(armed() as usize + queued * 16).is_some());
                    queued += 1;
                }
                queued
            });

            let drainer = scope.spawn(|| {
                let mut released = 0usize;
                while !deferring_done.load(Ordering::Acquire) {
                    released += interceptor.drain_deferred_unhooks();
                }
                released
            });

            // The re-install `hook` asks before it re-arms a door, from a thread of its own: the
            // other path that mutates the queue and fetch_subs the hint by what its split took out.
            let re_installer = scope.spawn(|| {
                let mut released = 0usize;
                while !deferring_done.load(Ordering::Acquire) {
                    released += interceptor.take_down_deferred_for_target(GAME_DOOR_METHOD);
                }
                released
            });

            let monitor = scope.spawn(|| {
                while !deferring_done.load(Ordering::Acquire) && !wrapped_hint.load(Ordering::Relaxed) {
                    if interceptor.deferred_unhook_count() > HINT_CEILING {
                        wrapped_hint.store(true, Ordering::Release);
                    }
                }
            });

            let queued = deferrer.join().expect("the deferring thread panicked");
            deferring_done.store(true, Ordering::Release);
            let drained = drainer.join().expect("the draining thread panicked");
            let re_installed = re_installer.join().expect("the re-installer thread panicked");
            monitor.join().expect("the monitoring thread panicked");
            assert_eq!(queued, QUEUED);
            (drained, re_installed)
        });

        // A tick after the last take-down, the way `unhook_all`'s drain is the last ask a module
        // being unloaded makes: at quiescence the hint and the queue must read the same thing - empty.
        let settle = interceptor.drain_deferred_unhooks() + interceptor.drain_deferred_unhooks();

        assert!(!wrapped_hint.load(Ordering::Acquire),
            "a drain or a re-install's split took an entry the hint had not yet counted: the subtract wrapped, and any tick landing in that window pays the queue lock the frame path exists to skip");
        assert_eq!(drain_released + re_installer_released + settle, QUEUED,
            "a take-down the queue held was released twice, or never reached the backend");
        assert_eq!(interceptor.backend_release_count() - released_before, QUEUED,
            "the released counts the counters keep and the releases the actors returned disagree");
        assert_eq!(interceptor.deferred_unhook_count(), 0,
            "the hint was not 0 at quiescence: a subtract landed for an entry the hint had not counted (AtomicUsize wraps), or the queue held one its hint read as 0");
        assert_eq!(interceptor.drain_deferred_unhooks(), 0, "the queue still answered after the settling drain");
    }

    #[test]
    fn a_waiting_take_down_is_released_before_the_same_target_is_hooked_again() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        arm_door(&interceptor, armed() as usize, 0x1000);
        assert!(interceptor.unhook(armed() as usize).is_some());

        // The ask `hook` makes before it writes a target: the backend will not create a hook on a target
        // it still holds, so a door re-armed by the next coroutine that needs it has to have its old
        // take-down out of the way first. The ask names the target, which on a real hook is the game's
        // method and not the wrapper key the registry is keyed on.
        assert_eq!(interceptor.take_down_deferred_for_target(GAME_DOOR_METHOD), 1);
        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(interceptor.drain_deferred_unhooks(), 0, "the re-install already released it");

        // A door waiting on a target nobody asked for again keeps waiting for the tick, and the ask that
        // misses names a target no waiting entry carries.
        arm_door(&interceptor, other_hook() as usize, 0x2000);
        assert!(interceptor.unhook(other_hook() as usize).is_some());
        assert_eq!(interceptor.take_down_deferred_for_target(0x9000), 0);
        assert_eq!(interceptor.deferred_unhook_count(), 1);
    }

    // The half of the take-down ordering no single-threaded test can see, and the half the deferred
    // release points had wrong. The registry half of a door take-down bumps the generation, and
    // `reread_trampoline` answers an arm standing in the deferral window from the queue - so a copy can
    // be stamped with the *current* generation and hold exactly the trampoline the drain is about to let
    // go. Invalidating after the backend frees it leaves that copy answering a freed trampoline for the
    // whole time the backend runs, and the queue exists precisely because other arms may be calling
    // through that door while the tick releases it (this file's header): a window a single thread cannot
    // observe at all, since each of its own calls returns before the tick's drain begins.
    //
    // This drives the shipped `drain_deferred_unhooks` against a spinning arm on another thread and
    // orders the two events this module makes observable: the generation bump, and the release the drain
    // commits into `backends_released`. The witness is the defect's exact shape - the arm handed its
    // warmed trampoline at a moment the release has already committed itself. With the bump sequenced
    // before the backend - as `unhook` and `unhook_all` always did - x86_64 store ordering makes the
    // witness unobservable: the counter the arm checks is stored only after the bump, and a load that
    // sees the new counter cannot answer for the stamp generation the bump just moved. The order this
    // test was written against stored the counter first and the bump after the backend loop, so every
    // sample inside that loop witnessed. A unit test can call through no trampoline (AGENTS section 4),
    // so this is an ordering witness, not a measured crash; the leg gate is the x86_64 store ordering
    // the argument rests on - the other legs compile the test module and CI runs it here.
    #[cfg(all(target_os = "windows", target_env = "msvc", target_arch = "x86_64"))]
    #[test]
    fn an_arm_standing_in_the_release_window_is_stale_before_the_trampoline_goes() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        const WARMED: usize = 0x1000;
        const WAITING: usize = 16; // the backend loop the wrong order invalidated after
        const MAX_ARM_TICKS: usize = 30_000_000; // a hang guard, not the sampling budget

        for round in 0..2 {
            arm_door(&interceptor, armed() as usize, WARMED);
            assert!(interceptor.unhook(armed() as usize).is_some());

            // A queue full of door take-downs the tick has not run yet, each its own wrapper key.
            for n in 1..=WAITING {
                let wrapper = armed() as usize + (round * WAITING + n) * 16;
                interceptor.record_hook_at(wrapper, GAME_DOOR_METHOD, 0x2000 + n * 16, HookType::Function);
                assert!(interceptor.unhook(wrapper).is_some());
            }

            // The copy an arm legitimately holds during the window: read back from the queue after every
            // bump the window has taken, so its stamp is the current generation and it answers the
            // trampoline its wrapper is still calling through.
            let cache = CachedTrampoline::new(armed());
            assert_eq!(cache.reread(&interceptor, armed()), WARMED);
            assert_eq!(cache.cached(armed()), Some(WARMED));

            let released_before = interceptor.backend_release_count();
            let handed_off = AtomicBool::new(false);
            let drain_returned = AtomicBool::new(false);

            std::thread::scope(|scope| {
                scope.spawn(|| {
                    let mut ticks = 0;
                    while !drain_returned.load(Ordering::Acquire) && ticks < MAX_ARM_TICKS {
                        ticks += 1;
                        if interceptor.backend_release_count() > released_before {
                            if let Some(addr) = cache.cached(armed()) {
                                if addr == WARMED {
                                    handed_off.store(true, Ordering::Release);
                                    break;
                                }
                            }
                        }
                        std::hint::spin_loop();
                    }
                });

                std::thread::yield_now(); // let the arm enter its spin before the tick runs

                assert_eq!(interceptor.drain_deferred_unhooks(), WAITING + 1);
                drain_returned.store(true, Ordering::Release);
            });

            assert!(!handed_off.load(Ordering::Acquire),
                "round {round}: the drain was freeing a trampoline a copy of the window still answered");

            // The settled state after the drain is the state every other test asserts: the warmed copy
            // is behind the release, and its re-read finds no entry and no queued handle.
            assert_eq!(cache.cached(armed()), None);
            assert_eq!(cache.reread(&interceptor, armed()), 0);
            assert_eq!(interceptor.deferred_unhook_count(), 0);
        }
    }

    #[test]
    fn a_detach_releases_what_was_waiting_for_a_tick_that_will_not_come() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        arm_door(&interceptor, armed() as usize, 0x1000);
        assert!(interceptor.unhook(armed() as usize).is_some());
        interceptor.record_hook(other_hook() as usize, 0x2000);

        // A take-down still waiting when this module is unloaded is a game method whose jump points into
        // code that is about to be gone, so the detach path runs the waiting halves first. The second
        // hook is not a door, so it never entered the queue: the 1 the drain released is the door's, and
        // the other one is taken in the map pass.
        interceptor.unhook_all();

        assert_eq!(interceptor.deferred_unhook_count(), 0);
        assert_eq!(interceptor.backend_release_count(), 1);
        assert_eq!(interceptor.get_trampoline_addr(other_hook() as usize), 0);
    }

    // The reported chain, end to end: the barrier's door rule taking a coroutine door down. The trip
    // half of this belongs to the barrier's own test
    // (`a_door_with_no_answer_says_the_coroutine_is_still_running_and_leaves_the_registry`); what this
    // one adds is the half that was the defect - the backend half `coroutine_trip` used to run on the
    // target address, from inside the frame the game reached through that door.
    //
    // The barrier's turn, which `take_turns` now is as well: this test bumps `INSTALL_GENERATION` twice
    // - through arming the door and through the drain - and the copy tests in this module assert against
    // that stamp, so it may not run while one of them is reading it. It also counts a take-down into
    // the barrier's process-wide totals, which the guard tests read the same way.
    #[test]
    fn the_barrier_door_rule_arms_nothing_and_releases_no_backend_from_the_door_it_is_answering() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        extern "C" fn a_coroutine_door(_enumerator: *mut u64) -> bool { true }

        arm_door(&interceptor, a_coroutine_door as usize, 0x1000);
        let released_before = interceptor.backend_release_count();

        assert!(
            crate::il2cpp::hook::guard::coroutine_trip_taken_down(&interceptor, a_coroutine_door as *const ()),
            "the barrier does not get to say a coroutine finished"
        );

        // Registry half, on the spot, and it alone: no `MH_DisableHook` and no `MH_RemoveHook` on the
        // game method, no trampoline let go, while this frame is standing in that method's detour.
        assert_eq!(interceptor.get_trampoline_addr(a_coroutine_door as usize), 0);
        assert_eq!(interceptor.deferred_unhook_count(), 1);
        assert_eq!(interceptor.backend_release_count(), released_before,
            "the door's game method was patched back from inside its own detour");

        // The game tick is where the bytes actually go back, and it releases each door once.
        assert_eq!(interceptor.drain_deferred_unhooks(), 1);
        assert_eq!(interceptor.backend_release_count(), released_before + 1);
        assert_eq!(interceptor.drain_deferred_unhooks(), 0);
    }

    // The mechanism the racy gate was, pinned as a decision instead of a timing window: a take-down
    // moves the one process-wide stamp every copy carries, whatever hook that copy belongs to. That is
    // correct production behaviour - a copy of an untouched hook may not keep answering a trampoline
    // across a hook set that moved (C33) - and it is exactly why the door tests and the copy tests in
    // this module cannot be allowed to run at the same time. Driven here in one thread inside one turn,
    // with the copy warmed after both hooks exist, so nothing but the take-down can move the stamp.
    #[test]
    fn a_door_take_down_sends_every_unrelated_copy_back_to_the_registry() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();

        extern "C" fn another_coroutine_door(_enumerator: *mut u64) -> bool { true }

        interceptor.record_hook(armed() as usize, 0x1000);
        arm_door(&interceptor, another_coroutine_door as usize, 0x2000);

        let cache = CachedTrampoline::new(armed());
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);
        assert_eq!(cache.cached(armed()), Some(0x1000));

        // The door rule's take-down, on a hook key this copy has nothing to do with.
        assert!(
            crate::il2cpp::hook::guard::coroutine_trip_taken_down(
                &interceptor,
                another_coroutine_door as *const ()
            ),
            "the barrier does not get to say a coroutine finished"
        );

        // The copy goes back to the registry, where its own entry is still sitting: same answer, new
        // read. If this ever answered `Some` straight through, the stamp would not be process wide and
        // the door tests would not need the copy tests' turn.
        assert_eq!(cache.cached(armed()), None,
            "a take-down left an unrelated copy's stamp current, so the copy never went back to the registry");
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);
        assert_eq!(cache.cached(armed()), Some(0x1000));
    }

    #[test]
    fn a_take_down_is_named_at_the_moment_it_happens_and_only_for_the_first_eight() {
        // The log line is the event: before this a door that came down said nothing at the moment it came
        // down - `PlayTrainingCutStateMachine_MoveNext` is armed once in `init`, is never re-armed, and
        // left the session gone with no line marking when. `announce_takedown` is the latch behind the
        // first-N pattern, checked the way the probes' `PROBE_DETAIL_LIMIT` is.
        for n in 1..=TAKEDOWN_LOG_LIMIT {
            assert!(announce_takedown(n), "take-down {n} said nothing at the moment it happened");
        }

        assert!(announce_takedown(TAKEDOWN_LOG_LIMIT + 1), "the line saying the rest are only counted never came");
        assert!(!announce_takedown(TAKEDOWN_LOG_LIMIT + 2));
        assert!(!announce_takedown(TAKEDOWN_LOG_LIMIT + 10_000));
    }

    #[test]
    fn a_reinstalled_hook_is_read_again() {
        let _turn = take_turns();
        // The cycle `windows/gui_impl/render_hook.rs` runs when the overlay is rebuilt: the same
        // wrapper, unhooked and hooked again, behind a new trampoline.
        let interceptor = Interceptor::default();
        interceptor.record_hook(armed() as usize, 0x1000);

        let cache = CachedTrampoline::new(armed());
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);

        interceptor.drop_hook(armed() as usize);
        interceptor.record_hook(armed() as usize, 0x2000);

        assert_eq!(cache.cached(armed()), None, "the copy of a released trampoline is not current");
        assert_eq!(cache.reread(&interceptor, armed()), 0x2000);
        assert_eq!(cache.cached(armed()), Some(0x2000));
    }

    #[test]
    fn a_poisoned_registry_lock_answers_instead_of_panicking() {
        let _turn = take_turns();
        // C2's shape: a detour that panicked while holding the registry lock. Every method above
        // used `hook_map.lock().unwrap()`, so every armed call after that panicked across an
        // `extern "C"` frame - one bad hook turned into a dead process.
        let interceptor = Interceptor::default();

        let tripped = catch_unwind(AssertUnwindSafe(|| {
            let _guard = shared_lock(&interceptor.hook_map);
            panic!("a detour body that blew up holding the registry lock");
        }));
        assert!(tripped.is_err(), "the lock is poisoned on purpose");

        assert_eq!(interceptor.get_trampoline_addr(armed() as usize), 0);
        interceptor.record_hook(armed() as usize, 0x1000);
        assert_eq!(interceptor.get_trampoline_addr(armed() as usize), 0x1000);

        let cache = CachedTrampoline::new(armed());
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);
    }

    #[test]
    fn a_cache_never_answers_for_another_hook() {
        let _turn = take_turns();
        // `get_orig_fn!` takes its copy from `trampoline_cache!`, which declares the static inside the
        // expansion and tags it with the wrapper's address. Two expansions in one body - the shape of
        // `DownloadView`, which walks `Show` and `Hide` out of one helper - may not answer for each
        // other's hook: a detour handed another hook's trampoline jumps to the wrong method. The tag
        // is checked rather than assumed because a release build of an earlier draft of this file
        // folded two identically initialised copies into one address, which is what a linker folding
        // identical data does to two caches that start out the same.
        let mine = trampoline_cache!(armed_wrapper);
        let other = CachedTrampoline::new(other_hook());

        let interceptor = Interceptor::default();
        interceptor.record_hook(armed() as usize, 0x1000);

        assert_eq!(mine.reread(&interceptor, armed()), 0x1000);
        assert_eq!(other.reread(&interceptor, other_hook()), 0);

        // Each answers for the hook it was made for...
        assert_eq!(mine.cached(armed()), Some(0x1000));
        assert_eq!(other.cached(other_hook()), Some(0));

        // ... and refuses to answer for another hook, even if the two copies ever end up sharing
        // storage: it goes back to the registry instead of handing over an address it did not resolve.
        assert_eq!(mine.cached(other_hook()), None);
        assert_eq!(other.cached(armed()), None);
    }

    #[test]
    fn a_call_that_is_not_a_detour_body_is_handed_nothing_to_call() {
        let _turn = take_turns();
        // The state a run leaves behind when a target never resolved: `new_hook!` logs
        // `X_addr is null` and installs nothing, so the registry holds no entry for that wrapper.
        // For a detour that is harmless (C1: it is only reachable through its own trampoline). For
        // `Text::apply_translations`, `Localize::dump_strings`, `Screen::get_Width_orig` and the
        // wrappers Rust code calls directly, the 0 `get_orig_fn!` hands out is a call the mod makes
        // itself, on a path no detour barrier is standing on.
        let interceptor = Interceptor::default();
        let cache = CachedTrampoline::new(other_hook());

        // The same cold read `resolve` does, taken here directly so the test never reaches the
        // singleton: the registry answers 0, and the copy now holds that answer.
        assert_eq!(cache.reread(&interceptor, other_hook()), 0);
        assert_eq!(cache.cached(other_hook()), Some(0));

        // The guard is the difference: 0 stays a missing target instead of a callable pointer, and
        // every later ask is answered from the copy, not from the registry.
        assert_eq!(cache.resolve_or_none(other_hook(), "uninstalled"), None);
        assert_eq!(cache.resolve_or_none(other_hook(), "uninstalled"), None);
    }

    #[test]
    fn the_reason_for_staying_inert_is_said_once_per_call_site() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();
        let cache = CachedTrampoline::new(other_hook());
        assert_eq!(cache.reread(&interceptor, other_hook()), 0);

        assert!(!cache.unresolved_reported(), "a copy that never skipped says nothing");

        // The asks below go through `resolve`, whose cold branch reaches `Hachimi::instance()` and ends
        // the test process (AGENTS section 4), so the copy being current is asserted first: a copy that
        // fell behind the hook set fails as an assertion here, never as a dead test binary.
        assert_eq!(cache.cached(other_hook()), Some(0),
            "the copy was behind the hook set, and the ask below would have left this test for the singleton");

        // One skipped call says it once. The 10,000 after that - the shape of a wrapper a per frame
        // path calls into an uninstalled hook - may not add a line, which is what AGENTS section 6
        // forbids on a hot path.
        assert_eq!(cache.resolve_or_none(other_hook(), "uninstalled"), None);
        assert!(cache.unresolved_reported(), "the first skip said nothing");

        for _ in 0..10_000 {
            assert_eq!(cache.resolve_or_none(other_hook(), "uninstalled"), None);
        }

        assert!(!cache.claim_unresolved_report(), "an unresolved target reported again on the call path");
    }

    #[test]
    fn a_hook_that_resolved_travels_the_guard_unchanged() {
        let _turn = take_turns();
        let interceptor = Interceptor::default();
        interceptor.record_hook(armed() as usize, 0x1000);

        let cache = trampoline_cache!(armed_wrapper);
        assert_eq!(cache.reread(&interceptor, armed()), 0x1000);

        // The armed case is the common one, and the guard must not change it: same trampoline as
        // `resolve` hands out, no report spent on it, and the copy answers the next call. The copy is
        // proved current first for the same reason as above - `resolve`'s cold branch is the singleton,
        // and the singleton ends this process.
        assert_eq!(cache.cached(armed()), Some(0x1000),
            "the copy was behind the hook set, and the guard ask below would have left this test for the singleton");
        assert_eq!(cache.resolve_or_none(armed(), "armed"), Some(0x1000));
        assert_eq!(cache.resolve(armed()), 0x1000);
        assert_eq!(cache.resolve_or_none(armed(), "armed"), Some(0x1000));
        assert!(!cache.unresolved_reported(), "an armed hook spent the once-only report");
    }
}
