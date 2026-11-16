r"""Print the Succession visit timeline of one hachimi.log.

Lists, in log order, the doors the fork's probe holds on SingleModeSuccessionEventViewController, the lever's own
Inspiration lines, the menu soft resets, the view steps into and out of 1500/1501, and the census lines whose controller
first appears in that visit, each with its log line number and time. Reads only.

Usage: python tools\scan_succession_visits.py <path to hachimi.log>
"""

import re
import sys
from datetime import datetime

TS = re.compile(r"^(\d{4}-\d\d-\d\dT(\d\d:\d\d:\d\d)[\d.]+)Z \[(\w+)\](?: \(\d+\))? (.+?): (.*)$")
DOOR = re.compile(r"Cutt probe SingleModeSuccessionEventViewController::(\w+)\(([^)]*)\) call (\d+)")
LEVER = re.compile(r"Inspiration (skip|re-ask|ask|watch|auto click)")
CENSUS = re.compile(r"Cut-in stall census: view (\d+) controller (0x[0-9a-f]+) seen (\d+) ms, (\d+) UpdateSpeed calls, current time ([-\d.]+), total ([-\d.]+)")
VIEW = re.compile(r"next_view_id = (\d+)")


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1

    rows = []
    with open(sys.argv[1], "r", encoding="utf-8", errors="replace") as handle:
        for number, line in enumerate(handle, 1):
            match = TS.match(line.rstrip("\n"))
            if match:
                rows.append((number, match.group(2), match.group(4), match.group(5)))

    visit = None
    shown = {}

    for number, clock, source, text in rows:
        door = DOOR.search(text)
        view = VIEW.search(text)
        census = CENSUS.search(text)

        if door:
            name, call = door.group(1), door.group(3)

            if name == "InitializeView":
                visit = int(call)
                shown = {}
                print("\nvisit %d  (log :%d at %s)" % (visit, number, clock))

            print("  :%-6s %s  door %-26s call %s" % (number, clock, name, call))
            continue

        if LEVER.search(text) and ("TrainingCuttProbe" in source or "Succession" in source):
            print("  :%-6s %s  lever %s" % (number, clock, text[:196]))
            continue

        if "Soft reset" in text:
            print("  :%-6s %s  RESET %s" % (number, clock, text[:80]))
            continue

        if view and view.group(1) in ("1500", "1501"):
            print("  :%-6s %s  view step to %s" % (number, clock, view.group(1)))
            continue

        if census and census.group(1) in ("1500", "1501"):
            seen, calls, current, total = census.group(3), census.group(4), census.group(5), census.group(6)
            past = float(current) > float(total)
            key = (visit, census.group(2))

            # One line when a controller starts, one when it crosses its own total, one when it is still past it at
            # the last sample the log holds for it.
            if shown.get(key) == past:
                continue

            shown[key] = past
            print("  :%-6s %s  census %s seen %5s ms ticks %5s current %6s of total %6s %s paused %s"
                  % (number, clock, census.group(2), seen, calls, current, total, "PAST" if past else "  ok  ",
                     "true" if "paused true" in text else "false"))

    return 0


if __name__ == "__main__":
    sys.exit(main())
