# AGENTS.md

Guide for coding agents working on this fork of Hachimi Edge. Read this before changing code,
then read [LEDGER.md](LEDGER.md), which is the fork's working memory: measured baselines, open
items with IDs (A1 to A27, C1 to C46, E1 to E3), the fix order, the run notes, and section F for
releases.

## 1. What this fork is for

Hachimi Edge is a Rust `cdylib` injected into Umamusume Pretty Derby (Unity 2022.3 IL2CPP). It
detours IL2CPP methods, Unity icalls and a few native functions to add translation, graphics and
quality of life options.

**This fork's goal is to make the game take less wall clock time and cost less to run**, without
breaking the game, the account, or the upstream feature set. In priority order:

1. **Less time spent waiting.** Shorter transitions, result screens, story playback and loading,
   driven through the game's own timing values and settings.
2. **Less overhead from the mod itself.** Startup time inside the loader, per frame cost of
   detours, allocations, locks and logging on hot paths.
3. **Never trade correctness for speed.** A faster build that crashes, desyncs a race, corrupts
   a save, or skips a server round trip is a regression.

Primary target: **Windows, Steam, Global client** (deployed as `cri_mana_vpx.dll` at the game
root, see C4). Android must still compile (CI runs clippy for `aarch64-linux-android`), but is not
where performance work is measured.

## 2. Rules that override everything else

- **Measure, don't feel.** Every speed claim is checked against the baselines in LEDGER.md
  (section A: transition gaps median ~1.0 s, screens 91–95% of a session, hook arming 4.68 s
  before batching). A change that cannot be shown in `hachimi.log` is not done.
- **Neutral defaults.** Every speed option defaults to `1.0` / `false` and must do *nothing at
  all* at its default. Users opt in.
- **Clamp in code, not just in the GUI.** Sliders allow 0.1..=1000.0. Hard ceilings live in code
  (`MAX_FACTOR = 20.0`, `MAX_TIME_SCALE = 5.0` in `AnimationSpeed.rs`). C5, C22 and C24 are what
  happens when they don't.
- **Client side presentation only.** Do not speed things up by skipping server calls, faking
  success callbacks, changing simulation results, or touching purchase, legality, SQLite key or
  network paths (see C3, C6, C12, C20, C21, C31). `Time.timeScale` is a simulation lever, not an
  animation lever: prefer duration, tween and setting hooks.
- **Never call through address 0.** Unresolved targets must stay inert.
- **Don't change behaviour you weren't asked to.** Upstream features (translation, GUI, Live,
  race HUD, free camera) are kept working; this fork merges upstream regularly (section E).

## 3. Repository map

| Path | What lives there |
|---|---|
| `src/lib.rs` | crate root, platform switch |
| `src/core/hachimi.rs` | `Hachimi` singleton, `Config` struct and every `default_*` fn |
| `src/core/interceptor.rs` | hook registry, `begin_batch`/`finish_batch`, `get_orig_fn!` |
| `src/core/gui.rs` | egui overlay and Config Editor (Performance tab ~line 5043), ~9.4k lines |
| `src/core/{tl_repo,updater,http,ipc}.rs` | translation repo sync, updater, IPC plane |
| `src/il2cpp/symbols.rs` | class/method/field resolution, `get_method_overload` (A2, A7) |
| `src/il2cpp/introspect.rs` | `debug_mode` dump to `introspect.log` (truncates at 500 classes, A8) |
| `src/il2cpp/hook/mod.rs` | hook macros and the ordered `init()` with batch arming |
| `src/il2cpp/hook/<Assembly>/<Class>.rs` | one file per hooked game class, named as in the game |
| `src/il2cpp/hook/umamusume/AnimationSpeed.rs` | **core of the speed work**: groups, factors, safe resolution |
| `src/il2cpp/hook/umamusume/{NowLoading,SingleModeResultContentBase,StoryTimelineController,StoryViewController,HighSpeedSetting,StoryFrameProbe}.rs` | speed hooks and probes |
| `src/il2cpp/hook/DOTween/TweenManager.rs` | `ui_animation_scale` |
| `src/il2cpp/hook/UnityEngine_CoreModule/Time.rs` | `time_scale` via the `set_timeScale` icall |
| `src/il2cpp/hook/umamusume/GameSystem.rs` | game thread tick, where deferred `apply_if_dirty()` runs |
| `src/windows/` | DLL entry, proxy exports, D3D11 overlay, window hooks, MinHook backend |
| `src/android/` | Dobby backend, Zygisk, GL overlay |
| `assets/locales/*.yml` | 10 locales (en, es, fil, id, ko, pt-br, ru, vi, zh-cn, zh-tw) |
| `tools/gen_il2cpp_slot_table.py` | generates `src/il2cpp/slot_table_generated.rs` (Android) |

