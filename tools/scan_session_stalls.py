"""Read one hachimi.log and print the stall shape of that session.

Usage: python tools/scan_session_stalls.py <path to hachimi.log>

Prints, in order: the build and config lines the run stamped, how many game
inits the log holds, the soft resets, the view dwell table, the stall watchdog
hits without their door dumps, every Grand Live skip lever line that is not a
census, the last census, the concert probe visits on view 1621, and the story
cut-in totals attributed to 1621. Line numbers are lines of the log file.
"""

import re
import sys
from datetime import datetime, timedelta

TS = re.compile(r"^(\d{4}-\d\d-\d\dT[\d:.]+)Z \[(\w+)\](?: \(\d+\))? (.+?): (.*)$")
WATCHDOG = re.compile(r"Stall watchdog: view (\d+) stood (\d+) s, (\d+) of (\d+) doors")
VIEW_STEP = re.compile(r"next_view_id = (\d+)")
CUT_ATTR = re.compile(r"other\(view 1621\)([^|]*)")


def parse(path):
    rows = []
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for number, line in enumerate(handle, 1):
            match = TS.match(line.rstrip("\n"))
            if match:
                stamp = datetime.fromisoformat(match.group(1))
                rows.append((number, stamp, match.group(3).split("::")[-1], match.group(4)))
    return rows


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    rows = parse(sys.argv[1])
    if not rows:
        print("no timestamped lines")
        return 1

    first, last = rows[0][1], rows[-1][1]
    print("session %s to %s (%.0f s), %d timestamped lines"
          % (first.isoformat(timespec="seconds"), last.isoformat(timespec="seconds"),
             (last - first).total_seconds(), len(rows)))

    for label, pattern in (("build", "Hachimi v"), ("config", "Config snapshot:"),
                           ("hooking", "Hooking finished"), ("init count", "Loaded library")):
        hits = [row for row in rows if pattern in row[3]]
        if label == "init count":
            print("game inits in this log: %d" % len(hits))
        else:
            for row in hits[:2]:
                print("%-9s %s:%s %s" % (label, row[0], row[1].strftime("%H:%M:%S"), row[3][:150]))

    print("\nsoft resets and other fork writes that end a screen")
    for row in rows:
        if "soft reset" in row[3].lower() or "SoftReset" in row[3]:
            print("  %s %s %s: %s" % (row[0], row[1].strftime("%H:%M:%S"), row[2], row[3][:120]))

    print("\nview dwells (from next_view_id steps)")
    steps = [(row[0], row[1], VIEW_STEP.search(row[3]).group(1)) for row in rows if VIEW_STEP.search(row[3])]
    for index, (number, stamp, view) in enumerate(steps):
        if index + 1 < len(steps):
            nxt_number, nxt_stamp, nxt_view = steps[index + 1]
            seconds = (nxt_stamp - stamp).total_seconds()
            print("  view %-5s open %s to %s  %.1f s (log :%s to :%s)"
                  % (view, stamp.strftime("%H:%M:%S"), nxt_stamp.strftime("%H:%M:%S"),
                     seconds, number, nxt_number))
        else:
            print("  view %-5s open %s, log ends %s with it open (log :%s)"
                  % (view, stamp.strftime("%H:%M:%S"), last.strftime("%H:%M:%S"), number))

    print("\nstall watchdog hits")
    for row in rows:
        match = WATCHDOG.search(row[3])
        if match:
            print("  %s %s view %-5s stood %s s, %s of %s doors crossed"
                  % (row[0], row[1].strftime("%H:%M:%S"), match.group(1), match.group(2),
                     match.group(3), match.group(4)))

    print("\nGrand Live skip lever lines (census apart)")
    census = [row for row in rows if "Grand Live skip census" in row[3]]
    for row in rows:
        if "Grand Live skip" in row[3] and "census" not in row[3]:
            print("  %s %s %s" % (row[0], row[1].strftime("%H:%M:%S"), row[3][:230]))
    if census:
        print("  last census %s %s:%s" % (census[-1][0], census[-1][1].strftime("%H:%M:%S"), census[-1][3][:460]))

    print("\nconcert probe visits")
    for row in rows:
        if "Concert visit" in row[3]:
            print("  %s %s %s" % (row[0], row[1].strftime("%H:%M:%S"), row[3][:200]))

    print("\nstory cut-in runs attributed to view 1621")
    for row in rows:
        match = CUT_ATTR.search(row[3])
        if match:
            print("  %s %s %s" % (row[0], row[1].strftime("%H:%M:%S"), match.group(0)[:150]))

    print("\nlast frame clock totals")
    clocks = [row for row in rows if "Frame clock totals at" in row[3]]
    for row in clocks[-1:]:
        print("  %s %s %s" % (row[0], row[1].strftime("%H:%M:%S"), row[3][:200]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
