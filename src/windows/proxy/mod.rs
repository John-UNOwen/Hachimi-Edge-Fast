// C1: what an exported proxy stub may do, and the registry the cold paths read.
//
// `proxy_proc!` writes a name into the DLL's export table (`build.rs` links
// `src/windows/proxy/exports.def` into every msvc build, whatever file the deployer renames the
// cdylib to) and one `static mut` cell that the stub reads on every call. The cell's only writers
// are the `init` functions in this directory, and `src/windows/hook.rs` ran them for two module
// names. This fork ships as `cri_mana_vpx.dll` (C4), so nothing ever wrote a cell while all 47
// exports stayed reachable, and the stub's only instruction was `jmp qword ptr [rip + 0]`.
//
// Measured on the linked binary by `python tools/check_proxy_export_stubs.py` (which walks the export
// table the link published and decodes what each stub opens with, and with `--call` maps the file
// without running `DllMain` and calls all 47 with every cell unwritten): on a build of the old macro
// 47 stubs open with `jmp qword ptr [rip + cell]` and the first call takes an access violation at
// address 0; on a build of this file 47 read and test their cell and 47 answer the call. The 0 is the
// whole defect, and it is an execution *through* 0 - the signature `guard::NULL_TARGET_FAULT_TRIPS`
// counts.
//
// Closed at the macro and at the registry rather than at 47 call sites:
//
// 1. The stub cannot execute 0. A cell holding a target jumps to it exactly as before; a cell holding
//    0 answers the caller and counts the refusal. No cell state reaches an execution through 0.
// 2. `install` runs whatever name the mod is loaded under and fills a cell from the module that
//    hosts the export, so a call that does arrive is forwarded rather than refused.

use std::ffi::CString;
use std::sync::atomic::{AtomicUsize, Ordering};

use widestring::U16CString;
use windows::core::PCWSTR;
use windows::Win32::{Foundation::HMODULE, System::LibraryLoader::GetModuleHandleW};

use crate::windows::{main::DLL_HMODULE, utils};

macro_rules! proxy_proc {
    ($name:ident, $orig_var_name:ident) => {
        static mut $orig_var_name: usize = 0;

        std::arch::global_asm!(
            concat!(".globl ", stringify!($name)),
            concat!(stringify!($name), ":"),
            // The load the old stub already did, plus a test. A filled cell jumps to the target it
            // names, the same two-instruction forwarding `jmp qword ptr [rip + <orig>]` was; an empty
            // cell is answered. One extra instruction on a call to a proxy export, which is a network
            // or a startup call and never a per frame path.
            "    mov rax, qword ptr [rip + {}]",
            "    test rax, rax",
            concat!("    jz ", stringify!($name), "_no_target"),
            "    jmp rax",
            // The refusal arm. `sub rsp, 40` is the 32 bytes of shadow space the `extern "C"` frame
            // the call lands on needs plus the 8 that leaves rsp 16 byte aligned at the call (MS x64
            // enters a callee at rsp == 8 mod 16), and it sits entirely below the entry rsp, so
            // nothing the game's own caller spilled is touched. `lea rcx, ...` may clobber the first
            // argument register because this arm never forwards the call it is refusing, and it
            // answers 0: NULL handle / FALSE, the failure every `WinHttp*` signature reports.
            concat!(stringify!($name), "_no_target:"),
            "    sub rsp, 40",
            "    lea rcx, [rip + {}]",
            "    call {}",
            "    add rsp, 40",
            "    xor eax, eax",
            "    ret",
            sym $orig_var_name,
            sym $orig_var_name,
            sym crate::windows::proxy::export_refused,
        );
    };
}

/// One exported stub: the name the export table carries, the module the real function lives in, and
/// the cell the generated jump reads. Written by `proxy_table!` off the same list the stubs are
/// written from, so a cell can never be registered under a name other than the export it belongs to.
pub struct ProxyExport {
    pub name: &'static str,
    pub module: &'static str,
    pub cell: *const usize,
}

// Compile time data over cells this directory owns, never rewritten as a whole. The cells themselves
// are written only on the attach path, before any game code can reach an export.
unsafe impl Sync for ProxyExport {}

