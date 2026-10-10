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

It also checks the key `disabled_hooks` matches on (C27), and it checks that rule against the tree rather
than against itself:

  * The rule is read out of `src/il2cpp/hook/mod.rs`, the file the arming path runs - the id expression
    `new_hook!` builds, the prefix `hook_id` cuts off it, the function the macro consults for the decision,
    and every comparison that function makes between a configured key and a hook id. The audit prints what
    it read with the line each came from, and it exits non-zero on a comparison in that decision it could
    not read: a rewritten rule this tool has not read fails the audit instead of passing it.
  * The log a player reads is written by a second copy of the same rule, `hook_key_match`, and a rule written
    twice by hand is a rule that can drift. The audit reads that copy too - mapping its parameters from the
    one comparison in it that cuts a last segment out of a hook id - and exits non-zero when the two copies
    name different halves, cut a bare name at different separators, join with `&&` rather than each naming a
    hook on its own, or when the report copy is written so the audit cannot tell which parameter a configured
    key reaches.
  * `new_hook!` keys each arming site on its own hook id, so the cross-check walks the population once more
    and prints how many distinct ids it comes to. Two armed sites answering to one id is the defect coming
    back - one key, more than one hook - so it exits non-zero on that.
  * The same rule keeps the bare wrapper name it matched on before ids existed. For *every* wrapper name
    this tree arms, the audit compares the population the *shipped* rule puts down - the comparisons read
    out of `hook/mod.rs`, evaluated over the arming population - with the population `disabled_hooks
    .contains(stringify!($hook))`, the rule the fork replaced, put down. A name whose two populations
    differ is a config key silently losing its effect, and it exits non-zero on that. Cutting the
    bare-name half out of `hook_is_disabled` moves this row and the one above it together: the shipped
    rule reaches 0 of the sites a pre-id key names, the replaced rule still reaches all of them, and the
    audit exits 1. It is the check that was missing when this defect shipped behind a "Legacy keys
    unchanged" claim, and it is not the check that proves the *behaviour* - `cargo test --lib`'s
    `every_bare_key_disables_the_population_the_bare_name_rule_disabled` calls the shipped function over
    the 14 names / 38 sites `COLLIDING_KEYS` holds, and `a_bare_key_naming_a_wrapper_nothing_else_shares
    _still_disables_that_hook_alone` calls it over `SINGLE_NAME_KEYS`. This audit reads the decision and
    the population; it does not execute it.
  * The Rust tests' fixtures are copies of this tool's population, so the audit compares both copies with
    what the tree arms now and exits non-zero when one no longer matches it.

