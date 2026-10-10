#!/usr/bin/env python3
"""
C2 scan: every `extern "C"` frame under src/, and the `.unwrap()` / `.expect()` calls on a
shared Mutex (or RwLock) that sit inside one.

Why it reads the tree the way it does:
- A frame is a definition a trampoline or a foreign caller lands on. Three shapes exist in this
  crate: a hand written `extern "C" fn`, a `def_detour!` invocation (the macro's output *is* an
  `extern "C" fn`), and a helper macro that expands to one (`live_*!`, `block_input_button!`,
  `def_getter_hook!`), so all three count as frame sites.
- Comments and string literals are blanked first, keeping every newline and column.
  `core/interceptor.rs:948` names `hook_map.lock().unwrap()` in a *comment* (the shape C33 removed);
  a scan that counted it would be measuring prose, not code.
- Frames inside `#[cfg(test)]` are counted apart from the shipped ones.
- `--transitive` adds the second, honest question, run to a fixed point over the shipped call
  graph: a name is exposed if any of its shipped definitions takes a lock and unwraps the
  PoisonResult, or if it free-calls a name that is exposed. A frame is exposed if it unwraps one in
  its own body or free-calls an exposed name. `test_regions` keeps `#[cfg(test)]` bodies out of the
  graph, so the number measures shipped code, and an edge is a free call only, so a method call on
  some value (`config.load()`) is never mistaken for a call into a crate helper. It over-approximates
  on purpose: it can name a frame a type-aware call graph would clear, never one it should name.

Usage: python tools/scan_extern_c_mutex.py [src_dir] [--transitive]

It lives under `tools/`, not under `target/scratch/`, because `/target` is gitignored: the count that
closes C2's lock half was printed by a file `cargo clean` and `git clean -xdf` both delete, so the
number stayed quotable after the thing that prints it was gone. `tools/scan_hook_barriers.py` prints,
for both scripts, whether a commit can carry them.
"""

import pathlib
import re
import sys

SRC = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 and not sys.argv[1].startswith("--") else "src")

# Macros whose expansion is an `extern "C" fn` wrapper (each verified against its definition).
FRAME_MACROS = [
    "def_detour",
    "def_getter_hook",
    "live_skip_void_frame",
    "live_main_camera_void_frame",
    "live_secondary_camera_void_frame",
    "live_secondary_camera_void_frame_time",
    "block_input_button",
]

# Take a lock, then unwrap the PoisonResult: the C2 shape. `.lock()` / RwLock `.read()` `.write()`.
LOCK_UNWRAP = re.compile(r"\.(lock|read|write)\s*\(\s*\)\s*\.(unwrap|expect)\s*\(")

FRAME_FN = re.compile(r'(?:unsafe\s+)?extern\s+"[^"]*"\s+fn\s+(\w+)')
MACRO_DEF = re.compile(r"macro_rules!\s+(\w+)")
MACRO_INVOCATION = re.compile(r"\b(" + "|".join(FRAME_MACROS) + r")\s*!")
CFG_TEST = re.compile(r"#\[cfg\(test\)\]")
# A call edge in the graph is a free call: a name not sitting behind a `.`. A method call on some
# value cannot name a crate helper (Rust reaches a method through the value's type, which this scan
# cannot see), and counting one is how `config.load()` was reported as a call into a crate `load`.
CALL = re.compile(r"(?<![.\w])([a-z_][A-Za-z0-9_]*)\s*\(")
FN_DEF = re.compile(r"\bfn\s+(\w+)\s*\(")

RAW_STRING = re.compile(r'[brc]*r(#*)"')
# A Rust char literal: exactly one character or one escape. `'static` and friends are lifetimes and
# are deliberately not matched, so they never swallow the code after them.
CHAR_LITERAL = re.compile(r"'(?:[^'\\\n]|\\(?:[^'\n]|u\{[0-9a-fA-F]{1,6}\}))'")


