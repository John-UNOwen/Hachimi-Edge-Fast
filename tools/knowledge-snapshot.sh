#!/usr/bin/env bash
# Snapshots LEDGER.md and LEDGER-ARCHIVE.md into a local git history that lives outside this repository.
#
#   bash tools/knowledge-snapshot.sh ["what changed"]   record both files (default message: the UTC time)
#   bash tools/knowledge-snapshot.sh --log              list every snapshot with the files it changed
#   bash tools/knowledge-snapshot.sh --show [rev]       what one snapshot changed (default HEAD)
#   bash tools/knowledge-snapshot.sh --restore [rev]    put both files back as they were at rev (default HEAD)
#
# LEDGER-ARCHIVE.md holds the dated receipts that were moved out of the live items. It carries no item of its
# own: an item stays in LEDGER.md and names the block it moved to. A history that predates 2026-10-18 simply
# has no archive file, and a restore from one touches LEDGER.md alone.
#
# The OKF bundle is not this script's job any more. It left the project on 2026-10-13 and is now
# $HOME/knowledge/projects/hachimi-edge, a folder of a git-tracked knowledge base with its own history,
# its own index.md and log.md, and no remote. A snapshot taken before that move also holds a knowledge/
# tree: it stays in the history untouched, and this script drops it from the set it stages.
#
# The history has no remote and must never get one. Its git directory is $HOME/.hachimi-knowledge-history.git,
# or $KNOWLEDGE_HISTORY when set. It sits outside this repository, so `git clean -xdff` here cannot reach it.
# This repository's git never stages LEDGER.md; only this script does, and only into that history.
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HIST="${KNOWLEDGE_HISTORY:-$HOME/.hachimi-knowledge-history.git}"
cd "$ROOT"

g() { git --git-dir="$HIST" --work-tree="$ROOT" "$@"; }
die() { echo "knowledge-snapshot: $*" >&2; exit 1; }

if [ ! -d "$HIST" ]; then
  git init -q --bare "$HIST"
  echo "created the history at $HIST"
fi
git --git-dir="$HIST" config core.autocrlf false
# The project's .gitattributes (`* text=auto`) still applies to this history, because it reads the work tree.
# info/attributes takes precedence over it: `-text` stores each file exactly as it is on disk, and `diff`
# keeps the diffs textual.
mkdir -p "$HIST/info"
printf '* -text diff\n' > "$HIST/info/attributes"

snapshot() {
  test -f LEDGER.md || die "LEDGER.md is missing, so its deletion is not recorded"
  local files=(LEDGER.md)
  if test -f LEDGER-ARCHIVE.md || g cat-file -e "HEAD:LEDGER-ARCHIVE.md" 2>/dev/null; then
    test -f LEDGER-ARCHIVE.md || die "LEDGER-ARCHIVE.md is missing, so its deletion is not recorded"
    files+=(LEDGER-ARCHIVE.md)
  fi
  if g rev-parse -q --verify HEAD >/dev/null && g cat-file -e HEAD:knowledge 2>/dev/null; then
    # The pre-move snapshots keep their bundle half; this script stops staging it from here on.
    g rm -r -q --cached knowledge
    echo "note: the history still holds the knowledge/ tree of its older snapshots; that half is left in"
    echo "      place, and the knowledge base at \$HOME/knowledge versions the bundle itself"
  fi
  g add -f -A -- "${files[@]}"
  if g diff --cached --quiet; then
    echo "no change since the last snapshot"
    return 0
  fi
  g commit -q -m "$1"
  g log -1 --format='recorded %h %s'
}

case "${1:-}" in
  --log)
    if g rev-parse -q --verify HEAD >/dev/null; then
      g log --date=format:'%Y-%m-%d %H:%M' --format='%h %ad %s' --stat
    else
      echo "no snapshots yet"
    fi ;;
  --show)
    name="${2:-HEAD}"
    rev="$(g rev-parse -q --verify "$name^{commit}")" || die "no snapshot named $name"
    g show --stat --date=format:'%Y-%m-%d %H:%M' --format='%h %ad %s' "$rev" ;;
  --restore)
    name="${2:-HEAD}"
    rev="$(g rev-parse -q --verify "$name^{commit}")" || die "no snapshot named $name"
    g cat-file -e "$rev:LEDGER.md" 2>/dev/null || die "$name does not hold LEDGER.md"
    files=(LEDGER.md)
    if g cat-file -e "$rev:LEDGER-ARCHIVE.md" 2>/dev/null; then
      files+=(LEDGER-ARCHIVE.md)
    elif test -f LEDGER-ARCHIVE.md; then
      echo "note: $name has no LEDGER-ARCHIVE.md, so this restore leaves the one on disk where it is."
      echo "      A snapshot from before 2026-10-18 holds its receipts inside LEDGER.md, so restoring one"
      echo "      puts them back there as well as leaving them in the archive."
    fi
    if g cat-file -e "$rev:knowledge" 2>/dev/null; then
      echo "note: $name also holds the knowledge/ tree the pre-move snapshots recorded; the ledger files are"
      echo "      all this restore touches, and the bundle itself is at \$HOME/knowledge/projects/hachimi-edge"
    fi
    if test -f LEDGER.md; then
      snapshot "before restoring $name"   # the current state stays reachable in the history
    fi
    rm -f "${files[@]}"
    g checkout -q "$rev" -- "${files[@]}"
    echo "restored ${files[*]} from $name" ;;
  "")
    snapshot "snapshot $(date -u +%Y-%m-%dT%H:%MZ)" ;;
  *)
    snapshot "$1" ;;
esac
