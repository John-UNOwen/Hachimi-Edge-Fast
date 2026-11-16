r"""Read one hachimi.log and print every stay on a live view as a ledger.

Usage: python tools/scan_live_visits.py <log> [more logs...] [--view 1621] [--red-seconds 15]

A stay starts at the `next_view_id = <view>` line and ends at the next view change, at a
menu soft reset, or at the end of the log. For each stay it prints how long the game sat
there, what the Grand Live lever printed inside it (cut-in window opened, armed, pressed),
how far into the stay the lever pressed, whether the player left with a soft reset and how
many seconds after the press that reset came, and the last `Concert visit` census line it
saw, which carries `can skip live answered` and the live skip door crossings.

Exit code 1 when a stay on that view got a lever press, never printed the live's own
`PhaseLiveOutWipe` phase, and was then left by a soft reset within --red-seconds of that
press: the shape the player reports as a stuck concert. Exit code 0 otherwise. With several
logs it prints one ledger per log and a rate line for each, so a session can be read against
the sessions before it.
"""

import re
import sys
from datetime import datetime, timedelta

LINE = re.compile(r"^(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z ")
VIEW = re.compile(r"next_view_id = (?P<view>\d+)")
RESET = re.compile(r"Soft reset requested from the menu")
OPENED = re.compile(r"Grand Live skip cut opened (?P<n>\d+) on (?P<helper>0x[0-9a-f]+)")
ARMED = re.compile(r"Grand Live skip armed:")
PRESS = re.compile(r"Grand Live skip press (?P<n>\d+) .*? (?P<ms>\d+) ms after the live.s cut-in opened")
HELD = re.compile(r"Grand Live skip gate held")
CONCERT = re.compile(r"Concert visit \d+: view (?P<view>\d+) stood (?P<sec>\d+) s")
PHASE = re.compile(r"Grand Live skip phase \d+ of this visit: (?P<name>[A-Za-z]+) on ")
CENSUS = re.compile(
    r"Grand Live skip census at (?P<sec>\d+) s:.*?(?P<clicks>\d+) clicks this build made on "
    r"`SingleModeTrainingCutInHelper::SkipRuntime\(\)` across (?P<ours>\d+) crossings of it this module watched, "
    r"(?P<cross>\d+) crossings of that door"
)
STOOD = re.compile(r"Stall watchdog: view (?P<view>\d+) stood (?P<sec>\d+) s")


def stamp(line):
    match = LINE.match(line)
    if not match:
        return None
    text = match.group("ts")
    return datetime.fromisoformat(text[:23])


def parse(path):
    stays = []
    current = None
    census = []
    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for number, raw in enumerate(handle, 1):
            line = raw.rstrip("\n")
            when = stamp(line)
            if when is None:
                continue
            view = VIEW.search(line)
            if view:
                if current is not None:
                    current["end"] = when
                    current["end_line"] = number
                    current["end_kind"] = "view change to " + view.group("view")
                    stays.append(current)
                    current = None
                current = {
                    "view": int(view.group("view")),
                    "start": when,
                    "start_line": number,
                    "end": None,
                    "end_line": None,
                    "end_kind": "log end",
                    "opened": [],
                    "armed": [],
                    "pressed": [],
                    "held": 0,
                    "phases": [],
                    "concert": [],
                    "watchdog": [],
                    "reset": None,
                }
                continue
            if current is None:
                match = CENSUS.search(line)
                if match:
                    census.append(
                        (
                            number,
                            when,
                            int(match.group("clicks")),
                            int(match.group("ours")),
                            int(match.group("cross")),
                        )
                    )
                continue
            match = CENSUS.search(line)
            if match:
                census.append(
                    (
                        number,
                        when,
                        int(match.group("clicks")),
                        int(match.group("ours")),
                        int(match.group("cross")),
                    )
                )
            if RESET.search(line):
                current["end"] = when
                current["end_line"] = number
                current["end_kind"] = "soft reset"
                current["reset"] = when
                stays.append(current)
                current = None
                continue
            opened = OPENED.search(line)
            if opened:
                current["opened"].append((number, when, opened.group("helper")))
                continue
            if ARMED.search(line):
                current["armed"].append((number, when))
                continue
            press = PRESS.search(line)
            if press:
                current["pressed"].append((number, when, int(press.group("ms")), int(press.group("n"))))
                continue
            if HELD.search(line):
                current["held"] += 1
                continue
            phase = PHASE.search(line)
            if phase:
                current["phases"].append(phase.group("name"))
                continue
            concert = CONCERT.search(line)
            if concert and int(concert.group("view")) == current["view"]:
                current["concert"].append((number, int(concert.group("sec"))))
                continue
            watchdog = STOOD.search(line)
            if watchdog and int(watchdog.group("view")) == current["view"]:
                current["watchdog"].append((number, int(watchdog.group("sec"))))
    if current is not None:
        stays.append(current)
    return stays, census