/// A proxy table: the exported stubs, their cells, and the registry entries for both.
macro_rules! proxy_table {
    ( module = $module:literal ; $( $name:ident , $orig_var_name:ident ; )* ) => {
        $( proxy_proc!($name, $orig_var_name); )*

        pub static EXPORT_TABLE: &[crate::windows::proxy::ProxyExport] = &[
            $(
                crate::windows::proxy::ProxyExport {
                    name: stringify!($name),
                    module: $module,
                    cell: ::std::ptr::addr_of!($orig_var_name),
                },
            )*
        ];
    };
}

pub mod unityplayer;
pub mod winhttp;

/// Every exported stub both tables declare.
pub fn exports() -> impl Iterator<Item = &'static ProxyExport> {
    unityplayer::EXPORT_TABLE
        .iter()
        .chain(winhttp::EXPORT_TABLE.iter())
}

/// The stub's cell back to the export it belongs to, for the cold arm that names what it refused.
fn export_for_cell(cell: *const usize) -> Option<&'static ProxyExport> {
    exports().find(|entry| entry.cell == cell)
}

/// `install` decided to leave a cell alone because the only module that could answer the export is
/// the file the mod itself was loaded from. Reading the export there hands the stub its own address.
fn reads_the_mods_own_file(injected_module_name: &str, host_module: &str) -> bool {
    injected_module_name.eq_ignore_ascii_case(host_module)
}

/// One line for the first `REFUSAL_LOG_LIMIT` refusals and a total every `REFUSAL_TOTAL_PERIOD`
/// after that: a stub called with nothing to forward is counted, not written once per call
/// (AGENTS section 6). `None` is the "say nothing" answer, so the claim is about a log line the run
/// can read and not about a `warn!` the test process never installs.
const REFUSAL_LOG_LIMIT: usize = 8;
const REFUSAL_TOTAL_PERIOD: usize = 4096;

fn refusal_line(name: &str, host_module: &str, trip: usize) -> Option<String> {
    if trip <= REFUSAL_LOG_LIMIT {
        Some(format!(
            "Proxy export {} has no resolved target in {}: call refused ({} refusal(s) so far)",
            name, host_module, trip
        ))
    }
    else if trip % REFUSAL_TOTAL_PERIOD == 0 {
        Some(format!(
            "Proxy export {} still has no resolved target in {}: {} refusal(s) so far",
            name, host_module, trip
        ))
    }
    else {
        None
    }
}

static REFUSALS: AtomicUsize = AtomicUsize::new(0);

/// Test-only: the refusals this process counted. A run reads them off the cold lines above, which
/// carry the same number.
#[cfg(test)]
pub fn refusal_count() -> usize {
    REFUSALS.load(Ordering::Relaxed)
}

/// The arm every empty cell lands on: count the refusal, name the export for the first few, and
/// answer 0. Nothing here may unwind - the frame that calls it is asm with no unwind path, and a
/// panic out of it would end the process instead of refusing one call - so the only thing on this
/// path that can panic, the log, goes behind `catch_unwind` and the count stands either way.
#[cold]
pub unsafe extern "C" fn export_refused(cell: *const usize) -> usize {
    let trip = REFUSALS.fetch_add(1, Ordering::Relaxed) + 1;

    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let (name, host) = match export_for_cell(cell) {
            Some(entry) => (entry.name, entry.module),
            None => ("unknown proxy export", "unknown module"),
        };

        if let Some(line) = refusal_line(name, host, trip) {
            warn!("{}", line);
        }
    }));

    0
}

/// What one `install` pass found. Kept as data apart from the line so a test can drive the line.
pub struct Census {
    /// Cells a proxy `init` for this very module name had already filled.
    pub forwarded_by_proxy_init: usize,
    /// Cells this pass filled out of a module the process already had loaded.
    pub filled_now: usize,
    /// Exports whose host module is this file, so nothing here may read them.
    pub own_file: Vec<&'static str>,
    /// Exports with no host module in the process and no target to write.
    pub unfilled: Vec<&'static str>,
}

