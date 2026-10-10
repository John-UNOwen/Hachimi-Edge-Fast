#!/usr/bin/env python3
"""Which barrier stands on each hook boundary `new_hook!` arms.

The coverage scan this replaces walked `def_detour!` occurrences only. That is a population
defined by *which macro wrote the wrapper*, so a boundary written any other way - a helper macro
that expands into a bare `extern "C" fn`, or one that writes its own `guard::detour_barrier` call
instead of reaching the macro family - was not in its population at all, and every total it
printed described a population that excluded them.

This one starts from what makes a function a hook boundary: `new_hook!(addr, Wrapper)` is the call
that arms one, and `Wrapper` names an `extern "C" fn` no matter who wrote it. It walks

  1. every arming site - `new_hook!(.., Wrapper)` written in an `init`, plus the sites written
     through the helper macros that arm on their callers' behalf (`install_getter!`, `probe!`),
  2. back to the definition of each wrapper it names: written straight in the file, by
     `def_detour!`, or by a helper macro, resolved through whichever chain the helper expands
     into, and
  3. the barrier standing on that definition, and the answer that barrier gives the `Panicked`
     trip - because two wrappers behind the same word "barrier" do not owe the game the same call.

Wrappers a `def_detour!` or a helper macro writes that nothing arms (the guard's own test
wrappers, the injected-fault wrappers the hook files build for their tests) are reported in the
cross-check as not armed, so they can neither be quietly counted as coverage nor quietly dropped
from it.

The audit ends with the same question pointed at itself. The population it prints is a claim about
the source tree, and the claim is only worth quoting while the tree that carries the hooks also
carries the audit: `git add -u` and `git commit -am` commit what git tracks and skip a file that was
never put in the index, without saying so. So it asks git, and prints, what the change set holds and
what a commit built that way would drop - and exits non-zero while any file a build or an audit needs
(`src/**`, `tools/**`, `build.rs`, `Cargo.*`) is outside the index. A clean number from a check that
cannot fail is not evidence (AGENTS §10), and this one prints the `??` a `git add -u` commit drops
silently, which is the failure the C2 packaging section records.

Run: python tools/scan_hook_barriers.py [--summary]   (exit 1 = a file the change set needs is not committable)
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

SRC = pathlib.Path("src")

IDENT = r"[A-Za-z_][A-Za-z0-9_]*"
MACRO_HEAD = re.compile(r"\bmacro_rules!\s+(" + IDENT + r")")
NEW_HOOK_HEAD = re.compile(r"\bnew_hook!\s*\(")
DEF_DETOUR_HEAD = re.compile(r"\bdef_detour!\s*")
EXTERN_C_FN = re.compile(r'\bextern\s+"[^"\n]*"\s+fn\s+(' + IDENT + r")")
EXTERN_C_FN_IN_MACRO = re.compile(r'\bextern\s+"[^"\n]*"\s+fn\s+\$(' + IDENT + r")")
MACRO_CALL = re.compile(r"\b(" + IDENT + r")!\s*\(")
CHAR_LITERAL = re.compile(r"(?:\\.|[^'\\\n])'")
PLACEHOLDER = re.compile(r"\$(" + IDENT + r")\s*:\s*"
                         r"(ident|ty|tt|expr|block|meta|vis|literal|path|lifetime|item|stmt|expr_2021)")

# What each `def_detour!` arm answers a `Panicked` trip with. `guard` (src/il2cpp/hook/mod.rs)
# splits a trip in two: `Panicked` is the mod's own code stopping with the game's arguments still
# un-handed-over - the wrapper still owes the game its call - and `Faulted` is the state the
# wrapper was handed being bad, which a replay would fault again.
ARM_ANSWER = {
    "coroutine+bail": "Panicked: the game's MoveNext behind detour_fallback_or, else the door rule",
    "coroutine": "Panicked: door rule - true, and the door comes out of the registry",
    "moves": "Panicked: refusal - a zero the barrier counts and names",
    "publishes": "answer the body published, else a refusal",
    "value+bail": "Panicked: the game's call behind detour_fallback_or",
    "value+fallback": "both trips: the value the wrapper stated",
    "value": "Panicked: refusal - a zero the barrier counts and names",
    "void+bail": "Panicked: the game's call behind detour_fallback",
    "void": "nothing: the wrapper ends, the game's call for that frame is skipped",
    "prelude+bail": "Panicked: the game's call behind detour_fallback, the wrapper's guard held",
    "unknown": "not recognised - read the site",
}


# --------------------------------------------------------------- text preparation

def blank_non_code(text: str) -> str:
    """Comments, strings and char literals replaced by spaces; every offset and line kept.

    Without this the scan reads the prose: `hook/mod.rs`'s own comment names `def_detour!` and
    `new_hook!`, and `new_hook!` formats the string "new_hook!: {}". A scan that sees those as
    code counts boundaries that do not exist.
    """
    out = list(text)
    n = len(text)
    i = 0

    while i < n:
        c = text[i]

        if c == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                out[i] = " "
                i += 1
            continue

        if c == "/" and i + 1 < n and text[i + 1] == "*":
            depth = 0
            while i < n:
                if text.startswith("/*", i):
                    depth += 1
                    out[i] = out[i + 1] = " "
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    out[i] = out[i + 1] = " "
                    i += 2
                    if depth == 0:
                        break
                elif depth:
                    if text[i] != "\n":
                        out[i] = " "
                    i += 1
                else:
                    break
            continue

        if c == '"':
            hashes = 0
            back = i - 1
            if back >= 0 and text[back] == "r":
                while back - 1 >= 0 and text[back - 1] == "#":
                    hashes += 1
                    back -= 1

            closer = '"' + "#" * hashes

            # The quotes stay and only what sits between them is blanked: `extern "C" fn` is a
            # hook definition the audit has to see, while a string or a doc that merely contains
            # those words is not.
            out[i] = '"'
            i += 1

            while i < n:
                if text.startswith(closer, i):
                    out[i] = '"'
                    for k in range(1, len(closer)):
                        out[i + k] = "#"
                    i += len(closer)
                    break

                if text[i] == "\\" and hashes == 0:
                    out[i] = " "
                    if i + 1 < n and text[i + 1] != "\n":
                        out[i + 1] = " "
                    i += 2
                    continue

                if text[i] != "\n":
                    out[i] = " "
                i += 1
            continue
        if c == "'":
            # A char literal, never a lifetime: `'static` is code, `','` is not. Either way this
            # position is consumed - a `'` that is not a char literal must still move the scan on.
            match = CHAR_LITERAL.match(text, i + 1)
            if match:
                for k in range(i, match.end()):
                    out[k] = " "
                i = match.end()
            else:
                i += 1
            continue

        i += 1

    return "".join(out)


def line_starts(text: str) -> list[int]:
    starts = [0]
    for idx, ch in enumerate(text):
        if ch == "\n":
            starts.append(idx + 1)
    return starts


def line_of(starts: list[int], offset: int) -> int:
    lo, hi = 0, len(starts) - 1
    while lo < hi:
        mid = (lo + hi + 1) // 2
        if starts[mid] <= offset:
            lo = mid
        else:
            hi = mid - 1
    return lo + 1


def match_close(text: str, open_at: int, opening: str, closing: str) -> int:
    """Offset of the bracket closing the one at `open_at`."""
    depth = 0
    for idx in range(open_at, len(text)):
        if text[idx] == opening:
            depth += 1
        elif text[idx] == closing:
            depth -= 1
            if depth == 0:
                return idx
    return -1


def split_args(text: str, open_paren: int) -> list[str]:
    """Top-level comma split of a macro call's argument list."""
    close = match_close(text, open_paren, "(", ")")
    if close < 0:
        return []

    args: list[str] = []
    depth = 0
    start = open_paren + 1

    for idx in range(open_paren + 1, close):
        ch = text[idx]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == "," and depth == 0:
            args.append(text[start:idx].strip())
            start = idx + 1

    tail = text[start:close].strip()
    if tail:
        args.append(tail)

    return args


