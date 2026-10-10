#![allow(non_snake_case)]

use std::path::{Path, PathBuf};

use windows::{core::{w, PCWSTR}, Win32::{Foundation::HMODULE, System::LibraryLoader::GetModuleHandleW}};

use crate::{core::{Error, Hachimi}, windows::{main::DLL_HMODULE, steamworks, utils::{self, get_module_file_name}}};

use super::{hachimi_impl, proxy, ffi};

type LoadLibraryWFn = extern "C" fn(filename: PCWSTR) -> HMODULE;
extern "C" fn LoadLibraryW(filename: PCWSTR) -> HMODULE {
    let hachimi = Hachimi::instance();
    let orig_fn: LoadLibraryWFn = unsafe {
        std::mem::transmute(hachimi.interceptor.get_trampoline_addr(LoadLibraryW as *const () as usize))
    };

    let handle = orig_fn(filename);
    let filename_str = unsafe { filename.to_string().expect("valid utf-16 filename") };

    if hachimi_impl::is_criware_lib(&filename_str) {
        // Manually trigger a GameAssembly.dll load anyways since hachimi might have been loaded later
        let assembly_module = orig_fn(w!("GameAssembly.dll")).0 as usize;
        if assembly_module != 0 {
            hachimi.on_dlopen("GameAssembly.dll", assembly_module);
        }
    }

    let needs_init_steamworks = steamworks::is_overlay_conflicting(&hachimi);
    if hachimi.on_dlopen(&filename_str, handle.0 as usize) {
        if !needs_init_steamworks {
            hachimi.interceptor.unhook(LoadLibraryW as *const () as usize);
        }
    }
    else if needs_init_steamworks &&
        Path::new(&filename_str).file_name().is_some_and(|name| name == "steam_api64.dll")
    {
        steamworks::init(handle);
        hachimi.interceptor.unhook(LoadLibraryW as *const () as usize);
    }
    handle
}

type ExitProcessFn = extern "C" fn(exit_code: u32);
extern "C" fn ExitProcess(exit_code: u32) {
    // The door a normal in game quit reaches, which `DLL_PROCESS_DETACH` apparently does not: no run
    // this fork has read printed the take-down counts, and the detach branch in `main.rs` was the only
    // place they were said, so the fork could not tell a detach that never arrived from one that died
    // on the way to the reports. The printing stands behind the barrier because this boundary cannot
    // unwind, and a panic here would end the game in the middle of leaving. The call to the game's own
    // exit never stands behind it and is never skipped: a mod that ate the exit call would leave the
    // process hanging rather than closed.
    let _ = crate::il2cpp::hook::guard::detour_fallback_or(
        |_answer| {
            info!("ExitProcess asked for with code {}", exit_code);
            crate::core::hachimi::report_take_down_once();
        },
        || {},
    );

    let hachimi = Hachimi::instance();
    let trampoline = hachimi.interceptor.get_trampoline_addr(ExitProcess as *const () as usize);
    if trampoline == 0 {
        // C1: an uninstalled hook answers 0, and 0 is not a way out of a process.
        unsafe { ffi::ExitProcess(exit_code) };
        return;
    }

    let orig_fn: ExitProcessFn = unsafe { std::mem::transmute(trampoline) };
    orig_fn(exit_code);
}

fn init_internal() -> Result<(), Error> {
    let hachimi = Hachimi::instance();

    let module_name = PathBuf::from(unsafe { get_module_file_name(DLL_HMODULE) }.to_string())
        .file_name().map(|s| s.to_string_lossy().to_ascii_lowercase());

    match module_name.as_deref() {
        Some("unityplayer.dll") => {
            info!("Init UnityPlayer.dll proxy");
            proxy::unityplayer::init();
        }
        Some("winhttp.dll") => {
            info!("Init winhttp.dll proxy");
            proxy::winhttp::init(&utils::_get_system_directory());
        }
        other => {
            // C1: `exports.def` is linked into every build (`build.rs:56`), so every proxy export is
            // reachable whatever this file is named, and this fork names it `cri_mana_vpx.dll` (C4).
            // Skipping the init here used to leave every forwarding cell at 0 for the whole session.
            // An unknown name is not a reason to leave them unread: it means the targets come from
            // the modules the process already has instead of from a copy this mod makes.
            info!("Unknown module name {:?}, no proxy init of its own", other);
        }
    }

    // Runs for every name, after any init above: fills the cells that are still empty out of the
    // modules already loaded, and prints the census that says which ones could not be filled.
    proxy::install(module_name.as_deref().unwrap_or(""));

    info!("Hooking LoadLibraryW");
    hachimi.interceptor.hook(ffi::LoadLibraryW as *const () as usize, LoadLibraryW as *const () as usize)?;

    // Armed beside the loader hook and never disarmed: the game calls this once, on the way out.
    info!("Hooking ExitProcess");
    hachimi.interceptor.hook(ffi::ExitProcess as *const () as usize, ExitProcess as *const () as usize)?;

    if let Ok(handle) = unsafe { GetModuleHandleW(w!("GameAssembly.dll")) } {
        info!("Late loading detected");
        hachimi.on_dlopen("GameAssembly.dll", handle.0 as _);
        hachimi.on_hooking_finished();   
    }

    Ok(())
}

pub fn init() {
    init_internal().unwrap_or_else(|e| {
        error!("Init failed: {}", e);
    });
}