Run: python tools/scan_hook_barriers.py [--summary]   (exit 1 = a file the change set needs is not
committable, or two armed sites answer to one `disabled_hooks` key, or the shipped key rule reaches a
different population than the wrapper-name rule it replaced, or the decision in `hook/mod.rs` is written in
a shape this audit cannot read, or a test's fixture no longer matches the tree)
"""

from __future__ import annotations

from collections import Counter
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


def crate_name() -> str:
    """The name `module_path!()` starts every hook id with, read out of Cargo.toml.

    Typed here it would go stale on a rename and the audit would keep printing a key the build no
    longer produces. The `[lib] name` is what the crate compiles as; `[package] name` is the fallback.
    """
    section = ""
    try:
        for line in pathlib.Path("Cargo.toml").read_text(encoding="utf-8").splitlines():
            stripped = line.strip()
            if stripped.startswith("[") and stripped.endswith("]"):
                section = stripped[1:-1]
            elif section in ("lib", "package") and stripped.startswith("name"):
                return stripped.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    return "hachimi"


CRATE = crate_name()
# ------------------------------------------------- the key rule, read out of its own source
#
# C27's tree half used to rest on two rows that could not report anything. The audit built each site's
# key itself - `<module read off the file path>::<the wrapper that site armed>` - and then asked whether
# that key's last segment was that wrapper, and whether the rule the audit modelled reached a different
# population than the wrapper-name rule it replaced. Both questions were asked of the audit's own copy of
# the rule, so each printed 0 on any tree, including a tree whose shipped `hook_is_disabled` compares
# hook ids alone - which is the build this defect arrived in. AGENTS section 10: a check that cannot match
# what it looks for returns a clean number on any tree.
#
# So the rule is read out of the file the arming path runs. Out of `src/il2cpp/hook/mod.rs` the audit
# takes the id expression `new_hook!` builds, the prefix `hook_id` cuts off it, the function the arming
# macro consults for the decision, and every comparison that function makes between a configured key and
# the hook id of the site being armed - with the separator each cut uses and the line each was read at, so
# the claim the audit prints names its source. The one thing here not read from the shipped source is what
# `module_path!()` is for a file, and the Rust side pins that too
# (`hook_ids_are_the_call_site_module_plus_the_wrapper_name`). A shape in that file this audit cannot model
# is reported as one and gates the exit, so a rewrite of the rule the tool has not read fails the audit
# instead of passing it.

KEY_RULE_FILE = pathlib.Path("src/il2cpp/hook/mod.rs")

# The function the key report after hooking matches on. It and the arming decision are two copies of the
# same rule written by hand, and the log a player reads comes from this one.
REPORT_FN = "hook_key_match"

ID_CONCAT = re.compile(r"concat!\s*\(\s*module_path!\s*\(\s*\)\s*,\s*")
ID_TAIL = re.compile(r"\"[^\n]*\"\s*,\s*stringify!\s*\(\s*\$([A-Za-z_][A-Za-z0-9_]*)\s*\)\s*\)")
ROOT_CONST = re.compile(r"\bconst\s+HOOK_ID_ROOT\s*:\s*[^=;\n]+=")
LAST_CUT = re.compile(r"\.(rsplit|rsplitn|split|splitn)\s*\(")
MEMBERSHIP = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*contains\s*\(")
SIDE = r"(?:[A-Za-z_][A-Za-z0-9_]*\s*\(\s*[A-Za-z_][A-Za-z0-9_]*\s*\)|[A-Za-z_][A-Za-z0-9_]*)"
EQUALITY = re.compile("(" + SIDE + r")\s*==\s*(" + SIDE + ")")
CALL_HEAD = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")

# Every way this file could compare a configured key with a hook id that the audit does not model as a
# comparison. Counted in the decision's answer only to notice one appearing, never to model it.
COMPAREISH = re.compile(r"\.(?:contains|starts_with|ends_with|eq|ne|matches|strip_prefix|binary_search)\s*\(")


def flat(text: str) -> str:
    """Whitespace removed, so a comparison read out of a formatted file compares as one string."""
    return re.sub(r"\s+", "", text)


def fn_span(blank: str, name: str) -> tuple[int, int, int] | None:
    """Offsets in blanked text of `fn name(..)`: its head, the brace opening its body, the closing brace."""
    head = re.search(r"\bfn\s+" + re.escape(name) + r"\s*\(", blank)
    if not head:
        return None
    paren = match_close(blank, head.end() - 1, "(", ")")
    if paren < 0:
        return None
    brace = blank.find("{", paren)
    if brace < 0:
        return None
    close = match_close(blank, brace, "{", "}")
    return (head.start(), brace, close) if close > brace else None


def statement_breaks(body: str) -> list[int]:
    """Offsets of the `;` that end a statement of this body, at the body's own brace depth.

    The coverage check needs the one expression a function answers with. A `;` inside a block or a call is
    not the end of a statement of the body, and treating one as such would cut the region the comparisons
    were read from down to part of what the function actually returns.
    """
    depth = 0
    out: list[int] = []
    for idx, ch in enumerate(body):
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == ";" and depth <= 0:
            out.append(idx)
    return out


def literal_in(raw: str, blank: str, start: int, end: int) -> str | None:
    """The first string literal written in raw[start:end].

    `blank_non_code` blanks what sits between the quotes and leaves the quotes where they were, so the
    blanked text locates a literal at the same offsets and the raw text supplies its content.
    """
    quote = blank.find('"', start, end)
    if quote < 0:
        return None
    close = blank.find('"', quote + 1, end)
    return raw[quote + 1:close] if close > quote else None


def module_path_of(site_path: str, sep: str) -> str:
    """`module_path!()` for a file: `src/` dropped, `mod.rs` dropped, `.rs` dropped, joined by `sep`.

    A hook armed through a helper macro is named by the file that macro was *called* from - the arm path
    this audit recorded - not the file the helper macro is written in, which is what `module_path!()`
    expands to at the call site.
    """
    parts = pathlib.PurePosixPath(site_path).parts[1:]
    if parts and parts[-1] == "mod.rs":
        parts = parts[:-1]
    return sep.join(p[:-3] if p.endswith(".rs") else p for p in parts)


def parameter_names(blank: str, head_at: int, brace: int) -> list[str]:
    """The parameter names of a Rust function, in order, out of its signature."""
    open_paren = blank.find("(", head_at)
    close = match_close(blank, open_paren, "(", ")")
    if open_paren < 0 or close < 0 or close > brace:
        return []

    parts: list[str] = []
    depth = 0
    start = open_paren + 1
    for idx in range(open_paren + 1, close):
        ch = blank[idx]
        if ch in "([{<":
            depth += 1
        elif ch in ")]}>":
            depth -= 1
        elif ch == "," and depth == 0:
            parts.append(blank[start:idx])
            start = idx + 1
    if blank[start:close].strip():
        parts.append(blank[start:close])

    names: list[str] = []
    for part in parts:
        head = re.match(r"\s*(?:[A-Za-z_][A-Za-z0-9_]*\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:", part)
        if head:
            names.append(head.group(1))
    return names


class KeyRule:
    """The `disabled_hooks` decision as the shipped source writes it (C27)."""

    def __init__(self) -> None:
        self.read_from = KEY_RULE_FILE.as_posix()
        self.id_at = 0
        self.id_shape = ""
        self.id_found = False
        self.id_sep = "::"
        self.id_cut_by = ""
        self.root = ""
        self.root_at = 0
        self.decision = ""
        self.decision_at = 0
        self.decision_call = ""
        self.bare_helper = ""
        self.bare_sep = ""
        self.bare_at = 0
        self.bare_keeps_one = False
        self.connective = ""
        self.comparisons: list[tuple[str, int, str]] = []  # (source text, line, "id" or "bare")
        self.cannot: list[str] = []

    def site_id(self, boundary) -> str:
        """The key one armed site answers to, built the way the id read out of the macro is built.

        `module_path!()` for the site joined to the wrapper its `new_hook!` names, with the separator read
        out of the macro, then the prefix `hook_id` cuts - including for a site armed in the hook module
        itself, where the id that leaves is the bare wrapper name.
        """
        full = (CRATE + self.id_sep + module_path_of(boundary.arm_path, self.id_sep)
                + self.id_sep + boundary.wrapper)
        return full[len(self.root):] if self.root and full.startswith(self.root) else full

    def reaches(self, key: str, site_id: str) -> bool:
        """Whether one configured key puts one site down, under the comparisons read from the source."""
        answers: list[bool] = []
        for _text, _line, kind in self.comparisons:
            if kind == "id":
                answers.append(key == site_id)
            elif kind == "bare":
                answers.append(key == site_id.rsplit(self.bare_sep, 1)[-1])
        if not answers:
            return False
        return all(answers) if self.connective == "&&" else any(answers)


def last_cut_of(blank: str, raw: str, name: str) -> tuple[str, bool] | None:
    """(`separator`, keeps a single segment) when `fn name(x)` answers the last segment of `x`."""
    span = fn_span(blank, name)
    if span is None:
        return None

    _head, brace, close = span
    body = blank[brace + 1:close]
    cut = LAST_CUT.search(body)
    if not cut:
        return None

    sep = literal_in(raw, blank, cut.end(), close)
    if sep is None:
        return None
    return sep, bool(re.search(r"\.unwrap_or\b", body[cut.end():]))


def classify_side(rule: KeyRule, blank: str, raw: str, side: str, id_param: str) -> str:
    """`id` when a side is a hook id itself, `bare` when it is that id's last segment, else empty."""
    side = flat(side)
    if side == id_param:
        return "id"

    helper = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)\(" + re.escape(id_param) + r"\)", side)
    if not helper:
        return ""

    name = helper.group(1)
    cut = last_cut_of(blank, raw, name)
    if cut is None:
        rule.cannot.append(
            f"{rule.read_from}: the key is compared with {side}, and fn {name} is not a cut at a fixed "
            "separator this audit can model")
        return ""

    if rule.bare_helper and rule.bare_helper != name:
        rule.cannot.append(
            f"{rule.read_from}: the bare half of the rule reaches through both {rule.bare_helper} and {name}")
        return ""

    rule.bare_helper = name
    rule.bare_sep, rule.bare_keeps_one = cut
    return "bare"


