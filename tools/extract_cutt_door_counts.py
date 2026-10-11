"""Read the training cut-in door counts out of this fork's own run logs.

The numbers LEDGER items 93, 100 and 102 and the C64 concept quote come from here. It answers one
question per door: did the client call it, how many times at most, and what value did the door carry.
A door with no match is a door that was installed and never called, which is the A4 case: installed is
not the same as called. Run it as `python tools/extract_cutt_door_counts.py`, optionally naming run
log files.
"""

import pathlib
import re
import sys

# Doors on the training cut-in path, each written the way the `Cutt probe totals` line prints it.
DOORS = [
    "CutInTimelineController::UpdateSpeed()",
    "CutInTimelineController::ResetCurrentTime()",
    "CutInTimelineController::SkipRuntime(time)",
    "CutInTimelineController::SkipRuntime(frames, keep)",
    "CutInTimelineController::SkipTimeDirect(time)",
    "SingleModeMainTrainingCuttController::set_IsPlayingCutt(playing)",
    "SingleModeMainTrainingCuttController::get_IsPlayingCutt()",
    "SingleModeTrainingCutInHelper::SkipRuntime()",
    "SingleModeMainViewTrainingCutStatus::Skip(skip)",
    "SingleModeMainViewTagTrainingCutInPlayer::PlayCutIn(cards, done)",
    "SingleModeMainViewTagTrainingCutInPlayer::PlayCutInOut(done)",
    "SingleModeUtils::GetTrainingCutTimeScale(scale)",
]


def run_logs(argv):
    if argv:
        return [pathlib.Path(a) for a in argv]
    return sorted(pathlib.Path("run log").glob("hachimi-run*.log"), reverse=True)


def main():
    logs = run_logs(sys.argv[1:])
    if not logs:
        print("no run logs found")
        return 1

    for log in logs:
        text = log.read_text(encoding="utf-8", errors="replace")
        print("== %s (%d lines)" % (log.name, text.count("\n")))
        for door in DOORS:
            matches = list(re.finditer(re.escape(door) + r"=(\d+)(?: peak ([0-9.]+))?", text))
            if not matches:
                print("   %-62s never called" % door)
                continue
            counts = [int(m.group(1)) for m in matches]
            peaks = sorted({float(m.group(2)) for m in matches if m.group(2)})
            extra = "  values " + "/".join(str(p) for p in peaks) if peaks else ""
            print("   %-62s max %4d  (first %d)%s" % (door, max(counts), counts[0], extra))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
