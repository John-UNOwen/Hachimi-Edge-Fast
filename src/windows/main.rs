use std::os::raw::{c_ulong, c_void};

use widestring::U16CString;
use windows::{core::PCWSTR, Win32::{Foundation::{HMODULE, TRUE}, System::LibraryLoader::LoadLibraryW}};

use crate::{core::{plugin_api::Plugin, Hachimi}, windows::utils};

use super::{hook, wnd_hook};

const DLL_PROCESS_ATTACH: c_ulong = 1;
const DLL_PROCESS_DETACH: c_ulong = 0;

pub fn load_libraries() -> Vec<Plugin> {
    let mut plugins = Vec::new();
    for name in Hachimi::instance().config.load().windows.load_libraries.iter() {
        let Ok(name_cstr) = U16CString::from_str(name) else {
            warn!("Invalid library name: {}", name);
            continue;
        };
        let res = unsafe { LoadLibraryW(PCWSTR(name_cstr.as_ptr())) };

        if let Ok(handle) = res {
            if !handle.is_invalid() {
                let init_enum = {
                    let v3_addr = utils::get_proc_address(handle, c"hachimi_init_v3");
                    if v3_addr != 0 {
                        Some(crate::core::plugin_api::PluginInit::V3(unsafe { std::mem::transmute(v3_addr) }))
                    } else {
                        let v2_addr = utils::get_proc_address(handle, c"hachimi_init");
                        if v2_addr != 0 {
                            Some(crate::core::plugin_api::PluginInit::V2(unsafe { std::mem::transmute(v2_addr) }))
                        } else {
                            None
                        }
                    }
                };

                if let Some(init_fn) = init_enum {
                    info!("Loaded library: {}", name);
                    plugins.push(Plugin {
                        name: name.clone(),
                        init_fn,
                    });
                } else {
                    warn!("Library loaded but missing hachimi_init: {}", name);
                }

                continue;
            }
        }

        warn!("Failed to load library: {}", name);
    }

    plugins
}

pub static mut DLL_HMODULE: HMODULE = HMODULE(0 as _);

#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn DllMain(hmodule: HMODULE, call_reason: c_ulong, _reserved: *mut c_void) -> bool {
    if call_reason == DLL_PROCESS_ATTACH {
        unsafe { DLL_HMODULE = hmodule; }
        if !Hachimi::init() {
            return TRUE.into();
        }

        let hachimi = Hachimi::instance();
        // C2: `unwrap()` here is a panic inside `DllMain`, a frame that cannot unwind - one plugin
        // that died holding its own list would end the process on the next attach. The cell is the
        // mod's own, and the list being written is the one every later reader of it expects.
        *crate::core::hachimi::recover_lock(&hachimi.plugins) = load_libraries();

        hook::init();
        info!("Attach completed");
    }
    else if call_reason == DLL_PROCESS_DETACH && Hachimi::is_initialized() {
        wnd_hook::uninit();

        info!("Unhooking everything");
        // Cold path, and the place the barrier's counts are said. A trip still never logs on the spot -
        // a hook that faults every frame would otherwise write a line every frame - with one exception:
        // a hook take-down names itself at the moment it happens, for the first eight of them
        // (`core::interceptor::TAKEDOWN_LOG_LIMIT`), because a door that came down for the rest of the
        // session used to leave no trace at all until this line.
        crate::il2cpp::hook::guard::report_trips();
        crate::core::hachimi::report_poisoned_lock_recoveries();
        Hachimi::instance().interceptor.unhook_all();
    }
    TRUE.into()
}