def read_decision_fn(rule: KeyRule, blank: str, raw: str, starts: list[int], name: str,
                     slots: dict[int, str], seen: tuple[str, ...] = ()) -> None:
    """Add to `rule` every comparison `fn name` makes between a configured key and a hook id.

    `slots` maps the function's positional parameters to the two things a key rule can be about: the key
    the player configured, and the id of the site being armed. They come from the call the arming macro
    writes, so the audit follows the data - who hands the decision the set and who hands it the id -
    instead of assuming which side of a comparison is which. A comparison this mapping does not explain is
    reported as unread rather than guessed at, because guessing is what made the old rows print 0 whatever
    the shipped rule said.
    """
    if name in seen:
        return

    span = fn_span(blank, name)
    if span is None:
        rule.cannot.append(f"{rule.read_from}: fn {name} is not in this file, so that half of the rule is unread")
        return

    head_at, brace, close = span
    params = parameter_names(blank, head_at, brace)
    if len(params) != len(slots) or any(index not in slots for index in range(len(params))):
        rule.cannot.append(
            f"{rule.read_from}: fn {name} takes {len(params)} parameters and the arming macro hands it "
            f"{len(slots)}, so which parameter a configured key reaches cannot be read")
        return

    slot_of = {param: slots[index] for index, param in enumerate(params)}
    id_param = next((param for param, slot in slot_of.items() if slot == "id"), "")
    body = blank[brace + 1:close]

    found = list(MEMBERSHIP.finditer(body))
    shape = "membership"
    if not found:
        found = list(EQUALITY.finditer(body))
        shape = "equality"

    if not found:
        delegated: tuple[str, dict[int, str]] | None = None
        for call in CALL_HEAD.finditer(body):
            closed = match_close(body, call.end() - 1, "(", ")")
            if closed < 0:
                continue
            args = [flat(arg) for arg in split_args(body, call.end() - 1)]
            if not args or any(arg not in slot_of for arg in args):
                continue
            delegated = (call.group(1), {index: slot_of[arg] for index, arg in enumerate(args)})
            break

        if delegated is None:
            rule.cannot.append(
                f"{rule.read_from}: fn {name} answers whether a key disables a hook in a form this audit "
                "does not model, so the tree-wide key claim has nothing behind it")
            return

        read_decision_fn(rule, blank, raw, starts, delegated[0], delegated[1], seen + (name,))
        return

    connective = ""
    previous_close = -1
    modelled = 0
    boundaries = 0
    first_at = -1
    last_end = -1

    for match in found:
        open_paren = match.end() - 1
        closed = match_close(body, open_paren, "(", ")") if shape == "membership" else -1
        if shape == "membership" and closed < 0:
            rule.cannot.append(f"{rule.read_from}: fn {name} writes a `.contains(` this audit cannot read")
            continue

        if shape == "membership":
            sides = [match.group(1), body[open_paren + 1:closed]]
            end_of_comparison = closed
        else:
            sides = [match.group(1), match.group(2)]
            end_of_comparison = match.end() - 1

        key_side = next((side for side in sides if slot_of.get(flat(side)) == "key"), "")
        others = [side for side in sides if side != key_side]

        if not key_side or len(others) != 1:
            rule.cannot.append(
                f"{rule.read_from}:{line_of(starts, brace + 1 + match.start())} writes "
                f"{flat(match.group(0))}, which does not compare the configured key with the hook id of the "
                "site being armed")
            continue

        kind = classify_side(rule, blank, raw, others[0], id_param)
        if not kind:
            if not rule.cannot or not rule.cannot[-1].startswith(rule.read_from):
                rule.cannot.append(
                    f"{rule.read_from}:{line_of(starts, brace + 1 + match.start())} compares the configured "
                    f"key with {flat(others[0])}, a shape this audit does not model")
            continue

        written = flat(match.group(0) + others[0] + ")") if shape == "membership" else flat(match.group(0))
        rule.comparisons.append((written, line_of(starts, brace + 1 + match.start()), kind))
        modelled += 1
        first_at = match.start() if first_at < 0 else min(first_at, match.start())
        last_end = max(last_end, end_of_comparison)

        gap = flat(body[previous_close + 1:match.start()]) if previous_close >= 0 else ""
        previous_close = end_of_comparison
        if not gap:
            continue
        if gap in ("||", "&&"):
            if connective and connective != gap:
                rule.cannot.append(
                    f"{rule.read_from}: fn {name} joins its comparisons with both {connective} and {gap}")
            connective = gap
        elif re.search(r"(?:\}|\))\s*if$|;\s*if$", gap):
            # Comparisons written as separate guard arms: each one that matches decides on its own, which
            # is a disjunction of the same kind the `||` form is.
            boundaries += 1
            if connective not in ("", "arm"):
                rule.cannot.append(
                    f"{rule.read_from}: fn {name} joins its comparisons with both {connective} and separate "
                    "guard arms")
            connective = "arm"
        else:
            rule.cannot.append(
                f"{rule.read_from}: fn {name} joins its comparisons with {gap}, which this audit does not "
                "model")

    if rule.bare_helper:
        helper = fn_span(blank, rule.bare_helper)
        rule.bare_at = 0 if helper is None else line_of(starts, helper[0])

    if modelled:
        # The model is only a reading of the decision if it accounts for the whole expression the decision
        # returns. An adapter the audit did not model - an `any(..)` wrapped around the comparisons, a third
        # comparison it could not place - would otherwise vanish from the model instead of being said, and a
        # rule that vanishes from the model is exactly how the last version of this check passed a tree whose
        # shipped rule had already lost its bare half.
        region_start = 0
        region_end = len(body)
        breaks = statement_breaks(body)
        earlier = [at for at in breaks if at < first_at]
        if earlier:
            region_start = earlier[-1] + 1
        later = [at for at in breaks if at >= last_end]
        if later:
            region_end = later[0]

        region = body[region_start:region_end]
        # Counted from the operators written there, not from this tool's own patterns: a comparison whose
        # sides are more than a name or one call - a chained `id.rsplit(..).unwrap_or(..)`, an index, a
        # method on a field - is invisible to those patterns, and an invisible comparison is an uncounted
        # population.
        written = len(re.findall(r"==|!=", region)) + len(COMPAREISH.findall(region))
        adapters = re.findall(r"\.(?:any|all|iter|iter_mut|filter|find|position|chain|take|skip|map"
                              r"|flat_map|copied|cloned|drain|retain)\s*\(", region)
        operators = len(re.findall(r"\|\||&&", region))

        arms = len(re.findall(r"=>", region))
        reasons = []
        if arms:
            reasons.append(f"it answers through {arms} match arm(s), which this audit does not read as a "
                           "comparison between a key and a hook id")
        if written != modelled:
            reasons.append(f"the expression it returns writes {written} comparison, so "
                           f"{written - modelled} of them is in a shape this audit does not match")
        if adapters:
            reasons.append(f"it wraps comparisons in {len(adapters)} adapter call(s): "
                           + ", ".join(sorted({flat(a) for a in adapters})))
        if operators + boundaries != max(modelled - 1, 0):
            reasons.append(f"its {modelled} comparison(s) are joined by {operators + boundaries}, which is "
                           "not the one connective per pair the model assumes")
        if reasons:
            rule.cannot.append(f"{rule.read_from}: fn {name} answers with {modelled} comparison this audit "
                               "modelled, and " + "; ".join(reasons))