impl Census {
    fn new() -> Self {
        Census {
            forwarded_by_proxy_init: 0,
            filled_now: 0,
            own_file: Vec::new(),
            unfilled: Vec::new(),
        }
    }

    fn total(&self) -> usize {
        self.forwarded_by_proxy_init + self.filled_now + self.own_file.len() + self.unfilled.len()
    }
}

/// The attach line a run reads for this item. It names the module the mod was loaded under, the
/// counts, and every export it left without a target, so "these stubs forward" and "these stubs are
/// inert" are two different things a run can tell apart. What it does not claim is the shape: an
/// inert stub answers, it does not jump, and `proxy_proc!` is why - that half is driven by
/// `an_export_stub_with_an_unfilled_cell_answers_the_call_instead_of_executing_address_0`.
pub fn census_report(injected_module_name: &str, census: &Census) -> String {
    let mut left = String::new();

    if !census.own_file.is_empty() {
        left.push_str(&format!(
            ", {} left alone because they name this file ({})",
            census.own_file.len(),
            census.own_file.join(", ")
        ));
    }
    if !census.unfilled.is_empty() {
        left.push_str(&format!(
            ", {} with no module loaded to read ({})",
            census.unfilled.len(),
            census.unfilled.join(", ")
        ));
    }

    format!(
        "Proxy export census for module \"{}\": {} stub(s), {} filled by their own proxy init, {} filled from the loaded modules{}",
        injected_module_name,
        census.total(),
        census.forwarded_by_proxy_init,
        census.filled_now,
        left
    )
}

/// The host module of an export, only if the process already has it loaded. `GetModuleHandleW` and
/// nothing else: a `LoadLibraryW` from `DllMain` is the loader lock cost C25 is about, and the
/// modules these exports name are the ones the game itself pulls in.
fn loaded_host_module(module_name: &str) -> Option<HMODULE> {
    let Ok(name) = U16CString::from_str(module_name) else {
        return None;
    };

    match unsafe { GetModuleHandleW(PCWSTR(name.as_ptr())) } {
        Ok(handle) if !handle.is_invalid() => Some(handle),
        _ => None,
    }
}

/// What `install` does with one export, held apart from the loader calls its arguments come from.
/// The order is the safety: a cell a proxy `init` already filled is never rewritten (a deployment
/// named `winhttp.dll` keeps the target `winhttp::init` resolved for it, not a lookup that answers
/// the mod itself), and a cell is never filled out of the image the mod itself is, which would hand
/// the stub its own address.
enum InstallDecision {
    KeepFilled,
    SkipOwnFile,
    NoTarget,
    Write(usize),
}

fn install_decision(
    injected_module_name: &str,
    host_module: &str,
    cell_value: usize,
    host_is_own_image: bool,
    resolved: usize,
) -> InstallDecision {
    if cell_value != 0 {
        return InstallDecision::KeepFilled;
    }

    // `host_is_own_image` is the same rule read off the handle instead of off the name: nothing was
    // renamed to match, and the module list still answered the export out of this image.
    if reads_the_mods_own_file(injected_module_name, host_module) || host_is_own_image {
        return InstallDecision::SkipOwnFile;
    }

    if resolved == 0 {
        return InstallDecision::NoTarget;
    }

    InstallDecision::Write(resolved)
}

/// The export out of a module the process already has. The `CString` is one small allocation per
/// export on a path that runs once, at attach; nothing on a call path builds one.
fn export_address(name: &str, handle: HMODULE) -> usize {
    match CString::new(name) {
        Ok(name) => utils::get_proc_address(handle, &name),
        Err(_) => 0,
    }
}

