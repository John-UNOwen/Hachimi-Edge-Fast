# Dev Tools
These are the tools which can be used when developing this project.

Unless stated otherwise, they're meant to be run in the project's root directory. See each platform's README for more info.

## Audits that back a claim in `knowledge/defects/`

| Tool | What it prints |
|---|---|
| `python tools/scan_hook_barriers.py` | For every hook boundary `new_hook!` arms, which barrier macro or guard call stands on it and which answer that arm gives a `Panicked` trip, with the totals adding up to the population it walked. Walks the same arming population once more for the `disabled_hooks` key (C27), reading the rule out of `src/il2cpp/hook/mod.rs` instead of keeping its own copy of it: the id expression `new_hook!` builds and the prefix `hook_id` cuts, each printed with the line it was read at, every comparison the decision function makes between a configured key and a hook id, the second copy of the same rule that the report after hooking runs on and whether it agrees with the arming decision, the population every bare wrapper name in the tree reaches under those comparisons against the population `disabled_hooks.contains(stringify!($hook))` reached, and both fixtures the Rust tests are built from (`COLLIDING_KEYS`, `SINGLE_NAME_KEYS`) compared with the population the tree arms now. A comparison it cannot read is a row, not a silence: a rule rewritten out of this tool's model fails the audit. It attests the decision and the population, never the behaviour - `cargo test --lib` is what calls `hook_is_disabled`. Ends with the committability section: what `git status --porcelain -uall` reports for this change set and whether a `git add -u` or `git commit -am` commit would silently drop a file the build or an audit needs. Exit 1 while it would, while two arming sites share one key, while the shipped rule reaches a different population than the rule it replaced, while the decision in `hook/mod.rs` is written in a shape the audit cannot read, or while a test's fixture no longer matches the tree. |
| `python tools/scan_extern_c_mutex.py src --transitive` | Every `extern "C"` frame under `src/` (hand written, `def_detour!`, or written by a helper macro), split shipped / `#[cfg(test)]`, and the `unwrap()` / `expect()` calls on a shared lock written inside one. `--transitive` adds the frames that reach such an acquirer through a same-crate helper. |

Both live here, in `tools/`, and both belong in the change set they audit. `target/` is gitignored, so a
script kept under `target/scratch/` is deleted by `cargo clean` and by `git clean -xdf`, and a script that
was never put in git's index is dropped by `git add -u` and by `git commit -am` without either command
saying so. A count quoted from a tool that a commit can lose is not reproducible evidence, so these two
travel with the hooks they check and the barrier audit checks that for them.
