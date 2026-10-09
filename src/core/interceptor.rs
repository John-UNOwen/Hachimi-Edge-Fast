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
    batching: AtomicBool
}

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
/// released.
///
/// `0` means "this hook is not installed", which is what `get_orig_fn!` documented before (C1): the
/// caller is handed address 0 rather than a call through a target the registry does not have.
pub struct CachedTrampoline {
    // The wrapper's own address, kept as a pointer because a function address cannot be cast to an
    // integer in a const context, and this field has to be filled in at compile time.
    hook: AtomicPtr<c_void>,
    // The address is written before the stamp, and the pairing a reader uses is the stamp: an
    // acquire load of a stamp that matches the registry's generation has therefore acquired the
    // address written before the release store of that stamp.
    addr: AtomicUsize,
    stamp: AtomicUsize
}

impl CachedTrampoline {
    pub const fn new(hook_addr: *mut c_void) -> Self {
        Self {
            hook: AtomicPtr::new(hook_addr),
            addr: AtomicUsize::new(0),
            stamp: AtomicUsize::new(0)
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
        let addr = interceptor.get_trampoline_addr(hook_addr as usize);

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
}

impl Interceptor {
    pub fn hook(&self, orig_addr: usize, hook_addr: usize) -> Result<usize, Error> {
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

    pub fn get_trampoline_addr(&self, hook_addr: usize) -> usize {
        if let Some(hook) = shared_lock(&self.hook_map).get(&hook_addr) {
            hook.trampoline_addr
        }
        else {
            warn!("Attempted to get invalid hook: {}", hook_addr);
            0
        }
    }

    pub fn unhook(&self, hook_addr: usize) -> Option<HookHandle> {
        let hook = shared_lock(&self.hook_map).remove(&hook_addr)?;

        // Out of the registry, and behind its stamp, before the backend takes the hook down: a copy
        // that had not been read yet is sent back to the registry, where this entry is gone, and is
        // handed 0 instead of a trampoline about to be released.
        invalidate_trampoline_caches();

        if let Err(e) = unsafe { hook.unhook() } {
            error!("Failed to unhook {}: {}", hook.orig_addr, e);
        }

        Some(hook)
    }

    pub fn unhook_all(&self) {
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

macro_rules! get_orig_fn {
    ($hook:ident, $type:tt) => (
        unsafe {
            ::std::mem::transmute::<usize, $type>(
                trampoline_cache!($hook).resolve($hook as *const () as *mut ::std::os::raw::c_void)
            )
        }
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

    // The generation counter is process wide, so the tests that stamp against it take turns, the way
    // the probe counters' tests do.
    static REGISTRY_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn take_turns() -> MutexGuard<'static, ()> {
        shared_lock(&REGISTRY_TEST_LOCK)
    }

    impl Interceptor {
        // The registry half of `hook`, without the backend call.
        fn record_hook(&self, hook_addr: usize, trampoline_addr: usize) {
            shared_lock(&self.hook_map).insert(
                hook_addr,
                HookHandle { orig_addr: hook_addr, trampoline_addr, hook_type: HookType::Function }
            );
            invalidate_trampoline_caches();
        }

        // The registry half of `unhook`, in the same order the shipped one does it.
        fn drop_hook(&self, hook_addr: usize) -> Option<HookHandle> {
            let hook = shared_lock(&self.hook_map).remove(&hook_addr)?;
            invalidate_trampoline_caches();
            Some(hook)
        }
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
}