def read_key_rule(blank: str, raw: str, starts: list[int], new_hook) -> KeyRule:
    """Read the `disabled_hooks` rule out of `hook/mod.rs`, the file the arming path runs (C27)."""
    rule = KeyRule()

    if new_hook is None:
        rule.cannot.append(f"{rule.read_from}: no macro_rules! new_hook to read the key rule from")
        return rule

    root = ROOT_CONST.search(blank)
    if root is None:
        rule.cannot.append(f"{rule.read_from}: no const HOOK_ID_ROOT, so no id prefix is cut off")
    else:
        end = blank.find(";", root.start())
        end = len(blank) if end < 0 else end
        join = ID_CONCAT.search(blank, root.start(), end)
        sep = literal_in(raw, blank, join.end(), end) if join else None
        if sep is None:
            rule.cannot.append(
                f"{rule.read_from}: HOOK_ID_ROOT is not concat!(module_path!(), <separator>), so the prefix "
                "cut off a hook id cannot be read")
        else:
            rule.root_at = line_of(starts, root.start())
            rule.root = CRATE + sep + module_path_of(KEY_RULE_FILE.as_posix(), sep) + sep

    cut_id = fn_span(blank, "hook_id")
    if cut_id is None:
        rule.cannot.append(f"{rule.read_from}: no fn hook_id, so nothing cuts the hook module off an id")
    elif "strip_prefix(HOOK_ID_ROOT)" not in flat(blank[cut_id[1]:cut_id[2]]):
        rule.cannot.append(
            f"{rule.read_from}: fn hook_id no longer cuts HOOK_ID_ROOT off the id new_hook! builds")

    for arm_index, (_pattern_at, _pattern, body_at, arm_body) in enumerate(new_hook.arms):
        start = new_hook.body_at + body_at
        end = start + len(arm_body)
        join = ID_CONCAT.search(blank, start, end)
        if not join:
            continue

        tail = ID_TAIL.match(blank, join.end())
        sep = literal_in(raw, blank, join.end(), end)
        if not tail or sep is None:
            rule.cannot.append(
                f"{rule.read_from}:{line_of(starts, join.start())} builds an id from module_path!() "
                "in a shape this audit cannot read")
            continue

        wrapper = tail.group(1)
        position = new_hook.placeholders[arm_index].get(wrapper)
        if position != 1:
            rule.cannot.append(
                f"{rule.read_from}:{line_of(starts, join.start())} puts stringify!(${wrapper}) - "
                f"argument {position} of new_hook! - into the id, and the audit reads the wrapper from "
                "argument 1")
            continue

        rule.id_at = line_of(starts, join.start())
        rule.id_found = True
        rule.id_sep = sep
        rule.id_shape = f"concat!(module_path!(), {sep!r}, stringify!(${wrapper}))"

        before = flat(blank[max(start, join.start() - 60):join.start()]).rstrip()
        rule.id_cut_by = "hook_id" if before.endswith("hook_id(") else ""
        if not rule.id_cut_by:
            rule.cannot.append(
                f"{rule.read_from}:{rule.id_at} hands the id to nothing that cuts the hook module root off it")

        decision: dict[int, str] | None = None
        for call in CALL_HEAD.finditer(blank, start, end):
            closed = match_close(blank, call.end() - 1, "(", ")")
            if closed < 0 or closed > end:
                continue
            args = split_args(blank, call.end() - 1)
            key_slot = next((i for i, arg in enumerate(args) if "disabled_hooks" in arg), None)
            if key_slot is None or len(args) != 2:
                continue

            id_slot = 1 - key_slot
            bound = re.search(r"\blet\s+" + re.escape(flat(args[id_slot])) + r"\s*=\s*([^;]*)", arm_body)
            if bound is None or "module_path!" not in bound.group(1):
                rule.cannot.append(
                    f"{rule.read_from}:{line_of(starts, call.start())} decides with "
                    f"{flat(args[id_slot])}, which this arm does not bind to an id built from module_path!()")
                continue

            rule.decision = call.group(1)
            rule.decision_at = line_of(starts, call.start())
            rule.decision_call = flat(call.group(1) + "(" + ",".join(args) + ")")
            decision = {key_slot: "key", id_slot: "id"}
            break

        if decision is None and not rule.cannot:
            rule.cannot.append(
                f"{rule.read_from}:{line_of(starts, start)} new_hook! does not hand one function both the "
                "configured disabled_hooks and this site's hook id, so the arming decision is not one rule")
            continue

        if decision is not None and "disabled_hooks.contains(" in flat(arm_body):
            rule.cannot.append(
                f"{rule.read_from}:{rule.decision_at} new_hook! compares the configured set itself, which is "
                "the rule C27 was written against, instead of consulting one decision function")

        if decision is not None:
            read_decision_fn(rule, blank, raw, starts, rule.decision, decision)

    if not rule.id_found:
        rule.cannot.append(
            f"{rule.read_from}: no new_hook! arm builds its key out of module_path!(), so the ids this "
            "audit keys the arming sites on are its own guess and not the build's")

    return rule


