pub mod types;
pub mod api;
pub mod symbols;
pub mod hook;
pub mod introspect;
pub mod utils;
pub mod ext;
pub mod sql;

// The libunity `Il2CppApi` slot table exists only because the Android build ships a hollowed
// `libil2cpp.so`; every consumer of it (`symbols::dlsym`, `symbols::recheck`) is already behind
// the same cfg, so `cargo check` and the Windows deliverable still carry none of these 234
// pinned offsets. `test` is in the gate because the module's reproductions (the per name mismatch
// counter, the never re-walk a rejected table regression) are host tests: gated on android alone
// nothing runs them, the Windows `unit_tests` job cannot compile them and a cross target cannot
// run its own test binary. Off Android the module stays inert: `find_libunity_base` returns None
// under `cfg(not(unix))`, so no probe reads any address there.
#[cfg(any(target_os = "android", test))]
pub mod slot_table;
#[cfg(any(target_os = "android", test))]
mod slot_table_generated;