def mask(text: str) -> str:
    """Blank comments and string/byte literals, keeping every newline and every column."""
    out = []
    i, n = 0, len(text)
    block_depth = 0

    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""

        if block_depth:
            if c == "/" and nxt == "*":
                block_depth += 1
                out.append("  ")
                i += 2
            elif c == "*" and nxt == "/":
                block_depth -= 1
                out.append("  ")
                i += 2
            else:
                out.append(c if c == "\n" else " ")
                i += 1
            continue

        if c == "/" and nxt == "/":
            while i < n and text[i] != "\n":
                out.append(" ")
                i += 1
            continue

        if c == "/" and nxt == "*":
            block_depth = 1
            out.append("  ")
            i += 2
            continue

        m = RAW_STRING.match(text, i)
        if m:
            hashes = m.group(1)
            close = '"' + hashes
            found = text.find(close, m.end())
            j = n - 1 if found < 0 else found + len(close) - 1
            out.append(text[i:m.end()])          # keep r#" / br" and the opening quote
            for ch in text[m.end():j + 1]:       # blank the body, keep the closing quote(s)
                out.append(ch if ch in "\n\"" else " ")
            i = j + 1
            continue

        if c == "'":
            lit = CHAR_LITERAL.match(text, i)
            if lit:
                out.append(text[i:lit.end()])
                i = lit.end()
                continue
            out.append(c)
            i += 1
            continue

        if c == '"':
            out.append('"')
            i += 1
            while i < n:
                if text[i] == "\\":
                    out.append(text[i:i + 2])
                    i += 2
                elif text[i] == '"':
                    out.append('"')
                    i += 1
                    break
                else:
                    out.append(text[i] if text[i] == "\n" else " ")
                    i += 1
            continue

        out.append(c)
        i += 1

    return "".join(out)


def balanced(text: str, open_idx: int) -> int:
    """Index of the bracket that closes the group opening at `open_idx`."""
    close_ch = {"{": "}", "(": ")", "[": "]"}[text[open_idx]]
    open_ch = text[open_idx]
    depth = 0

    for i in range(open_idx, len(text)):
        if text[i] == open_ch:
            depth += 1
        elif text[i] == close_ch:
            depth -= 1
            if depth == 0:
                return i

    return len(text) - 1


def line_of(text: str, idx: int) -> int:
    return text.count("\n", 0, idx) + 1


def frame_regions(text: str):
    """(start, end, label, kind) for every frame site in this file."""
    regions = []

    for m in FRAME_FN.finditer(text):
        brace = text.find("{", m.end())
        if brace < 0:
            continue
        regions.append((m.start(), balanced(text, brace), m.group(1), "extern \"C\" fn"))

    for m in MACRO_INVOCATION.finditer(text):
        i = m.end()
        while i < len(text) and text[i] in " \t\n":
            i += 1
        if i < len(text) and text[i] in "{([":
            regions.append((m.start(), balanced(text, i), m.group(1) + "! <site>", m.group(1) + "!"))

    for m in MACRO_DEF.finditer(text):
        brace = text.find("{", m.end())
        if brace < 0:
            continue
        end = balanced(text, brace)
        if re.search(r'extern\s+"[^"]*"\s+fn', text[brace:end + 1]):
            regions.append((m.start(), end, m.group(1) + " (macro body)", "macro_rules!"))

    return regions


def test_regions(text: str):
    out = []
    for m in CFG_TEST.finditer(text):
        mod = re.compile(r"mod\s+\w+").search(text, m.end(), m.end() + 80)
        if not mod:
            continue
        brace = text.find("{", mod.end())
        if brace < 0:
            continue
        out.append((mod.start(), balanced(text, brace)))
    return out


