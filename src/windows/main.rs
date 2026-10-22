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

// A panic that reaches a frame which cannot unwind ends the process with 0xc0000409 and its message goes to a
// stderr the game does not have, so nothing of it reaches `hachimi.log` (which is also off until the config is
// read). This appends the message and its source location to `hachimi/panic.log` beside the game, whatever the
// config says, and then hands on to the default hook.
fn install_panic_file_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("hachimi").join("panic.log")));
        if let Some(path) = path {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let thread = std::thread::current();
                let _ = writeln!(
                    file,
                    "{:?} thread {:?}: {}",
                    std::time::SystemTime::now(),
                    thread.name().unwrap_or("<unnamed>"),
                    info
                );
            }
        }
        default_hook(info);
    }));
}

#[no_mangle]
#[allow(non_snake_case)]
pub extern "C" fn DllMain(hmodule: HMODULE, call_reason: c_ulong, _reserved: *mut c_void) -> bool {
    if call_reason == DLL_PROCESS_ATTACH {
        unsafe { DLL_HMODULE = hmodule; }
        install_panic_file_hook();
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
        // The line is said before anything here can fail. `wnd_hook::uninit` restores a window subclass
        // and closes the Discord pipe, and ten runs of `hachimi.log` printed neither this line nor the
        // counts, which left the fork unable to tell a detach that never arrived from one that died on
        // the way to the reports. The counts are claimed in `core::hachimi`, so the exit call door in
        // `src/windows/hook.rs` says them once if it got there first and this branch says nothing.
        info!("Unhooking everything");
        // Cold path, and the place the barrier's counts are said. A trip still never logs on the spot -
        // a hook that faults every frame would otherwise write a line every frame - with one exception:
        // a hook take-down names itself at the moment it happens, for the first eight of them
        // (`core::interceptor::TAKEDOWN_LOG_LIMIT`), because a door that came down for the rest of the
        // session used to leave no trace at all until this line.
        crate::core::hachimi::report_take_down_once();

        wnd_hook::uninit();
        Hachimi::instance().interceptor.unhook_all();
    }
    TRUE.into()
}