def report(path, view, red_seconds):
    stays, census = parse(path)
    lives = [stay for stay in stays if stay["view"] == view]
    print("== %s: %d stays on view %d" % (path, len(lives), view))
    red = 0
    long_stays = 0
    pressed_no_wipe = 0
    presses_total = 0

    def counter(line_number, field):
        value = 0
        for entry in census:
            if entry[0] <= line_number:
                value = entry[field]
            else:
                break
        return value

    for stay in lives:
        end = stay["end"] or stay["start"]
        seconds = (end - stay["start"]).total_seconds()
        press_note = "no press"
        gap = None
        if stay["pressed"]:
            line, when, ms, index = stay["pressed"][0]
            presses_total += len(stay["pressed"])
            press_note = "press %d at +%d ms (%.1f s in)" % (index, ms, (when - stay["start"]).total_seconds())
            if stay["reset"] is not None:
                gap = (stay["reset"] - when).total_seconds()
        wipe = [name for name in stay["phases"] if "OutWipe" in name]
        # A build that printed no phase lines at all cannot be judged on the wipe.
        if stay["pressed"] and stay["phases"] and not wipe:
            pressed_no_wipe += 1
        if stay["pressed"] and stay["phases"] and not wipe and gap is not None and gap <= red_seconds:
            red += 1
            verdict = "RED pressed, no PhaseLiveOutWipe, reset %.1f s later" % gap
        elif stay["pressed"] and stay["phases"] and not wipe:
            verdict = "pressed, no PhaseLiveOutWipe, %s" % stay["end_kind"]
        elif seconds >= 30 and stay["reset"] is not None:
            verdict = "reset after %.1f s" % seconds
        elif seconds >= 30:
            verdict = "long stay, left on its own"
        else:
            verdict = "left on its own"
        if seconds >= 30:
            long_stays += 1
        opened = stay["opened"][0][1] if stay["opened"] else None
        open_note = "cut-in opened at +%d s" % (opened - stay["start"]).total_seconds() if opened else "no cut-in window"
        stood = max((pair[1] for pair in stay["concert"]), default=0)
        last_line = stay["end_line"] or stay["start_line"]
        cross_delta = counter(last_line, 4) - counter(stay["start_line"], 4)
        click_delta = counter(last_line, 2) - counter(stay["start_line"], 2)
        print(
            "  :%-7s %s  %.1f s  %s  %s  phases %s  %s  door crossings %d (census clicks %d)  stood %d s  -> %s"
            % (
                stay["start_line"],
                stay["start"].strftime("%H:%M:%S"),
                seconds,
                open_note,
                press_note,
                ",".join(stay["phases"]) or "none",
                stay["end_kind"],
                cross_delta,
                click_delta,
                stood,
                verdict,
            )
        )
    if census:
        number, when, clicks, ours, cross = census[-1]
        print(
            "  last census :%-7s %s  clicks %d  our crossings %d  door crossings %d"
            % (number, when.strftime("%H:%M:%S"), clicks, ours, cross)
        )
    resets = [stay for stay in lives if stay["reset"] is not None]
    no_wipe = [stay for stay in lives if stay["phases"] and not any("OutWipe" in name for name in stay["phases"])]
    print(
        "  rate: %d presses over %d stays, %d stays where a press was not followed by PhaseLiveOutWipe, %d RED, %d stays of 30 s or more"
        % (presses_total, len(lives), pressed_no_wipe, red, long_stays)
    )
    print(
        "  no out wipe: %d of %d stays printed the live phase chain but never PhaseLiveOutWipe"
        % (len(no_wipe), len([stay for stay in lives if stay["phases"]]))
    )
    print(
        "  left by menu reset: %d of %d stays (%d of them pressed by this fork)"
        % (len(resets), len(lives), len([stay for stay in resets if stay["pressed"]]))
    )
    return red


def main(argv):
    view = 1621
    red_seconds = 15
    logs = []
    skip = False
    for index, argument in enumerate(argv):
        if skip:
            skip = False
            view = int(argument)
            continue
        if argument == "--view":
            skip = True
            continue
        if argument == "--red-seconds":
            red_seconds = int(argv[index + 1])
            skip = True
            continue
        logs.append(argument)
    total = 0
    for path in logs:
        total += report(path, view, red_seconds)
    print("total RED stays: %d" % total)
    return 1 if total else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
