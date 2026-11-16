r"""Print a line range of a hachimi.log in a short, greppable form.

Usage: python tools/dump_log_window.py <log> <first_line> <last_line> [--grep PATTERN] [--width N]

Drops the shared timestamp and module prefix so a whole game-side window fits on screen, and
keeps the log line number on every row. With --grep only lines whose text matches the pattern
are printed, so a stuck screen can be read with its probe noise cut away.
"""

import re
import sys

PREFIX = re.compile(r"^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})\.\d+Z \[(?:INFO|DEBUG|WARN|ERROR)\]( \(\d+\))? hachimi::[A-Za-z:]*::")


def main(argv):
    if len(argv) < 3:
        print("usage: dump_log_window.py <log> <first_line> <last_line> [--grep PATTERN] [--width N]")
        return 2
    path, first, last = argv[0], int(argv[1]), int(argv[2])
    pattern = None
    width = 170
    index = 3
    while index < len(argv):
        if argv[index] == "--grep":
            pattern = re.compile(argv[index + 1])
            index += 2
        elif argv[index] == "--width":
            width = int(argv[index + 1])
            index += 2
        else:
            index += 1
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for number, raw in enumerate(handle, 1):
            if number < first:
                continue
            if number > last:
                break
            line = PREFIX.sub(lambda match: match.group(1)[11:] + " ", raw.rstrip("\n"))
            if pattern and not pattern.search(line):
                continue
            if len(line) > width:
                line = line[:width] + " ..."
            print(":%-6s %s" % (number, line))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