def main():
    frames, hits = [], []
    helpers = {}

    for path in sorted(SRC.rglob("*.rs")):
        raw = path.read_text(encoding="utf-8")
        text = mask(raw)
        tregs = test_regions(text)

        for m in FN_DEF.finditer(text):
            brace = text.find("{", m.end())
            if brace < 0:
                continue
            end = balanced(text, brace)
            body = text[brace:end + 1]
            # A test body is not shipped code: without this split a `#[cfg(test)]` helper of the same
            # name exposes every frame that calls it, and the reachability number measures tests.
            if any(ts <= m.start() <= te for ts, te in tregs):
                continue
            # (does this definition take a lock and unwrap it, what does this body call). A name with
            # several definitions keeps one entry per definition, so a duplicated helper is never
            # dropped for being ambiguous: the scan treats the name as unsafe if any definition is.
            helpers.setdefault(m.group(1), []).append(
                (bool(LOCK_UNWRAP.search(body)), set(CALL.findall(body)))
            )

        for (start, end, label, kind) in frame_regions(text):
            body = text[start:end + 1]
            in_test = any(ts <= start <= te for ts, te in tregs)
            calls = sorted(set(CALL.findall(body)))
            frame = {
                "file": str(path),
                "line": line_of(raw, start),
                "label": label,
                "kind": kind,
                "in_test": in_test,
                "calls": calls,
            }
            frames.append(frame)

            for hit in LOCK_UNWRAP.finditer(body):
                at = start + hit.start()
                snippet = " ".join(raw[max(start, at - 46):start + hit.end()].split())
                hits.append({**frame, "hit_line": line_of(raw, at), "snippet": snippet})

    shipped_frames = [f for f in frames if not f["in_test"]]
    test_frames = [f for f in frames if f["in_test"]]
    shipped_hits = [h for h in hits if not h["in_test"]]
    test_hits = [h for h in hits if h["in_test"]]

    def per_file(rows):
        counts = {}
        for row in rows:
            counts[row["file"]] = counts.get(row["file"], 0) + 1
        return counts

    print(f"frame sites scanned: {len(shipped_frames)} shipped, {len(test_frames)} in #[cfg(test)]")
    print(f"mutex unwrap()/expect() written inside a shipped extern \"C\" frame: {len(shipped_hits)}")
    print(f"mutex unwrap()/expect() written inside a test-only extern \"C\" frame: {len(test_hits)}")

    print("\nshipped sites, by file:")
    for file, count in sorted(per_file(shipped_hits).items()):
        print(f"  {file}: {count}")

    print("\ntest-only sites, by file:")
    for file, count in sorted(per_file(test_hits).items()):
        print(f"  {file}: {count}")

    print("\n--- shipped sites ---")
    for h in shipped_hits:
        print(f"{h['file']}:{h['hit_line']}  [{h['kind']}] {h['label']}\n    {h['snippet']}")

    print("\n--- test-only sites ---")
    for h in test_hits:
        print(f"{h['file']}:{h['hit_line']}  [{h['kind']}] {h['label']}\n    {h['snippet']}")

    if "--transitive" in sys.argv:
        # The honest question, run to a fixed point over the whole call graph rather than one hop:
        # a name is exposed if any of its definitions takes a lock and unwraps the PoisonResult, or
        # if it calls a name that is exposed. Calls are matched by name only (resolving a Rust call
        # needs a type checker), so this over-approximates: it can name a frame a real call graph
        # would clear, and it never clears a frame a real call graph would name.
        exposed = {name for name, defs in helpers.items() if any(direct for direct, _ in defs)}
        while True:
            grown = {
                name
                for name, defs in helpers.items()
                if name not in exposed and any(calls & exposed for _, calls in defs)
            }
            if not grown:
                break
            exposed |= grown

        print("\n--- shipped frames that reach a lock unwrap through a same-crate helper ---")
        print(f"names in the call graph: {len(helpers)}, exposed by lock or by call: {len(exposed)}")
        reached = 0
        for f in shipped_frames:
            offenders = sorted(set(f["calls"]) & exposed)
            if offenders:
                reached += 1
                print(f"{f['file']}:{f['line']} {f['label']} -> {', '.join(offenders)}")
        print(f"shipped frames reachable this way: {reached} of {len(shipped_frames)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