## 4. Build and check

```bash
cargo check
cargo build --release
cargo clippy --target x86_64-pc-windows-msvc -- -D warnings
cargo clippy --target aarch64-linux-android --all-targets -- -D warnings
cargo test --lib
```

- Release profile is `opt-level=3`, fat LTO, `codegen-units=1`, stripped. Don't loosen it.
- Clippy: everything is allowed except `clippy::perf`, which is **deny**. Treat a perf lint as a
  real bug, never `#[allow]` it away. clippy is installed for the stable toolchain here
  (`cargo clippy --version`), so run both legs locally instead of recording them as unrunnable.
- CI (`.github/workflows/clippy_check.yml`) runs clippy with `-D warnings` for both Windows and
  `aarch64-linux-android`, and `cargo test --lib` on a Windows runner. Platform specific code must
  sit behind `#[cfg(target_os = ...)]`. `src/android/` is linted only by the Android leg, and
  `slot_table` / `slot_table_generated` are gated `cfg(any(target_os = "android", test))`: a Windows
  `--all-targets` run lints them and `cargo test --lib` runs their tests, while Windows `check`,
  `clippy` without `--all-targets`, and `build --release` never compile them (C43).
- Android builds use `tools/android/build.sh` (needs `ANDROID_NDK_ROOT`). For the Android clippy leg on
  a Windows host, the tool names CI writes into the environment are the *Linux* ones: in a Windows NDK
  copy `aarch64-linux-android24-clang` and `llvm-ar` are bash wrapper scripts Windows cannot exec, and
  cc-rs dies in `ring` and `blake3` with `os error 193` before the crate is reached. Use the runnable
  names from `toolchains/llvm/prebuilt/windows-x86_64/bin`, either
  `CC/CXX_aarch64_linux_android = aarch64-linux-android24-clang(.cmd)`, `AR_aarch64_linux_android =
  llvm-ar.exe`, `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` the same, or `clang.exe` with
  `CFLAGS_aarch64_linux_android = --target=aarch64-linux-android24`. Keep `TMP`/`TEMP`/`TMPDIR` on a
  writable path inside the workspace (`ring` preprocesses its `.S` files through a temp file). clippy
  emits metadata only, so the ELF link args never run there. `cargo build --target aarch64-linux-android`
  does link on this Windows host once `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` names a runnable clang
  wrapper, and writes a real AArch64 `libhachimi.so`; with no linker named it stops at `error: linker
  ``cc`` not found`, and CI's `-static-libstdc++` link args only draw a clang "argument unused" warning
  (C42).
- Do not commit build output or scratch captures (`cargo_check_out.txt` is an example of one).

The hook modules carry unit tests (`#[cfg(test)] mod tests` in `Time.rs`, `AnimationSpeed.rs`,
`HighSpeedSetting.rs`, `StoryTimelineController.rs`, and in `slot_table.rs`, whose modules are gated
`cfg(any(target_os = "android", test))` so the host run compiles them and the shipped Windows build
does not). They drive the functions
the shipped hooks call: the plan a pass makes, its applied markers, the clamps, the counters and
their log lines. They cannot reach the game, so they prove a decision and never a cost - no il2cpp
field read or write, no trampoline, and no `Hachimi::instance()`, which ends the process when it is
asked for before init. Whether a hook the game actually reached, and what the mod costs, comes from
a game run and the log (section 7). Cite the test *and* the run line when you close an item.

## 5. How a hook is built here