def skip_attributes(text: str, at: int) -> int:
    """Past the `#[...]` attributes and the visibility a wrapper definition may open with."""
    while True:
        while at < len(text) and text[at].isspace():
            at += 1
        if text.startswith("#[", at):
            bracket = text.index("[", at)
            end = match_close(text, bracket, "[", "]")
            if end < 0:
                return at
            at = end + 1
            continue
        match = re.match(r"pub(?:\([^)]*\))?\s+", text[at:])
        if match:
            at += match.end()
            continue
        return at


def tokens_of(text: str) -> list[str]:
    """Top-level token sketch of a `def_detour!` body: whole blocks collapse to one BLOCK token."""
    tokens: list[str] = []
    i = 0
    n = len(text)

    while i < n:
        ch = text[i]

        if ch.isspace():
            i += 1
            continue

        if ch == "{":
            end = match_close(text, i, "{", "}")
            if end < 0:
                break
            tokens.append("BLOCK")
            i = end + 1
            continue

        if text.startswith("->", i):
            tokens.append("->")
            i += 2
            continue

        match = re.match(IDENT, text[i:])
        if match:
            tokens.append(match.group(0))
            i += match.end()
            continue

        if ch == "#":
            if not text.startswith("#[", i):
                i += 1
                continue
            end = match_close(text, i + 1, "[", "]")
            if end < 0:
                break
            i = end + 1
            continue

        i += 1

    return tokens


