use windows::{core::PCWSTR, Win32::Foundation::HMODULE};

#[link(name = "kernel32")]
extern "C" {
    pub fn LoadLibraryW(filename: PCWSTR) -> HMODULE;
    pub fn ExitProcess(exit_code: u32);
}