/// Fill every export cell that is still empty from the module that hosts the export, and report what
/// could not be filled. Runs once, at attach, for every name the mod is loaded under: an export the
/// link advertises is reachable whatever the file is called, so an unknown module name is not a
/// reason to leave the cells the stubs read unread.
pub fn install(injected_module_name: &str) -> Census {
    let mut census = Census::new();
    let own_module = unsafe { DLL_HMODULE }.0 as usize;

    // One lookup per host module, not per export: both tables name two hosts between them, and this
    // runs under the loader lock.
    let mut hosts: Vec<(&str, Option<HMODULE>)> = Vec::new();

    for entry in exports() {
        let cell = entry.cell as *mut usize;
        let cell_value = unsafe { *cell };

        let host = match hosts.iter().find(|(host_module, _)| *host_module == entry.module) {
            Some((_, handle)) => *handle,
            None => {
                let handle = loaded_host_module(entry.module);
                hosts.push((entry.module, handle));
                handle
            }
        };

        let host_is_own_image = host.is_some_and(|handle| handle.0 as usize == own_module);
        let resolved = match host {
            Some(handle) if !host_is_own_image => export_address(entry.name, handle),
            _ => 0,
        };

        match install_decision(injected_module_name, entry.module, cell_value, host_is_own_image, resolved) {
            InstallDecision::KeepFilled => census.forwarded_by_proxy_init += 1,
            InstallDecision::SkipOwnFile => census.own_file.push(entry.name),
            InstallDecision::NoTarget => census.unfilled.push(entry.name),
            InstallDecision::Write(addr) => {
                unsafe { *cell = addr };
                census.filled_now += 1;
            }
        }
    }

    info!("{}", census_report(injected_module_name, &census));
    census
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{core::w, Win32::System::LibraryLoader::LoadLibraryW};

    proxy_proc!(C1ProbeUnfilled, C1_PROBE_UNFILLED);
    proxy_proc!(C1ProbeFilled, C1_PROBE_FILLED);
    proxy_proc!(C1ProbeResolved, C1_PROBE_RESOLVED);

    extern "C" {
        fn C1ProbeUnfilled() -> usize;
        fn C1ProbeFilled(a: usize, b: usize, c: usize, d: usize, e: usize) -> usize;
        fn C1ProbeResolved() -> usize;
    }

    extern "C" fn c1_probe_target(a: usize, b: usize, c: usize, d: usize, e: usize) -> usize {
        // Five arguments: the four register arguments and the first stack argument. A forwarded call
        // has to arrive with all five where the caller put them, which is what a stub that jumped
        // through 0 would have lost.
        a + b * 2 + c * 3 + d * 4 + e * 5
    }

    #[test]
    fn an_export_stub_with_an_unfilled_cell_answers_the_call_instead_of_executing_address_0() {
        unsafe { C1_PROBE_UNFILLED = 0 };

        let answered = unsafe { C1ProbeUnfilled() };
        assert_eq!(answered, 0, "the stub has to answer the caller, not jump through its cell");
        assert!(refusal_count() >= 1, "the refusal has to be counted");
    }

    #[test]
    fn an_export_stub_with_a_filled_cell_hands_the_call_to_the_target_it_names() {
        let target: extern "C" fn(usize, usize, usize, usize, usize) -> usize = c1_probe_target;
        unsafe { C1_PROBE_FILLED = target as usize };

        assert_eq!(unsafe { C1ProbeFilled(1, 2, 3, 4, 5) }, 1 + 2 * 2 + 3 * 3 + 4 * 4 + 5 * 5);
    }

    #[test]
    fn a_proxy_export_is_never_read_out_of_the_mods_own_file() {
        // Reading `winhttp.dll` while this image *is* `winhttp.dll` hands the stub its own address.
        assert!(reads_the_mods_own_file("winhttp.dll", "winhttp.dll"));
        assert!(reads_the_mods_own_file("WINHTTP.DLL", "winhttp.dll"));
        assert!(reads_the_mods_own_file("unityplayer.dll", "UnityPlayer.dll"));

        // The name this fork ships under hosts neither proxy, so it blocks nothing.
        assert!(!reads_the_mods_own_file("cri_mana_vpx.dll", "winhttp.dll"));
        assert!(!reads_the_mods_own_file("cri_mana_vpx.dll", "UnityPlayer.dll"));
    }

    #[test]
    fn a_cell_a_proxy_init_already_filled_is_never_rewritten() {
        // The byte-for-byte claim for the two proxy deployments: the target `unityplayer::init` or
        // `winhttp::init` resolved stays the target, whatever a lookup would have answered.
        assert!(matches!(
            install_decision("winhttp.dll", "winhttp.dll", 0x1000, true, 0x2000),
            InstallDecision::KeepFilled
        ));
        assert!(matches!(
            install_decision("unityplayer.dll", "UnityPlayer.dll", 0x1000, true, 0x2000),
            InstallDecision::KeepFilled
        ));
        assert!(matches!(
            install_decision("cri_mana_vpx.dll", "winhttp.dll", 0x1000, false, 0x2000),
            InstallDecision::KeepFilled
        ));
    }

    #[test]
    fn a_cell_is_never_filled_from_the_image_the_mod_itself_is() {
        assert!(matches!(
            install_decision("winhttp.dll", "winhttp.dll", 0, false, 0x2000),
            InstallDecision::SkipOwnFile
        ));
        assert!(matches!(
            install_decision("cri_mana_vpx.dll", "UnityPlayer.dll", 0, true, 0x2000),
            InstallDecision::SkipOwnFile
        ));

        // The name this fork ships under blocks nothing, a target resolved out of a module the
        // process already has is written, and nothing resolved means nothing written: the cell stays
        // empty and the stub answers instead of jumping.
        assert!(matches!(
            install_decision("cri_mana_vpx.dll", "winhttp.dll", 0, false, 0x2000),
            InstallDecision::Write(0x2000)
        ));
        assert!(matches!(
            install_decision("cri_mana_vpx.dll", "UnityPlayer.dll", 0, false, 0),
            InstallDecision::NoTarget
        ));
    }

    #[test]
    fn a_refusal_is_named_for_the_first_eight_and_counted_after() {
        for trip in 1..=REFUSAL_LOG_LIMIT {
            let line = refusal_line("WinHttpOpen", "winhttp.dll", trip).expect("the first refusals are named");
            assert!(line.contains("WinHttpOpen"));
            assert!(line.contains("winhttp.dll"));
            assert!(line.contains("refused"));
        }

        assert!(refusal_line("WinHttpOpen", "winhttp.dll", REFUSAL_LOG_LIMIT + 1).is_none());
        assert!(refusal_line("WinHttpOpen", "winhttp.dll", REFUSAL_TOTAL_PERIOD - 1).is_none());

        let total = refusal_line("WinHttpOpen", "winhttp.dll", REFUSAL_TOTAL_PERIOD).expect("a periodic total is said");
        assert!(total.contains("4096"));
    }

    #[test]
    fn the_census_line_names_every_export_it_left_without_a_target() {
        let census = Census {
            forwarded_by_proxy_init: 2,
            filled_now: 44,
            own_file: Vec::new(),
            unfilled: vec!["UnityMain2"],
        };

        let line = census_report("cri_mana_vpx.dll", &census);
        assert!(line.contains("cri_mana_vpx.dll"));
        assert!(line.contains("47 stub(s)"));
        assert!(line.contains("44 filled from the loaded modules"));
        assert!(line.contains("1 with no module loaded to read (UnityMain2)"));

        // An export left alone for naming this file is named too: under a `winhttp.dll` deployment
        // that is 45 of them, and a run has to see which population it is looking at.
        let own = Census {
            forwarded_by_proxy_init: 0,
            filled_now: 44,
            own_file: vec!["WinHttpOpen", "WinHttpOpenRequest"],
            unfilled: Vec::new(),
        };
        let line = census_report("winhttp.dll", &own);
        assert!(line.contains("2 left alone because they name this file (WinHttpOpen, WinHttpOpenRequest)"));
        assert!(!line.contains("no module loaded"));
    }

    #[test]
    fn the_registry_covers_exactly_the_names_the_link_advertises() {
        // `exports.def` is what the linker exports; the registry is what this file can resolve and
        // name. A name in one and not the other is the C1 gap coming back.
        let advertised: Vec<&str> = include_str!("exports.def")
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with(';') && *line != "EXPORTS")
            .collect();
        let registered: Vec<&str> = exports().map(|entry| entry.name).collect();

        assert_eq!(advertised.len(), 47);
        for name in &advertised {
            assert!(registered.contains(name), "{} is exported but not registered", name);
        }
        for name in &registered {
            assert!(advertised.contains(name), "{} is registered but not exported", name);
        }
    }

    #[test]
    fn every_registered_export_names_the_cell_its_stub_reads() {
        // The cell is what the asm reads; a registry entry pointing at some other cell would name
        // the wrong export on the cold path and write the wrong cell on the install path.
        for entry in exports() {
            let cell = entry.cell as *mut usize;
            unsafe { *cell = 0 };
            assert_eq!(export_for_cell(entry.cell).map(|found| found.name), Some(entry.name));
        }
    }

    /// The whole `install` pass, in a real process, under the name this fork deploys under.
    ///
    /// This is the half the unit tests above could not answer: they drive the decision and the line,
    /// not the loader calls. `winhttp::init` never runs for `cri_mana_vpx.dll`, so every cell starts
    /// empty here exactly as it does on a `cri_mana_vpx.dll` launch, and the only thing that can
    /// answer a `WinHttp*` export is a `winhttp.dll` the process already has - which this test pulls
    /// in the way the game's own loader would, before the pass runs. `UnityPlayer.dll` is what the
    /// game process does have loaded at attach and a cargo test host does not, so the two Unity stubs
    /// are the population that stays inert.
    #[test]
    fn the_install_pass_runs_for_this_forks_module_name_and_fills_only_what_a_loaded_host_answers() {
        // Every cell empty first: the pass is being read as a first launch, not as a second one.
        for entry in exports() {
            unsafe { *(entry.cell as *mut usize) = 0 };
        }
        unsafe { LoadLibraryW(w!("winhttp.dll")).expect("winhttp.dll is a Windows system module") };

        let census = install("cri_mana_vpx.dll");

        assert_eq!(census.total(), 47, "every export exports.def advertises is accounted for");
        assert_eq!(census.forwarded_by_proxy_init, 0, "this name runs no proxy init of its own");
        assert_eq!(census.filled_now, 45, "the winhttp population comes out of the loaded winhttp.dll");
        assert!(census.own_file.is_empty(), "cri_mana_vpx.dll is neither host, so it blocks nothing");
        assert_eq!(census.unfilled, vec!["UnityMain", "UnityMain2"], "no UnityPlayer.dll in a test host");

        let line = census_report("cri_mana_vpx.dll", &census);
        assert!(line.contains("47 stub(s)"), "the line a run reads: {line}");
        assert!(line.contains("45 filled from the loaded modules"), "the line a run reads: {line}");
        assert!(line.contains("2 with no module loaded to read (UnityMain, UnityMain2)"), "the line a run reads: {line}");

        // What the pass wrote is a callable address out of the module it named, not a number it made
        // up: a cell filled the same way forwards to the real system function.
        let host = loaded_host_module("winhttp.dll").expect("loaded above");
        let check_platform = export_address("WinHttpCheckPlatform", host);
        assert_ne!(check_platform, 0, "winhttp.dll answers the export name the table declares");

        unsafe { C1_PROBE_RESOLVED = check_platform };
        assert_ne!(unsafe { C1ProbeResolved() }, 0, "the resolved address answers WinHttpCheckPlatform");

        // And the inert half is still inert: a cell with nothing to read is answered, not jumped.
        unsafe { C1_PROBE_RESOLVED = 0 };
        assert_eq!(unsafe { C1ProbeResolved() }, 0, "an export with no host module answers 0");
    }

    /// The other two deployments keep doing what their own `init` did. A file named `winhttp.dll`
    /// must not have its own `WinHttp*` names read back out of itself - that hands a stub its own
    /// address - so the whole population is left alone, filled by `winhttp::init` or inert.
    #[test]
    fn a_deployment_named_like_a_host_reads_none_of_that_hosts_exports() {
        for entry in exports() {
            unsafe { *(entry.cell as *mut usize) = 0 };
        }

        let census = install("winhttp.dll");

        assert_eq!(census.total(), 47);
        assert_eq!(census.forwarded_by_proxy_init, 0, "winhttp::init did not run in this test");
        assert_eq!(census.filled_now, 0, "nothing is written out of the image the mod itself is");
        assert_eq!(census.own_file.len(), 45, "the whole winhttp population names this file");
        assert_eq!(census.unfilled, vec!["UnityMain", "UnityMain2"]);
    }
}
