#!/usr/bin/env bash
# Snapshots knowledge/ and LEDGER.md into a local git history that lives outside this repository.
#
#   bash tools/knowledge-snapshot.sh ["what changed"]   record both paths (default message: the UTC time)
#   bash tools/knowledge-snapshot.sh --log              list every snapshot with the files it changed
#   bash tools/knowledge-snapshot.sh --show [rev]       what one snapshot changed (default HEAD)
#   bash tools/knowledge-snapshot.sh --restore [rev]    put both paths back as they were at rev (default HEAD)
#
# The history has no remote and must never get one. Its git directory is $HOME/.hachimi-knowledge-history.git,
# or $KNOWLEDGE_HISTORY when set. It sits outside this repository, so `git clean -xdff` here cannot reach it.
# This repository's git never stages these paths; only this script does, and only into that history.
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
  test -d knowledge && test -f LEDGER.md || die "knowledge/ or LEDGER.md is missing, so their deletion is not recorded"
  g add -f -A -- knowledge LEDGER.md
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
    { g cat-file -e "$rev:knowledge" && g cat-file -e "$rev:LEDGER.md"; } 2>/dev/null \
      || die "$name does not hold both knowledge/ and LEDGER.md"
    if test -d knowledge || test -f LEDGER.md; then
      test -d knowledge && test -f LEDGER.md \
        || die "only one of knowledge/ and LEDGER.md exists; move it aside, then run --restore again"
      snapshot "before restoring $name"   # the current state stays reachable in the history
    fi
    rm -rf knowledge LEDGER.md
    g checkout -q "$rev" -- knowledge LEDGER.md
    echo "restored knowledge/ and LEDGER.md from $name" ;;
  "")
    snapshot "snapshot $(date -u +%Y-%m-%dT%H:%MZ)" ;;
  *)
    snapshot "$1" ;;
esac
