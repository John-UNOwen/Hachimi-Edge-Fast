"""Which class owns each method line in this client's metadata inventory.

`doors.py` next to it prints the whole surface of classes you name. This answers the other question, the one a hook
write starts with: a method name is known and the class that carries it is not. It walks the inventory in file order,
keeps the last `### image :: Class` header it saw, and prints every method or field line matching a pattern under the
class that owns it. Names come from `out/metadata/inventory.txt`; unresolved types print as `type#N`, which is the
metadata side's limit and not a gap here (C80).

  python tools/find_method_owner.py "C:\\Users\\Pure Fox\\static-analysis\\hachimi-edge\\out\\metadata\\inventory.txt" "SetMarkerVisible|VisibleMarkers"
"""

import pathlib
import re
import sys

USAGE = "usage: find_method_owner.py <inventory.txt> <pattern>"


def main() -> int:
    if len(sys.argv) != 3:
        print(USAGE)
        return 2

    inventory = pathlib.Path(sys.argv[1])
    if not inventory.is_file():
        print(f"no inventory at {inventory}")
        return 1

    try:
        pattern = re.compile(sys.argv[2])
    except re.error as error:
        print(f"bad pattern: {error}")
        return 2

    current = None
    hits = 0
    for line in inventory.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith("### "):
            current = line[4:]
        elif pattern.search(line):
            hits += 1
            print(f"{current} :: {line.strip()}")

    if hits == 0:
        print(f"nothing under any class matches {sys.argv[2]!r}")
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
