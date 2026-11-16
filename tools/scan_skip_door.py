r"""Print the shared cut-in skip door counter as it moves through one log.

`SingleModeTrainingCutInHelper::SkipRuntime()` is the door the Grand Live lever clicks and the
door the training cut-ins share. The live lever's census line carries that door's session total,
so reading the census lines in order shows which stay the crossings happened in.

Usage: python tools/scan_skip_door.py <log> [more logs...]
"""

import re
import sys

CENSUS = re.compile(
    r"Grand Live skip census at (?P<sec>\d+) s:.*?(?P<clicks>\d+) clicks this build made on "
    r"`SingleModeTrainingCutInHelper::SkipRuntime\(\)` across (?P<ours>\d+) crossings of it this module watched, "
    r"(?P<cross>\d+) crossings of that door"
)
VIEW = re.compile(r"next_view_id = (?P<view>\d+)")
RESET = re.compile(r"Soft reset requested from the menu")


def main(argv):
    for path in argv:
        print("== %s" % path)
        last = 0
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            for number, raw in enumerate(handle, 1):
                line = raw.rstrip("\n")
                match = CENSUS.search(line)
                if match:
                    cross = int(match.group("cross"))
                    note = "" if cross == last else "  <-- door moved by %d" % (cross - last)
                    print(
                        "  :%-7s census %5s s  our clicks %s  our watch %s  door total %d%s"
                        % (number, match.group("sec"), match.group("clicks"), match.group("ours"), cross, note)
                    )
                    last = cross
                    continue
                if VIEW.search(line) or RESET.search(line):
                    text = VIEW.search(line)
                    if text:
                        print("  :%-7s view %s" % (number, text.group("view")))
                    else:
                        print("  :%-7s SOFT RESET" % number)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