```rust
type FooFn = extern "C" fn(this: *mut Il2CppObject, duration: f32);
extern "C" fn Foo(this: *mut Il2CppObject, duration: f32) {
    let duration = AnimationSpeed::scale_duration(duration, Group::Screens);
    get_orig_fn!(Foo, FooFn)(this, duration);
}

pub fn init(image: *const Il2CppImage) {
    get_class_or_return!(image, "Gallop", SomeClass);
    let Foo_addr = unsafe { AnimationSpeed::resolve_method(SomeClass, "Foo", &[Il2CppTypeEnum_IL2CPP_TYPE_R4], Il2CppTypeEnum_IL2CPP_TYPE_VOID) };
    new_hook!(Foo_addr, Foo);
}
```

Rules learned the hard way (each has a ledger entry):

- **Resolve by signature, not by name plus arity.** Use `AnimationSpeed::resolve_method`
  (parameter enums *and* return type), `resolve_ref_method` for `ref`/`out` parameters,
  `resolve_getter` for getters. `get_method_addr(class, name, argc)` is legacy (C7).
  `CLASS` matches every reference type, so distinct overloads can still collide (A2).
- **Static methods have no `this`.** A wrapper with a `this` parameter misreads every argument
  of a static target. `resolve_method` rejects statics; write an argument only wrapper instead (A3).
- **Struct parameters:** only pass through types confirmed safe in the dump. A 4 byte enum
  travels in a general register (A5); larger structs do not.
- **Only scale the argument that is a duration.** `PlayFadeNowLoading(to, from, duration)` once
  had all three scaled, turning a fade to alpha 1 into 0.05 (A1).
- **Never scale only the getter of a settable property.** Read, modify, write loops compound
  geometrically. `resolve_getter` refuses when a matching setter exists.
- **Frame counts stay ≥ 1**, time scales only go up and are capped (`scale_frame_count`,
  `scale_time_scale`).
- **Be idempotent.** Remember the original value and write `original / factor`. Never multiply
  the current value (C22, C24). Use an "applied" marker when an asset can be processed twice.
- **Write on the game thread.** The GUI sets a `DIRTY` flag (`mark_dirty`); the write happens in
  `GameSystem_Update` via `apply_if_dirty()`. Don't call Unity natives from the overlay thread.
- **`get_orig_fn!` returns 0 for an uninstalled hook** (C1). Only call it from inside the detour
  it belongs to; a wrapper reachable from mod code should use `def_method_wrapper_fn!`, which
  guards against 0.
- **`disabled_hooks` keys on the bare wrapper name**, which is not unique (C27). Prefer unique
  wrapper names for new hooks.
- New speed groups are installed **last** in `umamusume::init`.
- Names come from the live client. Confirm them in `introspect.log` (`debug_mode: true`) before
  writing a hook, and remember the dump is truncated (A8): absence there proves nothing.

## 6. Making the mod itself cheaper

Hot paths run every frame or every tween tick. In them:

- **No `config.load()` per call.** Mirror the value into an atomic when the config changes, the
  way `AnimationSpeed::factor()` does (`AtomicU32` holding `f32` bits). Known offenders to fix:
  `DOTween/TweenManager.rs` `Update` (runs every tween tick), `CySpringController.rs`,
  `Texture.rs`, `Text.rs`, `TextMesh.rs`, `TextCommon.rs`. There are ~250 `config.load()` sites;
  only the per frame ones matter.
- **No allocation, formatting, `Mutex` or logging per call.** Use the "first N calls" logging
  pattern (`StoryFrameProbe`, first hit logger) and periodic totals instead.
- **No `unwrap()` on shared mutexes in detours.** A poisoned lock turns every later call into a
  panic across FFI (C2). Use `unwrap_or_else(|e| e.into_inner())` or bail out to the original.
- **Fast exit at neutral settings.** `if factor == 1.0 { return orig(...) }` before any other work.
- **Startup:** hooks are created inside `begin_batch()` and armed in one `MH_EnableHook(MH_ALL_HOOKS)`
  pass by `finish_batch()`. Nothing between those two calls may call through a trampoline.
  Remaining startup cost sits under the loader lock (C25): the introspect dump, native sqlite
  hooking, window work, the Discord pipe and the update check (C26). Moving work out of `DllMain`
  to a deferred thread or first game tick is a valid optimisation; keep anything that must precede
  game code where it is.
- Prefer `fnv`/`FnvHashMap`, `once_cell::Lazy`, `arc_swap` (already dependencies) over adding crates.

## 7. Measuring a change