# ------------------------------------------------------------------- macro shapes

class Macro:
    def __init__(self, path: str, name: str, body_at: int, body: str):
        self.path = path
        self.name = name
        self.body_at = body_at
        self.body = body
        self.arms = self._split_arms(body)

        # arm index -> placeholder name -> which comma separated argument of a call it is
        self.placeholders: list[dict[str, int]] = []
        for _pattern_start, pattern, _body_at, _arm_body in self.arms:
            self.placeholders.append(
                {match.group(1): index for index, match in enumerate(PLACEHOLDER.finditer(pattern))}
            )

        self.wrapper_args: set[int] = set()  # call arguments this macro names a wrapper with
        self.arming_args: set[int] = set()   # call arguments this macro hands to a new_hook!
        self.expands_to = "no barrier"       # refined to "def_detour!" / "hand barrier"

    @staticmethod
    def _split_arms(body: str) -> list[tuple[int, str, int, str]]:
        arms: list[tuple[int, str, int, str]] = []
        i = 0
        n = len(body)

        while i < n:
            while i < n and (body[i].isspace() or body[i] in ";,"):
                i += 1
            if i >= n or body[i] != "(":
                break

            pattern_close = match_close(body, i, "(", ")")
            if pattern_close < 0:
                break

            after = pattern_close + 1
            while after < n and body[after].isspace():
                after += 1
            if body[after:after + 2] != "=>":
                break

            arm_body_at = after + 2
            while arm_body_at < n and body[arm_body_at].isspace():
                arm_body_at += 1
            if arm_body_at >= n or body[arm_body_at] not in "{(":
                break

            opening = body[arm_body_at]
            arm_body_close = match_close(body, arm_body_at, opening, {"{": "}", "(": ")"}[opening])
            if arm_body_close < 0:
                break

            arms.append((i, body[i:pattern_close + 1], arm_body_at, body[arm_body_at:arm_body_close + 1]))
            i = arm_body_close + 1

        return arms


def collect_macros(path: str, code: str) -> dict[str, Macro]:
    macros: dict[str, Macro] = {}
    for match in MACRO_HEAD.finditer(code):
        brace = code.find("{", match.end())
        if brace < 0:
            continue
        close = match_close(code, brace, "{", "}")
        if close < 0:
            continue
        # The arms are what sits *inside* the macro's braces, and `body_at` is where that inside
        # starts, so an arm's offset can be turned back into an offset in the file.
        macro = Macro(path, match.group(1), brace + 1, code[brace + 1:close])
        macros[macro.name] = macro
    return macros


def enclosing_arm(spans: list[tuple[int, int, str, int]], offset: int) -> tuple[str | None, int | None]:
    """The innermost macro arm whose body holds `offset` -> (macro name, arm index)."""
    best: tuple[int, str, int] | None = None
    for start, end, macro_name, arm_index in spans:
        if start <= offset < end and (best is None or start >= best[0]):
            best = (start, macro_name, arm_index)
    return (best[1], best[2]) if best else (None, None)


def arm_shape(invocation: str) -> str:
    """Which `def_detour!` arm an invocation matched, from the tokens after its parameter list.

    The wrapper name may be a `$placeholder` - this classifies the call a helper macro writes as
    well as the one a hook file writes.
    """
    brace = invocation.find("{")
    close = match_close(invocation, brace, "{", "}")
    if brace < 0 or close < 0:
        return "unknown"

    inner = invocation[brace + 1:close]
    at = skip_attributes(inner, 0)
    name_match = re.match(r"\$?" + IDENT, inner[at:])
    if not name_match:
        return "unknown"

    paren = inner.find("(", at + name_match.end())
    paren_close = match_close(inner, paren, "(", ")")
    if paren < 0 or paren_close < 0:
        return "unknown"

    tokens = tokens_of(inner[paren_close + 1:])
    if not tokens:
        return "unknown"

    blocks = [index for index, token in enumerate(tokens) if token == "BLOCK"]
    if not blocks:
        return "unknown"

    # The wrapper's body is the first block of every arm (`prelude` puts a guard block ahead of
    # it, and that guard is what the arm exists for). A trip answer - `bail` or `fallback` - is a
    # keyword standing after that body at the arm's own level.
    answer_words = {token for index, token in enumerate(tokens)
                    if index > blocks[0] and token in ("bail", "fallback")}
    has_bail = "bail" in answer_words
    has_fallback = "fallback" in answer_words

    head = tokens[0]

    if head == "coroutine":
        return "coroutine+bail" if has_bail else "coroutine"
    if head == "moves":
        return "moves"
    if head == "prelude":
        return "prelude+bail" if has_bail else "prelude (no bail arm written)"
    if head == "->":
        if has_bail:
            return "value+bail"
        if has_fallback:
            return "value+fallback"
        return "value"
    if head == "BLOCK":
        return "void+bail" if has_bail else "void"

    # `answer -> ty {..}`: an identifier holding the answer the body publishes.
    if "->" in tokens[1:blocks[0]]:
        return "publishes"
    return "unknown"


