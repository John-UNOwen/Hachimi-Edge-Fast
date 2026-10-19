#!/usr/bin/env bash
# Cycle test for tools/knowledge-snapshot.sh. It never touches the project's LEDGER.md, its
# LEDGER-ARCHIVE.md or their history: it copies both tools, a ledger and a small archive into
# target/scratch/ks-test and points KNOWLEDGE_HISTORY at a throwaway history inside that folder.
#
#   bash tools/test_knowledge_snapshot.sh
set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRATCH="$ROOT/target/scratch/ks-test"
cd "$ROOT" || exit 1
test -f LEDGER.md || { echo "test_knowledge_snapshot: LEDGER.md is missing, nothing to copy"; exit 1; }
test -f tools/knowledge-snapshot.sh || { echo "test_knowledge_snapshot: the script under test is missing"; exit 1; }

rm -rf "$SCRATCH"
mkdir -p "$SCRATCH/tools"
cp tools/knowledge-snapshot.sh "$SCRATCH/tools/"
cp LEDGER.md "$SCRATCH/LEDGER.md"
cp LEDGER.md "$SCRATCH/pristine.md"
printf '# LEDGER archive\n\n## Item 1, moved by the cycle test\n\n- a receipt the test invented\n' > "$SCRATCH/LEDGER-ARCHIVE.md"
cp "$SCRATCH/LEDGER-ARCHIVE.md" "$SCRATCH/pristine-archive.md"

cd "$SCRATCH" || exit 1
export KNOWLEDGE_HISTORY="$SCRATCH/hist.git"
g() { git --git-dir="$KNOWLEDGE_HISTORY" --work-tree="$SCRATCH" "$@"; }
fails=0
check() { if [ "$1" = "$2" ]; then echo "ok   : $3"; else echo "FAIL : $3 (got '$1', wanted '$2')"; fails=$((fails + 1)); fi; }

out=$(bash tools/knowledge-snapshot.sh "cycle first"); check "$?" 0 "first snapshot exits 0"
echo "$out" | grep -q 'recorded' || { echo "FAIL : first snapshot recorded nothing"; fails=$((fails + 1)); }
check "$(g ls-tree --name-only HEAD | tr '\n' ' ')" "LEDGER-ARCHIVE.md LEDGER.md " \
  "the first snapshot stages the ledger and its archive"

out=$(bash tools/knowledge-snapshot.sh "cycle nothing changed")
check "$out" "no change since the last snapshot" "a snapshot with nothing edited reports no change"

printf 'a line added by the cycle test\n' >> LEDGER.md
bash tools/knowledge-snapshot.sh "cycle second" >/dev/null
printf '\n- a second receipt the test invented\n' >> LEDGER-ARCHIVE.md
bash tools/knowledge-snapshot.sh "cycle archive only" >/dev/null
check "$(g show --name-only --format= HEAD)" "LEDGER-ARCHIVE.md" "an edit to the archive alone is recorded on its own"
first=$(g rev-list --reverse HEAD | head -1)
out=$(bash tools/knowledge-snapshot.sh --restore "$first")
cmp -s pristine.md LEDGER.md
check "$?" 0 "--restore of the first snapshot puts LEDGER.md back byte identical"
cmp -s pristine-archive.md LEDGER-ARCHIVE.md
check "$?" 0 "--restore puts LEDGER-ARCHIVE.md back byte identical"

mkdir -p knowledge && printf 'C1 note\n' > knowledge/c1-note.md
g add -f -A -- knowledge && g commit -q -m "simulated pre-move snapshot"
premove=$(g rev-parse HEAD)
rm -rf knowledge
printf 'third edit\n' >> LEDGER.md
out=$(bash tools/knowledge-snapshot.sh "cycle after the move")
echo "$out" | grep -q 'the history still holds the knowledge/ tree' \
  || { echo "FAIL : the drop of the pre-move half was not named"; fails=$((fails + 1)); }
check "$(g ls-tree --name-only HEAD | tr '\n' ' ')" "LEDGER-ARCHIVE.md LEDGER.md " \
  "the snapshot after the move stages the two ledger files and no bundle"
check "$(g ls-tree --name-only "$premove" | tr '\n' ' ')" "LEDGER-ARCHIVE.md LEDGER.md knowledge " \
  "the older snapshot keeps its knowledge half"

out=$(bash tools/knowledge-snapshot.sh --restore "$premove")
echo "$out" | grep -q 'also holds the knowledge/ tree' \
  || { echo "FAIL : the restore of a knowledge snapshot did not name that half"; fails=$((fails + 1)); }
check "$(test -d knowledge && echo yes || echo no)" "no" "that restore wrote no knowledge/ back into the project"

mv LEDGER-ARCHIVE.md aside-arch.md
bash tools/knowledge-snapshot.sh "cycle with no archive" >/dev/null 2>&1
check "$?" 1 "a snapshot refuses while the history tracks LEDGER-ARCHIVE.md and the disk does not hold it"
mv aside-arch.md LEDGER-ARCHIVE.md

rm -f LEDGER-ARCHIVE.md
g add -f -A -- LEDGER-ARCHIVE.md && g commit -q -m "archive dropped from the history"
bash tools/knowledge-snapshot.sh "cycle after the drop" >/dev/null 2>&1
check "$?" 0 "once the history no longer tracks the archive, a snapshot runs without it"
check "$(g ls-tree --name-only HEAD | tr '\n' ' ')" "LEDGER.md " "and that snapshot holds LEDGER.md alone"

mv LEDGER.md aside.md
bash tools/knowledge-snapshot.sh "cycle with no ledger" >/dev/null 2>&1
check "$?" 1 "a snapshot while LEDGER.md is missing refuses"
mv aside.md LEDGER.md

rm -rf "$SCRATCH"
if [ "$fails" -eq 0 ]; then
  echo "knowledge-snapshot cycle test: all checks passed"
else
  echo "knowledge-snapshot cycle test: $fails check(s) failed"
  exit 1
fi
