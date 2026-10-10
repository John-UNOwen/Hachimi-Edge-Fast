use std::path::Path;
use std::process::{Command, Output};

// C2: the structured-exception half of the detour barrier, il2cpp::hook::guard.
//
// `__try` and `__except` cannot be spelled in Rust, and a detour body that reads game memory can
// take a fault (a null `this`, a freed Il2CppObject, an index past an Il2CppArray). With no handler
// frame the fault unwinds through the trampoline - generated machine code with no entry in any
// module's .pdata, so RtlVirtualUnwind has nothing to consult - and on into the game's own frame
// loop, which ends the process. This frame is what stops it.
//
// Rust panics never reach here: the barrier runs catch_unwind inside the closure this frame calls,
// so a panic is taken by Rust, where destructors and the panic bookkeeping behave normally. This
// frame only ever sees a fault.
//
// It is kept as text inside a tracked file instead of in a src/windows/hook_guard.c: the barrier is
// the part of the hook change set that a *new* file carries, and git only commits what it tracks.
// `git add -u` and `git commit -am` skip an untracked source without saying so, and the tree they
// produce is one where build.rs names a C file that is not there. Written into OUT_DIR, which git
// never looks at, the frame travels with the change set that needs it.
const HOOK_GUARD_C: &str = r#"
#include <stdint.h>

#define WIN32_LEAN_AND_MEAN
#include <windows.h>

#define GUARD_RAN 0
#define GUARD_FAULTED 1

typedef void (*PROC_EXECUTOR)(void* Proc);

int32_t hachimi_guard_run(
    PROC_EXECUTOR Executor,
    void* Proc,
    uint32_t* Code,
    void** Address
) {
    __try
    {
        Executor(Proc);
        return GUARD_RAN;
    }
    __except (
        *Code = GetExceptionCode(),
        *Address = GetExceptionInformation()->ExceptionRecord->ExceptionAddress,
        EXCEPTION_EXECUTE_HANDLER
    )
    {
        return GUARD_FAULTED;
    }
}
"#;

fn setup_windows_build() {
    // Link proxy export defs
    let absolute_path = std::fs::canonicalize("src/windows/proxy/exports.def").unwrap();
    if std::env::var("CARGO_CFG_TARGET_ENV").unwrap() == "msvc" {
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", absolute_path.display());

        // The frame is text in this file, so it has to be written out before cc sees it, and a
        // change to this file is what re-runs the build. No debug info: cc's default `-Z7` trips
        // cl on this toolchain (D8050) and an exception frame has nothing to debug.
        let guard_source = Path::new(&std::env::var("OUT_DIR").unwrap()).join("hook_guard.c");
        std::fs::write(&guard_source, HOOK_GUARD_C).unwrap();
        println!("cargo:rerun-if-changed=build.rs");

        cc::Build::new()
            .file(&guard_source)
            .debug(false)
            .compile("hachimi_hook_guard");
    } else {
        // I have to remove the '/DEF:' every time I cross compile on linux, so might as well do this
        println!("cargo:rustc-cdylib-link-arg={}", absolute_path.display());
    }

    // Generate and link version information
    let res = tauri_winres::WindowsResource::new();
    res.compile().unwrap();
}

fn command_output_to_string(output: Output) -> String {
    String::from_utf8(output.stdout).expect("valid utf-8 from command output")
}

fn execute_command(command: &mut Command) -> Option<Output> {
    let output = command.output().ok()?;
    if !output.status.success() { return None; }
    Some(output)
}

fn setup_version_env() {
    let mut version_str = "v".to_owned() + env!("CARGO_PKG_VERSION");

    if execute_command(Command::new("git").args(["--version"])).is_some() {
        if let Some(output) = execute_command(Command::new("git").args(["rev-parse", "--short", "HEAD"])) {
            version_str.push('-');
            let output_str = command_output_to_string(output);
            version_str.push_str(&output_str[..output_str.len()-1]); // remove \n
        }
        else {
            println!("cargo:warning=Failed to retrieve git commit hash");
        }

        if let Some(output) = execute_command(Command::new("git").args(["status", "--porcelain"])) {
            if !output.stdout.is_empty() && std::env::var("HACHIMI_IGNORE_DIRTY").is_err() {
                version_str.push_str("-dirty");
            }
        }
        else {
            println!("cargo:warning=Failed to retrieve git repo status");
        }

        if let Some(output) = execute_command(Command::new("git").args(["rev-parse", "--git-dir"])) {
            println!("cargo:rerun-if-changed={}", command_output_to_string(output));
        }
        else {
            println!("cargo:warning=Failed to retrieve git directory");
        }
    }
    else {
        println!("cargo:warning=Failed to execute git. Is git installed?");
    }

    println!("cargo:rustc-env=HACHIMI_DISPLAY_VERSION={}", version_str);
}

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os == "windows" {
        setup_windows_build();
    } else if target_os == "android" {
        println!("cargo:rustc-link-arg=-Wl,-z,max-page-size=16384");
        println!("cargo:rustc-link-arg=-Wl,-z,common-page-size=16384");
    }

    setup_version_env();
}
