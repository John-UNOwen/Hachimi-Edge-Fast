use crate::{core::hachimi::recover_lock, il2cpp::{symbols::get_method_addr, types::*}};

use super::Connection::SELECT_QUERIES;

type GetTextFn = extern "C" fn(this: *mut Il2CppObject, idx: i32) -> *mut Il2CppString;
def_detour! {
    pub GetText(this: *mut Il2CppObject, idx: i32) -> *mut Il2CppString {
            // `il2cpp::sql.rs` reads columns straight through this wrapper - skill text, story
        // strings, item names - so this body runs on paths that are not its own trampoline. Every
        // one of those call sites maps a null column onto a default, which is what an uninstalled
        // `GetText` answers with here. A call through 0 in a master-data read is not (C1).
        let Some(get_text) = get_orig_fn_guarded!(GetText, GetTextFn) else {
            return std::ptr::null_mut();
        };

        if let Some(query) = recover_lock(&SELECT_QUERIES).get(&(this as usize)) {
            return query.get_text(this, idx).unwrap_or_else(|| get_text(this, idx));
        }

        get_text(this, idx)
    }
}

type DisposeFn = extern "C" fn(this: *mut Il2CppObject);
def_detour! {
    pub Dispose(this: *mut Il2CppObject) {
            recover_lock(&SELECT_QUERIES).remove(&(this as usize));

        // Same wrapper, called by every `sql.rs` reader when it closes a query. Dropping the mod's
        // own reference above is all the mod had to do; the game's `Dispose` is only there to call
        // when the hook is installed (C1).
        let Some(dispose) = get_orig_fn_guarded!(Dispose, DisposeFn) else {
            return;
        };

        dispose(this);
    }
    bail {
                // The bail arm runs outside the detour barrier, so it gets the guard too: a second
        // jump to 0 there is what turns a fault the barrier caught into a dead process.
        let Some(dispose) = get_orig_fn_guarded!(Dispose, DisposeFn) else { return };

        dispose(this)
    }
}

static mut GETINT_ADDR: usize = 0;
impl_addr_wrapper_fn!(GetInt, GETINT_ADDR, i32, this: *mut Il2CppObject, index: i32);

static mut STEP_ADDR: usize = 0;
impl_addr_wrapper_fn!(Step, STEP_ADDR, bool, this: *mut Il2CppObject);

pub fn init(LibNative_Runtime: *const Il2CppImage) {
    get_class_or_return!(LibNative_Runtime, "LibNative.Sqlite3", Query);

    let GetText_addr = get_method_addr(Query, c"GetText", 1);
    let Dispose_addr = get_method_addr(Query, c"Dispose", 0);

    new_hook!(GetText_addr, GetText);
    new_hook!(Dispose_addr, Dispose);

    unsafe {
        GETINT_ADDR = get_method_addr(Query, c"GetInt", 1);
        STEP_ADDR = get_method_addr(Query, c"Step", 0);
    }
}