#!/usr/bin/env python3
"""Read LEDGER.md and write LEDGER-INDEX.md, one line per numbered item.

LEDGER.md is the fork's working memory: 336 KB over 1,970 lines, 112 numbered items. A finished item keeps its
heading and its live state here and moves its dated receipts into LEDGER-ARCHIVE.md, naming the block it moved
to. This tool exists so a reader, human or agent, can find an item by number and open only the lines it needs
instead of reading the whole file. It also guards the shape of both files: a single logical line over
MAX_LINE_CHARS is reported, because a line that long cannot be read by line number and it defeats every grep
that reports whole lines. The archive has no items of its own, so it is listed by block rather than indexed.

Usage:
  python tools/ledger_index.py            read LEDGER.md, write LEDGER-INDEX.md, print the counts
  python tools/ledger_index.py --check    same, but exit 1 if the index is stale or a line is too long
  python tools/ledger_index.py --stdout   print the index instead of writing it

LEDGER-INDEX.md is generated from LEDGER.md and is gitignored like it. It is not snapshotted into the ledger
history: regenerate it instead.
"""

import argparse
import pathlib
import re
import sys

MAX_LINE_CHARS = 1200
TITLE_CHARS = 110

NUMBERED_ITEM = re.compile(r"^(\d+)\. \[( |~|x)\] (.*)$")
MARKS = {"x": "fixed", "~": "partial", " ": "open"}
SECTION = re.compile(r"^## (\w+)\. (.*)$")
FENCE = re.compile(r"^\s*```")
# A continuation of a heading or a bullet is indented deeper than the marker that started it and carries no
# marker of its own. Item sub bullets are "    - ", one space shallower than a "120. " continuation.
CONTINUATION = re.compile(r"^\s{2,}\S")
NOT_CONTINUATION = re.compile(r"^\s*(- |\d+\. \[|\[^\d|## )")


def read_lines(path: pathlib.Path) -> list[str]:
    if not path.exists():
        raise SystemExit(f"{path} is missing; LEDGER.md is a local record, restore it with tools/knowledge-snapshot.sh --restore")
    return path.read_bytes().decode("utf-8").split("\r\n")


def fold(lines: list[str], start: int) -> tuple[str, int]:
    """Join a heading line with the continuation lines that belong to it. Return the text and the next index."""
    parts = [lines[start].strip()]
    index = start + 1
    while index < len(lines):
        line = lines[index]
        if not line.strip() or NOT_CONTINUATION.match(line) or FENCE.match(line):
            break
        if not CONTINUATION.match(line):
            break
        parts.append(line.strip())
        index += 1
    return " ".join(parts), index


def title_of(text: str) -> str:
    """First sentence of an item, cut short, with the number and mark taken off. The rest is a grep away."""
    body = re.sub(r"^\d+\. \[[ ~x]\] ", "", text)
    cut = re.split(r"\.\s|\.\z|:\s", body, maxsplit=1)[0].strip()
    if len(cut) <= TITLE_CHARS:
        return cut
    clipped = cut[:TITLE_CHARS].rsplit(" ", 1)[0]
    return clipped + "..."


def date_of(text: str) -> str:
    match = re.search(r"20\d\d-\d\d-\d\d", text)
    return match.group(0) if match else ""


def scan(lines: list[str]) -> tuple[list[dict], list[tuple[int, int]], dict]:
    items: list[dict] = []
    long_lines: list[tuple[int, int]] = []
    counts = {"fixed": 0, "partial": 0, "open": 0, "sections": {}, "crlf": 0}
    section = ""
    in_fence = False

    for index, line in enumerate(lines):
        if FENCE.match(line):
            in_fence = not in_fence
        if len(line) > MAX_LINE_CHARS and not in_fence:
            long_lines.append((index + 1, len(line)))
        match = SECTION.match(line)
        if match:
            section = match.group(1)
            counts["sections"].setdefault(section, {"items": 0, "fixed": 0, "partial": 0, "open": 0})
            continue
        if in_fence:
            continue
        if re.match(r"^- \[( |~|x)\]", line):
            key = MARKS[line[3]]
            counts[key] += 1
            tally = counts["sections"].get(section)
            if tally:
                tally[key] += 1
            continue
        match = NUMBERED_ITEM.match(line)
        if not match:
            continue
        number, mark = match.group(1), match.group(2)
        text, _next = fold(lines, index)
        key = MARKS[mark]
        counts[key] += 1
        tally = counts["sections"].get(section)
        if tally:
            tally[key] += 1
            tally["items"] += 1
        items.append({
            "number": int(number),
            "mark": mark,
            "line": index + 1,
            "section": section or "-",
            "date": date_of(text),
            "title": title_of(text),
        })
    return items, long_lines, counts