def shape_of_definition(
    rel: str,
    kind: str,
    line: int,
    via: tuple[str, ...],
    code: dict[str, str],
    starts: dict[str, list[int]],
    macros: dict[str, dict[str, Macro]],
) -> str:
    """The `def_detour!` arm a definition ends up in, following the helper macro chain to it."""
    if kind != "def_detour!":
        return ""

    if not via:
        return arm_shape(code[rel][starts[rel][line - 1]:])

    table = macros[rel]
    macro = table.get(via[0][:-1])
    seen: set[str] = set()

    while macro is not None and macro.name not in seen:
        seen.add(macro.name)
        for _p, _pattern, _body_at, arm_body in macro.arms:
            for match in DEF_DETOUR_HEAD.finditer(arm_body):
                return arm_shape(arm_body[match.start():])

        next_macro = None
        for _p, _pattern, _body_at, arm_body in macro.arms:
            for match in MACRO_CALL.finditer(arm_body):
                callee = table.get(match.group(1))
                if callee is not None and callee is not macro and callee.wrapper_args:
                    next_macro = callee
                    break
            if next_macro is not None:
                break
        macro = next_macro

    return "unknown"


# --------------------------------------------------------------------- the scan

class Boundary:
    def __init__(self, arm_path: str, arm_line: int, wrapper: str, via_macro: str):
        self.arm_path = arm_path
        self.arm_line = arm_line
        self.wrapper = wrapper
        self.armed_via = via_macro
        self.def_path: str | None = None
        self.def_line: int | None = None
        self.via: tuple[str, ...] = ()
        self.barrier = "no definition found"
        self.detail = "armed by name; no extern \"C\" fn for that name was found"
        self.answer = ""


# ------------------------------------------------------------ committability of the audit

# A file the change set has to carry: without one of them the tree does not build, or nobody can
# re-print the count that closed the item. Anything else (a scratch capture, a run log, a `__pycache__`
# the tool itself leaves behind) may stay untracked - AGENTS section 4 says so out loud.
CARRYING = ("src/", "tools/", "build.rs", "Cargo.toml", "Cargo.lock")


def carrying(path: str) -> bool:
    """Whether git dropping this path would cost the change set something it cannot regenerate."""
    if "__pycache__/" in path or path.endswith(".pyc"):
        return False  # regenerable interpreter output, and AGENTS §4 keeps build output out of git
    return path.startswith(CARRYING)


STATUS_LETTERS = {
    " M": "tracked, modified, not staged",
    "M ": "tracked, modified, staged",
    "A ": "new file, staged",
    " A": "intent-to-add: in the index, content not staged",
    "??": "untracked: invisible to git add -u and to git commit -am",
    " D": "deleted, not staged",
    "D ": "deleted, staged",
    "R ": "renamed, staged",
    "T ": "type changed",
}