1. Build release, deploy, enable `enable_file_logging` (and `debug_mode` for probes/dump).
2. Play a comparable session (career run: training turns, races, result screens, story).
3. Read `hachimi.log`:
   - `Config snapshot:` the settings the run actually used (config.json is rewritten on exit).
   - `Hooking finished: N hooks armed in one pass, S s` against 4.68 s before batching.
   - `_addr is null`, `has no overload`, `is static` lines: hooks that did not install.
   - Per hook first hit lines (`X 0.16 -> 0.008`): which hooks the game actually reached.
     **Installed is not the same as called** (A4); a hook with no call line did nothing.
   - `NowLoading::PlayFadeNowLoading` pairs: transition gaps; compare min/median/max/total.
   - `AnimationSpeed apply pass N: a config reads, b entry locks, c table passes, d field reads,
     e field writes`: what the speed pass cost the run, counted by the pass itself (C36, C41).
     Field reads stay 0 while `init` resolves no duration field (C13).
   - `Frame probe totals at N s:` for the story stepping search (item D13).
4. Record the run in LEDGER.md in the existing format: build hash, duration, numbers, what
   moved, what didn't.

## 8. Where the remaining time is

Three runs agree: the wipe path is nearly exhausted (residual is asset loading, A9), and **91–95%
of a session is time on screens**. Work that can still move the numbers, roughly in order:

1. **Story stepping (A13, D12, D13).** Find which frame stepping path this client uses
   (`StoryFrameProbe.rs`), then scale it in the right direction. Typewriter and high speed
   constants are compile time literals, so only the methods that read them can be hooked.
2. **Static high speed helpers (A3)** and the `HighSpeedType` enum overload / `PlayFadeFrontCanvas` (A5).
3. **Game owned settings** (`HighSpeedSetting.rs`): raising the game's own skip/high speed
   settings is safer than scaling time. `GetMaxHighSpeedType` is context dependent (A11).
4. **Auto skip** of result screens through the game's own `SkipFadeInTween` path, which is
   confirmed called; one skip per result part is not yet proven, because one flag serialises the
   parts and C38 now counts the requests it drops. Extend to other screens only through the game's
   own skip buttons.
5. **Loading**: asset bundle load latency between scenes (0.3–3.2 s per transition).
6. **Mod overhead** (section 6) and **startup** (C25).

Look for the safety items that sit on speed paths before adding more: C2 (panic barrier), C5
(`independent_time` should not be scaled), C22/C23/C24 (compounding story multipliers).

## 9. Adding or changing an option

A new speed or performance option touches, in one change:

1. `Config` field plus `#[serde(default = ...)]` with a neutral default in `src/core/hachimi.rs`.
2. The `Config snapshot:` line in `src/il2cpp/hook/mod.rs` if it affects timing.
3. The Performance tab in `src/core/gui.rs`, with an in code clamp, not only a slider range.
4. Keys in **all ten** `assets/locales/*.yml` (English text is an acceptable placeholder; never
   leave a key out or define it twice, E1/E2).
5. A LEDGER.md note if it is unverified on the Global client. Options that do nothing on this
   client must not be offered as if they work (C14). JP only options are region gated.

## 10. Conventions

- Commits: `area: lower case imperative summary`, areas `il2cpp`, `core`, `gui`, `l10n`,
  `docs`, `windows`, `android`. One concern per commit; the ledger update is its own `docs:` commit.
- Code style follows the surrounding file: hook files use the game's PascalCase names
  (`src/lib.rs` allows `non_snake_case` crate wide), comments explain *why* a value is safe, not what the line does.
- LEDGER.md status marks: `[x]` fixed and verified, `[~]` partial, `[ ]` open, `[latent]` inert
  on this client. Cite the commit hash when closing an item, and never mark `[x]` without a run. An
  item whose own text says the run is missing is `[~]`, not `[x]`. A ledger self check must match the
  shape a diff prints: `^\+- \[x\]` is a closure the change set awards and `^-- \[x\]` one it takes
  away, while `^- \[x\]` matches the file only.
- Upstream merges: keep our `introspect`/speed modules, take upstream's everything else, rebuild
  with zero warnings, and record the merge in section E.
- Upstream's PR template forbids untested AI generated code. Anything heading upstream must be
  built, run in game and understood by the human submitting it.