def infer_key_id_slots(blank: str, raw: str, name: str) -> dict[int, str] | None:
    """Which parameter of `fn name` a configured key sits on and which a hook id sits on.

    The arming macro states this outright: it hands the decision `config.load().disabled_hooks` and the id it
    just built. The function the key report uses has no such call to read - it is called from a closure over
    the configured set and from a loop over the ids hooking collected - so the audit takes the mapping from
    the only thing in that function that can tell the two apart: the half that cuts a last segment out of a
    hook id. A function with nothing like that in it is reported as unread.
    """
    span = fn_span(blank, name)
    if span is None:
        return None

    head_at, brace, close = span
    params = parameter_names(blank, head_at, brace)
    if len(params) != 2:
        return None

    for match in EQUALITY.finditer(blank[brace + 1:close]):
        helper = re.fullmatch(r"([A-Za-z_][A-Za-z0-9_]*)\(([A-Za-z_][A-Za-z0-9_]*)\)", flat(match.group(2)))
        if helper is None or last_cut_of(blank, raw, helper.group(1)) is None:
            continue

        id_param, key_param = helper.group(2), flat(match.group(1))
        if id_param not in params or key_param not in params or id_param == key_param:
            continue
        return {params.index(key_param): "key", params.index(id_param): "id"}

    return None


