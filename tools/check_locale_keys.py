#!/usr/bin/env python3
"""Locale key audit: what the source asks a locale file for, and what each file actually defines.

Usage:
    python tools/check_locale_keys.py            # the working tree
    python tools/check_locale_keys.py --rev HEAD # the committed tree

What it enforces, and exits 1 while any of it is true:
  * a locale file defines the same key twice (the second definition is the one a player sees);
  * a key the source asks for with t!("...") is missing from en.yml, the file every other locale
    falls back to, so the raw key reaches the screen in every language.

What it measures and reports without failing: how many keys en.yml defines that another locale does
not. That backlog is inherited from upstream, whose locale pull requests update a few languages, and
this fork's own options are expected to carry all ten. The number is printed so a later run can
compare it, never asserted in the present tense.

The self test runs first and reads four locale shapes written out here by hand. A reader that cannot
see a duplicate, a missing key, or a key spelled like a YAML boolean is not fit to audit a file.
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.exit("needs PyYAML: python -m pip install pyyaml")

LOCALES = ["en", "es", "fil", "id", "ko", "pt-br", "ru", "vi", "zh-cn", "zh-tw"]
LOCALE_DIR = "assets/locales"
SOURCE_DIR = "src"


class LocaleLoader(yaml.SafeLoader):
    """A loader that records duplicate keys and keeps YAML 1.1 boolean words as strings."""

    def __init__(self, stream):
        super().__init__(stream)
        self.duplicates = []


def _construct_mapping(loader, node, deep=False):
    """Record every key this mapping defines more than once, then build the mapping anyway.

    Raising at the first duplicate would report one block per file and hide the rest, which is how a
    reader ends up 'fixing' a file twice.
    """
    seen, dups = set(), []
    for key_node, _ in node.value:
        name = str(loader.construct_object(key_node, deep=deep))
        if name in seen:
            dups.append(name)
        seen.add(name)
    if dups:
        loader.duplicates.extend(dups)
    return yaml.SafeLoader.construct_mapping(loader, node, deep)


LocaleLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _construct_mapping
)
# `yes:`, `no:`, `on:` and `off:` are real option names in these files. YAML 1.1 resolves them to
# booleans, which would make the key arrive as True/False and look absent from the file that has it.
LocaleLoader.add_constructor(
    "tag:yaml.org,2002:bool", lambda loader, node: loader.construct_scalar(node)
)


def flatten(tree, prefix=""):
    out = {}
    for key, value in tree.items():
        name = f"{prefix}.{key}" if prefix else str(key)
        if isinstance(value, dict):
            out.update(flatten(value, name))
        else:
            out[name] = value
    return out


def locale_bytes(locale, rev):
    path = f"{LOCALE_DIR}/{locale}.yml"
    if rev is None:
        return Path(path).read_bytes()
    done = subprocess.run(["git", "cat-file", "-p", f"{rev}:{path}"], capture_output=True)
    if done.returncode:
        raise SystemExit(f"no {path} at {rev}")
    return done.stdout


def audit(rev, quiet):
    trees, complaints = {}, []
    for locale in LOCALES:
        loader = LocaleLoader(locale_bytes(locale, rev).decode("utf-8"))
        trees[locale] = flatten(loader.get_single_data())
        dups = sorted(set(loader.duplicates))
        if dups:
            complaints.append(f"{locale}.yml defines the same key twice: {', '.join(dups)}")

    referenced, dynamic = referenced_keys(SOURCE_DIR)
    en = trees.get("en", {})

    for key in sorted(referenced - set(en)):
        complaints.append(f"en.yml has no {key}, which the source asks for")

    if not quiet:
        union = set().union(*(set(t) for t in trees.values()))
        print(f"keys referenced by t!() in {SOURCE_DIR}/: {len(referenced)}"
              f" ({dynamic} call sites build their key at runtime and are unread here)")
        for locale in LOCALES:
            own = set(trees[locale])
            print(f"  {locale}.yml: defines {len(own):5d}, missing vs the union {len(union - own):5d},"
                  f" asked for by the source and absent {len(referenced - own):5d}")
    return complaints


def referenced_keys(source_dir):
    """Keys the source asks for. t!("a.b") and rust_i18n::t!("a.b"). A key built at runtime is
    invisible to this reader, so its call sites are counted and printed instead of guessed at."""
    pattern = re.compile(r't!\(\s*"([A-Za-z0-9_.]+)"')
    found = set()
    dynamic = 0
    for path in Path(source_dir).rglob("*.rs"):
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            for match in pattern.finditer(line):
                found.add(match.group(1))
            if re.search(r't!\(\s*[^"\s]', line):
                dynamic += 1
    return found, dynamic


def selftest():
    """Three locale shapes written out here by hand. Returns (exit code, complaints)."""
    nested = "menu:\n  edit_shortcuts: \"Edit\"\nskill_dialog:\n  copy_name: \"Copy name\"\nyes: \"Yes\"\non: \"On\"\n"
    twice = "skill_dialog:\n  copy_name: \"one\"\n  copy_name: \"two\"\n"
    absent = "menu:\n  other: \"x\"\n"

    def read(text):
        loader = LocaleLoader(text)
        return set(flatten(loader.get_single_data())), sorted(set(loader.duplicates))

    problems = []

    keys, dups = read(nested)
    if keys != {"menu.edit_shortcuts", "skill_dialog.copy_name", "yes", "on"}:
        problems.append(f"nested shape read {sorted(keys)}")
    if dups:
        problems.append(f"nested shape reported duplicates {dups} where there are none")

    keys, dups = read(twice)
    if dups != ["copy_name"]:
        problems.append(f"duplicate shape recorded {dups}, wanted ['copy_name']")

    keys, _ = read(absent)
    if keys != {"menu.other"}:
        problems.append(f"absent shape read {sorted(keys)}, wanted only menu.other")

    return (1 if problems else 0), problems


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--rev", help="audit the tree committed at this rev instead of the working tree")
    ap.add_argument("--quiet", action="store_true", help="print only the complaints")
    args = ap.parse_args()

    rc, complaints = selftest()
    if rc:
        print("rule self test FAILED:")
        for c in complaints:
            print(f"   {c}")
        print("the audit will not look at a file it cannot read")
        return rc
    print("rule self test: 3 locale shapes read as written - a nested file whose keys include one"
          " spelled like a YAML boolean, the same key defined twice, and a key the file does not have")

    complaints = audit(args.rev, args.quiet)
    if complaints:
        print("complaints:")
        for c in complaints:
            print(f"   {c}")
        return 1
    print("no duplicate key in any of the ten locales, and en.yml has every key the source asks for")
    return 0


if __name__ == "__main__":
    sys.exit(main())