def git_output(*args: str) -> list[str] | None:
    """git's stdout for one command, or None when git cannot be run here.

    The return code is deliberately not consulted: `git ls-files -- tools` exits 0 with empty output for a
    path nothing tracks, and `git status --porcelain` does the same for a clean tree. The answer is in the
    lines.
    """
    try:
        done = subprocess.run(["git", *args], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout.splitlines()


def committability() -> int:
    """Ask git what the change set carries, and return how many needed files a commit would drop.

    This is the part that prose kept getting wrong: `git add -u` and `git commit -am` commit what git
    tracks and skip a file no one ever put in the index, exiting 0 and printing nothing. A barrier
    audit whose own file is not in the index is a count that outlives its evidence.
    """
    status = git_output("status", "--porcelain", "-uall")
    if status is None:
        print("committability")
        print("  git cannot be run here: not checked")
        print("")
        return 0

    letters: dict[str, int] = {}
    untracked: list[str] = []
    entry: dict[str, str] = {}
    for line in status:
        if len(line) < 3:
            continue
        code = line[:2]
        letters[code] = letters.get(code, 0) + 1
        entry.setdefault(line[3:], code)
        if code == "??":
            untracked.append(line[3:])

    tools = sorted(p.as_posix() for p in pathlib.Path("tools").rglob("*") if p.is_file())
    index_tools = set(git_output("ls-files", "--", "tools") or [])

    def state_of(path: str) -> str:
        """How a commit built from this tree treats the path. `??` is the only shape that vanishes."""
        code = entry.get(path)
        if code == "??":
            return "untracked"
        if code == " A":
            return "intent-to-add"
        if code is not None:
            return "in the change set"
        if path in index_tools:
            return "tracked"
        return "ignored by git"

    at_risk = [tool for tool in tools
               if state_of(tool) in ("untracked", "ignored by git") and carrying(tool)]
    dropped = sorted({p for p in untracked if carrying(p)} | set(at_risk))

    print("committability (git status --porcelain -uall; git ls-files -- tools)")
    for code in sorted(letters):
        print(f"  {code} {letters[code]:>5}  {STATUS_LETTERS.get(code, 'other')}")
    carried = [tool for tool in tools if state_of(tool) not in ("untracked", "ignored by git")]
    print(f"  audit tooling under tools/: {len(tools)}, git can commit it: {len(carried)}")
    for tool in tools:
        note = "" if carrying(tool) else "  (regenerable output: stays out of git)"
        print(f"      {state_of(tool):<15} {tool}{note}")
    if untracked:
        print(f"  untracked files this tree carries: {len(untracked)}")
        for path in untracked:
            need = "the build or an audit needs it" if carrying(path) else "scratch: stays out of git by design"
            print(f"      {path}  ({need})")
    if dropped:
        print(f"  a git add -u / git commit -am commit silently drops {len(dropped)}:")
        for path in dropped:
            print(f"      {path}")
    else:
        print("  a git add -u / git commit -am commit drops nothing this change set needs")
    print("")

    return len(dropped)


def main() -> int:
    summary_only = "--summary" in sys.argv

    if not SRC.is_dir():
        print("run this from the repository root: no src/ here", file=sys.stderr)
        return 2

    code: dict[str, str] = {}
    starts: dict[str, list[int]] = {}

    for path in sorted(pathlib.Path("src").rglob("*.rs")):
        rel = path.relative_to(pathlib.Path(".")).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            text = path.read_text(encoding="utf-8", errors="replace")
        code[rel] = blank_non_code(text)
        starts[rel] = line_starts(text)

    macros = {rel: collect_macros(rel, body) for rel, body in code.items()}

    arm_spans: dict[str, list[tuple[int, int, str, int]]] = {}
    for rel, table in macros.items():
        spans = []
        for macro in table.values():
            for arm_index, (_p, _pattern, body_at, arm_body) in enumerate(macro.arms):
                absolute = macro.body_at + body_at
                spans.append((absolute, absolute + len(arm_body), macro.name, arm_index))
        arm_spans[rel] = spans

    # --- pass 1: what each macro does with its own arguments
    for rel, table in macros.items():
        for macro in table.values():
            for arm_index, (_p, _pattern, _body_at, arm_body) in enumerate(macro.arms):
                placeholders = macro.placeholders[arm_index]

                # A wrapper the arm writes itself, straight as an extern "C" fn. The string `"C"`
                # is blanked but its quotes are not, so the shape is matched, not the letters.
                for match in EXTERN_C_FN_IN_MACRO.finditer(arm_body):
                    if match.group(1) in placeholders:
                        macro.wrapper_args.add(placeholders[match.group(1)])

                # A wrapper the arm hands to `def_detour!`.
                for match in DEF_DETOUR_HEAD.finditer(arm_body):
                    brace = arm_body.find("{", match.end())
                    if brace < 0:
                        continue
                    at = skip_attributes(arm_body, brace + 1)
                    token = re.match(r"\$?(" + IDENT + r")", arm_body[at:])
                    if not token:
                        continue
                    macro.expands_to = "def_detour!"
                    if token.group(1) in placeholders:
                        macro.wrapper_args.add(placeholders[token.group(1)])

                # A new_hook! the arm writes on its caller's behalf.
                for match in NEW_HOOK_HEAD.finditer(arm_body):
                    args = split_args(arm_body, match.end() - 1)
                    if len(args) < 2 or not args[1].startswith("$"):
                        continue
                    bare = args[1][1:]
                    if bare in placeholders:
                        macro.arming_args.add(placeholders[bare])

        # A helper macro whose body reaches for the barrier itself instead of `def_detour!`.
        for macro in table.values():
            if macro.expands_to == "def_detour!":
                continue
            for _p, _pattern, _body_at, arm_body in macro.arms:
                if re.search(r"\bdetour_barrier\s*\(", arm_body):
                    macro.expands_to = "hand barrier"
                    break

    # --- pass 2: a helper macro that writes a wrapper by calling another wrapper-writing macro
    for _round in range(6):
        changed = False
        for rel, table in macros.items():
            for macro in table.values():
                if macro.wrapper_args:
                    continue
                for arm_index, (_p, _pattern, _body_at, arm_body) in enumerate(macro.arms):
                    placeholders = macro.placeholders[arm_index]
                    for match in MACRO_CALL.finditer(arm_body):
                        callee = table.get(match.group(1))
                        if callee is None or callee is macro or not callee.wrapper_args:
                            continue
                        args = split_args(arm_body, match.end() - 1)
                        for position in callee.wrapper_args:
                            if position >= len(args) or not args[position].startswith("$"):
                                continue
                            bare = args[position][1:]
                            if bare in placeholders:
                                macro.wrapper_args.add(placeholders[bare])
                                macro.expands_to = callee.expands_to
                                changed = True
        if not changed:
            break

    # --- every wrapper definition the tree writes
    definitions: dict[str, list[dict]] = {}

    def add_definition(rel: str, offset: int, wrapper: str, kind: str, via: tuple[str, ...] = ()):
        definitions.setdefault(wrapper, []).append({
            "path": rel,
            "line": line_of(starts[rel], offset),
            "wrapper": wrapper,
            "kind": kind,
            "via": via,
        })

    # Written straight in a file. The `$hook` inside a helper macro's arm is not a match: `$` is
    # not an identifier start, so only real definitions land here.
    for rel, body in code.items():
        for match in EXTERN_C_FN.finditer(body):
            add_definition(rel, match.start(), match.group(1), "extern \"C\" fn written in the file")

    # `def_detour!` invocations. Generic (a `$placeholder` name) where the call sits inside a
    # macro arm, concrete where the wrapper name is written out.
    for rel, body in code.items():
        for match in DEF_DETOUR_HEAD.finditer(body):
            brace = body.find("{", match.end())
            if brace < 0:
                continue
            at = skip_attributes(body, brace + 1)
            token = re.match(r"\$?(" + IDENT + r")", body[at:])
            if not token or body[at] == "$":
                continue
            macro_name, _arm = enclosing_arm(arm_spans[rel], match.start())
            via = (macro_name + "!",) if macro_name else ()
            add_definition(rel, match.start(), token.group(1), "def_detour!", via)

    # Invocations of a helper macro that writes a wrapper.
    for rel, body in code.items():
        table = macros[rel]
        for match in MACRO_CALL.finditer(body):
            macro = table.get(match.group(1))
            if macro is None or macro.name == "def_detour" or not macro.wrapper_args:
                continue
            args = split_args(body, match.end() - 1)
            for position in sorted(macro.wrapper_args):
                if position >= len(args) or args[position].startswith("$"):
                    continue
                kind = {"def_detour!": "def_detour!", "hand barrier": "hand barrier"}.get(
                    macro.expands_to, "no barrier")
                add_definition(rel, match.start(), args[position], kind, (macro.name + "!",))

    # --- every arming site
    boundaries: list[Boundary] = []

    for rel, body in code.items():
        for match in NEW_HOOK_HEAD.finditer(body):
            macro_name, _arm = enclosing_arm(arm_spans[rel], match.start())
            if macro_name and macros[rel][macro_name].arming_args:
                continue  # counted where that macro is called, with its real arguments
            args = split_args(body, match.end() - 1)
            if len(args) < 2 or args[1].startswith("$"):
                continue
            boundaries.append(Boundary(rel, line_of(starts[rel], match.start()), args[1], ""))

    for rel, body in code.items():
        for macro in macros[rel].values():
            if not macro.arming_args:
                continue
            for match in re.finditer(r"\b" + re.escape(macro.name) + r"!\s*\(", body):
                caller, _arm = enclosing_arm(arm_spans[rel], match.start())
                if caller == macro.name:
                    continue
                args = split_args(body, match.end() - 1)
                for position in sorted(macro.arming_args):
                    if position >= len(args) or args[position].startswith("$"):
                        continue
                    boundaries.append(
                        Boundary(rel, line_of(starts[rel], match.start()), args[position], macro.name + "!")
                    )

    # --- match each boundary to the definition its name refers to
    def function_body(rel: str, line: int) -> str:
        text = code[rel]
        offset = starts[rel][line - 1]
        found = EXTERN_C_FN.search(text, offset)
        if not found:
            return ""
        paren = text.find("(", found.end())
        paren_close = match_close(text, paren, "(", ")")
        if paren < 0 or paren_close < 0:
            return ""
        brace = text.find("{", paren_close)
        end = match_close(text, brace, "{", "}")
        return text[brace:end + 1] if 0 <= brace < end else ""

    for boundary in boundaries:
        found = [d for d in definitions.get(boundary.wrapper, []) if d["path"] == boundary.arm_path]
        found = found or definitions.get(boundary.wrapper, [])

        if not found:
            continue

        definition = found[0]
        boundary.def_path = definition["path"]
        boundary.def_line = definition["line"]
        boundary.via = definition["via"]
        kind = definition["kind"]

        if kind == "def_detour!":
            shape = shape_of_definition(
                boundary.def_path, kind, boundary.def_line, boundary.via, code, starts, macros)
            boundary.barrier = "def_detour!"
            boundary.detail = "arm " + shape
            boundary.answer = ARM_ANSWER.get(shape, ARM_ANSWER["unknown"])
        elif kind == "hand barrier":
            boundary.barrier = "hand barrier"
            boundary.detail = ("guard::detour_barrier written out by hand"
                               + (", through " + " -> ".join(boundary.via) if boundary.via else ""))
            boundary.answer = "whatever that site's own match arms say - it is not the macro family's rule"
        elif kind == "no barrier":
            boundary.barrier = "no barrier"
            boundary.detail = ("nothing in front of the body"
                               + (", written by " + " -> ".join(boundary.via) if boundary.via else ""))
            boundary.answer = "none - a panic or a fault crosses this boundary"
        else:
            body = function_body(boundary.def_path, boundary.def_line)
            if re.search(r"\bdetour_barrier\b", body):
                boundary.barrier = "hand barrier"
                boundary.detail = "guard::detour_barrier written out by hand inside the wrapper body"
                boundary.answer = "whatever that site's own match arms say - it is not the macro family's rule"
            elif re.search(r"\bdef_detour!", body):
                boundary.barrier = "def_detour!"
                boundary.detail = "def_detour! written inside the wrapper"
                boundary.answer = ARM_ANSWER["unknown"]
            else:
                boundary.barrier = "no barrier"
                boundary.detail = "bare extern \"C\" fn, nothing in front of its body"
                boundary.answer = "none - a panic or a fault crosses this boundary"

    # --------------------------------------------------------------- report
    direct = [b for b in boundaries if b.barrier == "def_detour!" and not b.via]
    through = [b for b in boundaries if b.barrier == "def_detour!" and b.via]
    hand = [b for b in boundaries if b.barrier == "hand barrier"]
    none = [b for b in boundaries if b.barrier == "no barrier"]
    unresolved = [b for b in boundaries if b.barrier == "no definition found"]

    print("Hook boundary barrier audit")
    print(f"population: every new_hook! arming site under {SRC.as_posix()}/** "
          f"(comments and strings blanked, helper macros resolved)")
    print("")
    print(f"boundaries found:                                       {len(boundaries)}")
    print(f"  def_detour! written at the site:                      {len(direct)}")
    via_counts: dict[str, int] = {}
    for boundary in through:
        key = " -> ".join(boundary.via)
        via_counts[key] = via_counts.get(key, 0) + 1
    print(f"  def_detour! reached through a helper macro:           {len(through)}")
    for key in sorted(via_counts):
        print(f"      via {key:<58} {via_counts[key]}")
    print(f"  barrier hand-written at the site:                     {len(hand)}")
    print(f"  no barrier at all:                                    {len(none)}")
    print(f"  armed but the audit found no definition:              {len(unresolved)}")
    print("")
    print(f"covered by a barrier (any kind):                        {len(direct) + len(through) + len(hand)}")
    print(f"covered through the def_detour! macro family:           {len(direct) + len(through)}")
    print(f"not covered:                                            {len(none) + len(unresolved)}")
    print("")

    if not summary_only:
        for title, rows in (
            ("barrier hand-written at the site", hand),
            ("no barrier", none),
            ("armed but no definition found", unresolved),
        ):
            if not rows:
                print(f"{title}: none")
                print("")
                continue
            print(f"{title} ({len(rows)}):")
            for boundary in sorted(rows, key=lambda b: (b.arm_path, b.arm_line)):
                where = f"{boundary.arm_path}:{boundary.arm_line}"
                definition = f"  defined {boundary.def_path}:{boundary.def_line}" if boundary.def_path else ""
                print(f"  {boundary.wrapper:<52} {where}{definition}")
                print(f"      {boundary.detail}")
            print("")

        print(f"def_detour! ({len(direct) + len(through)}), by the answer its arm gives a Panicked trip:")
        by_answer: dict[str, list[Boundary]] = {}
        for boundary in direct + through:
            by_answer.setdefault(boundary.answer, []).append(boundary)
        for answer in sorted(by_answer, key=lambda a: (-len(by_answer[a]), a)):
            rows = by_answer[answer]
            print(f"  [{answer}]  {len(rows)} wrapper(s)")
            for boundary in sorted(rows, key=lambda b: (b.arm_path, b.arm_line)):
                via = " via " + " -> ".join(boundary.via) if boundary.via else ""
                print(f"      {boundary.wrapper:<52} {boundary.def_path}:{boundary.def_line}{via}"
                      f"  [{boundary.detail}]")
        print("")

    # `--show NAME`: how this audit resolved one wrapper, for anyone checking a row.
    show = [sys.argv[i + 1] for i, a in enumerate(sys.argv) if a == "--show"]
    for name in show:
        print(f"--- {name}")
        for entry in definitions.get(name, []):
            print(f"    definition {entry['path']}:{entry['line']} kind={entry['kind']} via={entry['via']}")
        for boundary in boundaries:
            if boundary.wrapper == name:
                print(f"    armed at {boundary.arm_path}:{boundary.arm_line} via_macro={boundary.armed_via}"
                      f" barrier={boundary.barrier} detail={boundary.detail}")
        for rel, table in macros.items():
            for macro in table.values():
                if macro.name in ("def_detour",):
                    continue
                if any(name in args for args in [split_args(code[rel], m.end() - 1)
                                                 for m in MACRO_CALL.finditer(code[rel])
                                                 if m.group(1) == macro.name]):
                    print(f"    macro {macro.name} at {rel}:{line_of(starts[rel], macro.body_at)}"
                          f" arms={len(macro.arms)} wrapper_args={sorted(macro.wrapper_args)}"
                          f" arming_args={sorted(macro.arming_args)} expands_to={macro.expands_to}")
        print("")

    # --- cross-checks: what the population does and does not contain
    armed_sites = {(b.def_path, b.def_line) for b in boundaries if b.def_path}
    all_definitions = [entry for entries in definitions.values() for entry in entries]
    not_armed = [e for e in all_definitions
                 if (e["path"], e["line"]) not in armed_sites
                 and e["kind"] != "extern \"C\" fn written in the file"]

    armed_names: dict[str, int] = {}
    for boundary in boundaries:
        armed_names[boundary.wrapper] = armed_names.get(boundary.wrapper, 0) + 1
    shared = sorted(name for name, count in armed_names.items() if count > 1)

    print("cross-checks")
    print(f"  wrapper definitions the tree writes:                  {len(all_definitions)}")
    print(f"  ... armed by a new_hook!:                             {len(armed_sites)}")
    print(f"  ... not armed (test wrappers, helpers nothing arms):  {len(not_armed)}")
    for entry in sorted(not_armed, key=lambda e: (e["path"], e["line"]))[:60]:
        print(f"      not armed: {entry['wrapper']:<45} {entry['path']}:{entry['line']}  "
              f"{entry['kind']}{' via ' + ' -> '.join(entry['via']) if entry['via'] else ''}")
    if len(not_armed) > 60:
        print(f"      ... and {len(not_armed) - 60} more")
    print(f"  wrapper names armed more than once in the tree:       {len(shared)}")
    for name in shared:
        sites = ", ".join(f"{b.arm_path}:{b.arm_line}" for b in boundaries if b.wrapper == name)
        print(f"      {name:<45} {sites}")
    print(f"  boundaries this audit cannot classify:                {len(unresolved)}")
    print("")

    dropped = committability()
    return 1 if dropped else 0


if __name__ == "__main__":
    raise SystemExit(main())