def read_report_rule(blank: str, raw: str, starts: list[int]) -> KeyRule:
    """The key rule read out of the report half: the function that decides what the log says a key reached."""
    rule = KeyRule()
    slots = infer_key_id_slots(blank, raw, REPORT_FN)
    if slots is None:
        rule.cannot.append(
            f"{rule.read_from}: fn {REPORT_FN} writes no comparison that cuts a last segment off a hook id, "
            "so the audit cannot tell which of its parameters a configured key reaches")
        return rule

    read_decision_fn(rule, blank, raw, starts, REPORT_FN, slots)
    return rule


def fixture_block(name: str) -> str:
    """The `&[..]` body of one test fixture written in `hook/mod.rs`, or an empty string."""
    try:
        source = KEY_RULE_FILE.read_text(encoding="utf-8")
    except OSError:
        return ""

    head = source.find("const " + name)
    if head < 0:
        return ""
    open_ = source.find("= &[", head)
    if open_ < 0:
        return ""
    open_ += 2

    depth = 0
    for index in range(open_, len(source)):
        if source[index] == "[":
            depth += 1
        elif source[index] == "]":
            depth -= 1
            if depth == 0:
                return source[open_:index + 1]
    return ""


def fixture_populations() -> dict[str, list[str]]:
    """The population the Rust test guarding the bare-name half names, read out of `hook/mod.rs`.

    `COLLIDING_KEYS` is the fixture `every_bare_key_disables_the_population_the_bare_name_rule_disabled`
    compares the shipped rule against the wrapper-name rule it replaced. It was copied out of this
    tool's own output, and a copy nobody re-checks stops guarding the sites the tree actually arms, so
    the audit reads it back and compares it with the population it derived a moment ago.
    """
    entries = re.finditer(r'\(\s*"([^"]+)"\s*,\s*&\[(.*?)\]', fixture_block("COLLIDING_KEYS"), re.S)
    return {match.group(1): sorted(set(re.findall(r'"([^"]+)"', match.group(2)))) for match in entries}