def scan_archive(lines: list[str]) -> tuple[list[dict], list[tuple[int, int]]]:
    """The blocks of LEDGER-ARCHIVE.md and any of its lines that run past the limit.

    The archive is not indexed item by item: it has no items, only blocks an item points at. The block list is
    there so a reader knows what is in it without opening 400 KB, and its lines are checked against the same
    limit so a moved receipt cannot smuggle an unreadable line out of sight.
    """
    heads = [i for i, line in enumerate(lines) if line.startswith("## ")]
    blocks = []
    for n, i in enumerate(heads):
        j = heads[n + 1] if n + 1 < len(heads) else len(lines)
        blocks.append({
            "heading": lines[i][3:],
            "line": i + 1,
            "lines": j - i,
            "bytes": sum(len(l) + 2 for l in lines[i:j]),
        })
    long_lines = [(i + 1, len(l)) for i, l in enumerate(lines) if len(l) > MAX_LINE_CHARS]
    return blocks, long_lines


def build_index(items: list[dict], long_lines: list[tuple[int, int]], counts: dict,
                archive_blocks: list[dict], archive_long: list[tuple[int, int]]) -> str:
    out = [
        "# LEDGER.md index",
        "",
        "Generated by `python tools/ledger_index.py`. Read this first, then open the item you need by its line",
        "number in `LEDGER.md`. Regenerate it after any change to `LEDGER.md`; it is gitignored and not snapshotted.",
        "",
        f"Items: {len(items)}. Marks: fixed {counts['fixed']}, partial {counts['partial']}, open {counts['open']}.",
        "",
    ]
    for section, tally in sorted(counts["sections"].items()):
        label = section or "before the first section heading"
        out.append(f"- Section {label}: {tally['items']} items, fixed {tally['fixed']}, partial {tally['partial']}, open {tally['open']}")
    out.append("")
    out.append("Marks are `x` fixed, `~` partial, `-` open. `line` is the line the item starts on in `LEDGER.md`.")
    out.append("")
    out.append("| # | mark | line | section | dated | what the item is |")
    out.append("|---|---|---|---|---|---|")
    for item in items:
        mark = {" ": "-", "x": "x", "~": "~"}[item["mark"]]
        out.append(
            f"| {item['number']} | {mark} | {item['line']} | {item['section']} "
            f"| {item['date']} | {item['title']} |"
        )
    if archive_blocks:
        out.append("")
        out.append("## Archive blocks")
        out.append("")
        out.append("`LEDGER-ARCHIVE.md` holds dated receipts that were moved out of live items. An item stays here and")
        out.append("names the block it moved to. The archive is snapshotted with `LEDGER.md` and holds no item of its own.")
        out.append("")
        for block in archive_blocks:
            out.append(f"- `## {block['heading']}` at line {block['line']}, {block['lines']} lines, {block['bytes']:,} bytes")
    if long_lines or archive_long:
        out.append("")
        out.append(f"## Lines over {MAX_LINE_CHARS} characters")
        out.append("")
        out.append("A line this long cannot be read by line number and defeats a line based grep. Wrap it: split at")
        out.append("spaces, and give each continuation the marker indent it continues.")
        out.append("")
        for line_no, length in long_lines:
            out.append(f"- `LEDGER.md:{line_no}`, {length} characters")
        for line_no, length in archive_long:
            out.append(f"- `LEDGER-ARCHIVE.md:{line_no}`, {length} characters")
    out.append("")
    return "\r\n".join(out)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--check", action="store_true", help="exit 1 if the index is stale or a line is too long")
    parser.add_argument("--stdout", action="store_true", help="print the index instead of writing it")
    args = parser.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    ledger = root / "LEDGER.md"
    archive_path = root / "LEDGER-ARCHIVE.md"
    index_path = root / "LEDGER-INDEX.md"

    lines = read_lines(ledger)
    items, long_lines, counts = scan(lines)
    archive_blocks: list[dict] = []
    archive_long: list[tuple[int, int]] = []
    if archive_path.exists():
        archive_blocks, archive_long = scan_archive(read_lines(archive_path))
    index = build_index(items, long_lines, counts, archive_blocks, archive_long)

    if args.stdout:
        sys.stdout.write(index)
        return 0

    stale = (not index_path.exists()) or index_path.read_bytes().decode("utf-8") != index
    if not args.check:
        index_path.write_bytes(index.encode("utf-8"))

    over = long_lines + archive_long
    archive_note = ""
    if archive_blocks:
        archive_note = (f"; archive {len(archive_blocks)} blocks over "
                        f"{sum(b['bytes'] for b in archive_blocks):,} bytes")
    print(f"items {len(items)}; marks fixed {counts['fixed']} partial {counts['partial']} "
          f"open {counts['open']}{archive_note}")
    print(f"lines over {MAX_LINE_CHARS}: {len(over)}")
    for name, rows in (("LEDGER.md", long_lines), ("LEDGER-ARCHIVE.md", archive_long)):
        for line_no, length in rows:
            print(f"  {name}:{line_no} {length} characters")
    print(f"index: {len(index.split(chr(13) + chr(10)))} lines, {len(index.encode()):,} bytes"
          + (" (stale, not written with --check)" if args.check and stale else ""))
    if args.check and (stale or over):
        print("FAILED: " + ("index is stale; regenerate it" if stale else "")
              + (" and " if stale and over else "")
              + ("lines are over the limit" if over else ""))
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
