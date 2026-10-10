# Dev Tools
These are the tools which can be used when developing this project.

Unless stated otherwise, they're meant to be run in the project's root directory. See each platform's README for more info.

## Audits that back a claim in `knowledge/defects/`

| Tool | What it prints |
|---|---|
| `python tools/scan_hook_barriers.py` | For every hook boundary `new_hook!` arms, which barrier macro or guard call stands on it and which answer that arm gives a `Panicked` trip, with the totals adding up to the population it walked. Ends with the committability section: what `git status --porcelain -uall` reports for this change set and whether a `git add -u` or `git commit -am` commit would silently drop a file the build or an audit needs. Exit 1 while it would. |
| `python tools/scan_extern_c_mutex.py src --transitive` | Every `extern "C"` frame under `src/` (hand written, `def_detour!`, or written by a helper macro), split shipped / `#[cfg(test)]`, and the `unwrap()` / `expect()` calls on a shared lock written inside one. `--transitive` adds the frames that reach such an acquirer through a same-crate helper. |

Both live here, in `tools/`, and both belong in the change set they audit. `target/` is gitignored, so a
script kept under `target/scratch/` is deleted by `cargo clean` and by `git clean -xdf`, and a script that
was never put in git's index is dropped by `git add -u` and by `git commit -am` without either command
saying so. A count quoted from a tool that a commit can lose is not reproducible evidence, so these two
travel with the hooks they check and the barrier audit checks that for them.