def fixture_single_names() -> list[tuple[str, str]]:
    """The (`bare name`, `id`) pairs the key rule's test asserts for a wrapper nothing else shares.

    Those are the names `COLLIDING_KEYS` cannot name - the majority of the arming population - and they
    are hand-copied from this tool's output too, so the same re-check applies: a name the tree no longer
    arms, or an id the tree no longer builds, is a test that stopped guarding anything.
    """
    return [(match.group(1), match.group(2))
            for match in re.finditer(r'\(\s*"([^"]+)"\s*,\s*"([^"]+)"\s*\)', fixture_block("SINGLE_NAME_KEYS"))]



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
    raw: dict[str, str] = {}
    starts: dict[str, list[int]] = {}

    for path in sorted(pathlib.Path("src").rglob("*.rs")):
        rel = path.relative_to(pathlib.Path(".")).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            text = path.read_text(encoding="utf-8", errors="replace")
        code[rel] = blank_non_code(text)
        raw[rel] = text
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

    # C27, over the whole arming population. Two questions, both asked of the rule read out of
    # `hook/mod.rs` above rather than of a copy this tool keeps: is the key one site answers to its own,
    # and does a key a config written before ids were keys may already hold still reach exactly the sites
    # the wrapper-name rule reached?
    key_rule = read_key_rule(code.get(KEY_RULE_FILE.as_posix(), ""),
                             raw.get(KEY_RULE_FILE.as_posix(), ""),
                             starts.get(KEY_RULE_FILE.as_posix(), [0]),
                             macros.get(KEY_RULE_FILE.as_posix(), {}).get("new_hook"))

    site_keys = [(b, key_rule.site_id(b)) for b in boundaries]
    key_counts = Counter(key for _, key in site_keys)
    shared_keys = sorted(key for key, count in key_counts.items() if count > 1)

    # The replaced rule, spelled out here rather than called, the way the Rust test spells it out:
    # `disabled_hooks.contains(stringify!($hook))` - the wrapper name the arming call's second argument
    # names, which is this audit's own measurement of the site and not the rule being audited.
    reached: dict[str, list[str]] = {}
    reached_before: dict[str, list[str]] = {}
    for name in sorted(armed_names):
        reached[name] = sorted({key for _, key in site_keys if key_rule.reaches(name, key)})
        reached_before[name] = sorted({key for b, key in site_keys if name == b.wrapper})
    population_drift = [(name, reached[name], reached_before[name]) for name in reached if reached[name] != reached_before[name]]
    legacy_sites = sum(len(keys) for keys in reached.values())

    # The two fixtures the Rust tests are built from are copies of this population. A copy that no longer
    # matches what the tree arms is a test that stopped guarding the sites the tree arms.
    fixture = fixture_populations()
    tree_groups = {name: sorted({key for b, key in site_keys if b.wrapper == name}) for name in shared}
    fixture_drift = sorted(name for name in set(tree_groups) | set(fixture) if fixture.get(name, []) != tree_groups.get(name, []))

    single_drift: list[str] = []
    singles = fixture_single_names()
    for name, key in singles:
        sites = [b for b in boundaries if b.wrapper == name]
        if not sites:
            single_drift.append(f"{name}: no arming site writes that wrapper here, the test names {key}")
        elif len(sites) > 1:
            single_drift.append(f"{name}: the tree arms it in {len(sites)} files, so it is not a single-name fixture")
        elif key_rule.site_id(sites[0]) != key:
            single_drift.append(f"{name}: this tree builds {key_rule.site_id(sites[0])} for that site, the test names {key}")

    print(f"  boundaries this audit cannot classify:                {len(unresolved)}")
    print(f"  the key rule read from {key_rule.read_from}:           {len(key_rule.comparisons)} comparisons against the configured set")
    if key_rule.id_at:
        print(f"      new_hook! builds the id at :{key_rule.id_at}         {key_rule.id_shape}, handed to {key_rule.id_cut_by or 'nothing'}")
    if key_rule.root:
        print(f"      the prefix cut off an id is {key_rule.root!r} (HOOK_ID_ROOT at :{key_rule.root_at})")
    if key_rule.decision:
        print(f"      new_hook! decides through {key_rule.decision} at :{key_rule.decision_at}")
    for text, line, kind in key_rule.comparisons:
        half = ("names one hook: the whole id" if kind == "id"
                else f"names every hook sharing a name: {key_rule.bare_helper} cuts the id at "
                     f"{key_rule.bare_sep!r} (fn at :{key_rule.bare_at})"
                     + (", and answers the whole id when there is no separator" if key_rule.bare_keeps_one else ""))
        print(f"      {text:<38} :{line:<5} {half}")
    if key_rule.connective:
        print(f"      joined with {key_rule.connective}")
    print(f"  ... comparisons in that decision this audit could not read: {len(key_rule.cannot)}")
    for item in key_rule.cannot[:20]:
        print(f"      rule unread: {item}")

    # The log a player reads is written by a second copy of the same rule. Two hand-written copies of one
    # rule is how a rule drifts, so both are read and their shapes are compared.
    report_rule = read_report_rule(code.get(KEY_RULE_FILE.as_posix(), ""),
                                   raw.get(KEY_RULE_FILE.as_posix(), ""),
                                   starts.get(KEY_RULE_FILE.as_posix(), [0]))
    rule_halves: list[str] = []
    armed_halves = [kind for _text, _line, kind in key_rule.comparisons]
    report_halves = [kind for _text, _line, kind in report_rule.comparisons]
    if armed_halves != report_halves:
        rule_halves.append(f"the arming decision names {armed_halves or 'nothing'} and {REPORT_FN} names "
                           f"{report_halves or 'nothing'}")
    if key_rule.bare_sep and report_rule.bare_sep and key_rule.bare_sep != report_rule.bare_sep:
        rule_halves.append(f"the arming decision cuts a bare name at {key_rule.bare_sep!r} and {REPORT_FN} "
                           f"cuts it at {report_rule.bare_sep!r}")
    if key_rule.connective == "&&" or report_rule.connective == "&&":
        rule_halves.append(f"one of them joins its comparisons with && rather than naming a hook on its own")

    print(f"  the same rule read out of the report half ({REPORT_FN}): {len(report_rule.comparisons)} "
          f"comparisons; disagreeing with the arming decision: {len(rule_halves)}")
    for text, line, kind in report_rule.comparisons:
        print(f"      {text:<38} :{line:<5} {kind}")
    for item in report_rule.cannot[:10]:
        print(f"      report rule unread: {item}")
    for item in rule_halves[:10]:
        print(f"      key rule halves disagree: {item}")
    print(f"  hook ids the arming sites key on (C27):               {len(key_counts)} ids over {len(site_keys)} sites")
    print(f"  ... ids more than one site answers to:                {len(shared_keys)}")
    for key in shared_keys[:20]:
        print(f"      {key:<45} armed at "
              + ", ".join(f"{b.arm_path}:{b.arm_line}" for b, k in site_keys if k == key))
    for name in shared:
        print(f"      keyed apart: {name:<37} {', '.join(tree_groups[name])}")

    print(f"  bare wrapper names a pre-id config may hold (C27):    {len(armed_names)} names over {len(site_keys)} sites, "
          f"reaching {legacy_sites} sites between them")
    print(f"  ... names whose population the two rules disagree on: {len(population_drift)}")
    for name, now, before in population_drift[:20]:
        print(f"      key effect changed: {name:<31} now {', '.join(now) or 'nothing'} / before {', '.join(before) or 'nothing'}")
    print(f"  ... the key rule's own test covers (COLLIDING_KEYS):  {len(fixture)} names over "
          f"{sum(len(ids) for ids in fixture.values())} sites; drift: {len(fixture_drift)}")
    for name in fixture_drift[:20]:
        print(f"      fixture stale: {name:<31} tree arms {', '.join(tree_groups.get(name, [])) or 'nothing'} / "
              f"test names {', '.join(fixture.get(name, [])) or 'nothing'}")
    print(f"  ... their single-name fixture (SINGLE_NAME_KEYS):     {len(fixture_single_names())} names over "
          f"{len(boundaries)} sites; drift: {len(single_drift)}")
    for row in single_drift[:20]:
        print(f"      single-name fixture stale: {row}")
    print("")

    key_rule_broken = (shared_keys or key_rule.cannot or population_drift or fixture_drift or single_drift
                       or rule_halves or report_rule.cannot)
    dropped = committability()
    return 1 if (dropped or key_rule_broken) else 0


if __name__ == "__main__":
    raise SystemExit(main())
