use std::sync::atomic;

use crate::{core::Hachimi, il2cpp::{api::il2cpp_resolve_icall, types::*}};

type SetVSyncCountFn = extern "C" fn(value: i32);
def_detour! {
    pub set_vSyncCount(mut value: i32) {
            let vsync_count = Hachimi::instance().vsync_count.load(atomic::Ordering::Relaxed);
        if vsync_count != -1 {
            value = vsync_count;
        }
        // `windows/hachimi_impl.rs` and the GUI's vsync row call this wrapper directly, so this
        // body is not only a trampoline body: with the icall unresolved the answer would be a call
        // through 0 from the overlay thread (C1). Leaving the game's own vsync setting alone is
        // the inert answer.
        let Some(set_vsync_count) = get_orig_fn_guarded!(set_vSyncCount, SetVSyncCountFn) else {
            return;
        };

        set_vsync_count(value);
    }
}

pub fn init(_UnityEngine_CoreModule: *const Il2CppImage) {
    let set_vSyncCount_addr = il2cpp_resolve_icall(
        c"UnityEngine.QualitySettings::set_vSyncCount(System.Int32)".as_ptr()
    );

    new_hook!(set_vSyncCount_addr, set_vSyncCount);
}