#!/usr/bin/env bash
# Mutation ladder for the dead-letter queue report.
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the unknown --target is accepted again (the refusal is removed)
#   M2  `pending` counts the rows shown again, not the letters pending
#   M3  an orphaned letter is dropped from the report again
#   M4  a replay with an orphaned letter reads clean again (exit 0)
#   M5  the refusal no longer names the offending value
#
# Usage: tools/mutation-ladder-dlq-target-domain.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"
TEST="dlq_target_domain"

# The tree must come back byte-identical: a ladder that leaves a mutation
# behind is worse than no ladder.
#
# The restore must NOT use `git checkout`: that resets to HEAD and would
# DESTROY work that is not committed yet (the fix under test is often
# uncommitted — that is the normal state of a card in progress). The
# ladder therefore snapshots the tree it was given and puts THAT back.
SNAPSHOT=""
before=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)

snapshot_tree() {
  SNAPSHOT=$(mktemp -d)
  mkdir -p "$SNAPSHOT/src" "$SNAPSHOT/tests"
  cp -a src/. "$SNAPSHOT/src/" 2>/dev/null || true
  cp -a tests/. "$SNAPSHOT/tests/" 2>/dev/null || true
}

restore() {
  if [ -n "$SNAPSHOT" ] && [ -d "$SNAPSHOT/src" ]; then
    cp -a "$SNAPSHOT/src/." src/ 2>/dev/null || true
    cp -a "$SNAPSHOT/tests/." tests/ 2>/dev/null || true
    # `cp -a` preserves the snapshot's OLD mtimes, so cargo's fingerprint
    # sees nothing changed and keeps the MUTATED binary — a measurement run
    # right after the ladder would then measure the last mutation and blame
    # the fix. Bump the mtimes so the next build recompiles the real tree.
    find src tests -type f -name '*.rs' -exec touch {} + 2>/dev/null || true
  else
    echo "restore: no snapshot — refusing to fall back to git checkout (it would destroy uncommitted work)"
    return 1
  fi
}

trap 'restore >/dev/null 2>&1 || true; [ -n "$SNAPSHOT" ] && rm -rf "$SNAPSHOT"' EXIT

run_ladder() {
  cargo test --release --test "$TEST" 2>&1 | grep -E 'test result:' | tail -1
}

echo "=== control: the unmutated tree must be green"
snapshot_tree
control=$(run_ladder)
echo "$control"
if ! echo "$control" | grep -q '0 failed'; then
  echo "CONTROL FAILED — the tree is not green before the ladder runs"
  exit 2
fi

caught=0
total=0

check_mutation() {
  local name="$1"
  total=$((total + 1))
  echo ""
  echo "=== $name"
  local out
  out=$(run_ladder)
  echo "$out"
  if echo "$out" | grep -q '0 failed'; then
    echo "SURVIVED: $name was not caught"
    restore
    return 1
  fi
  echo "caught: $name"
  restore
  caught=$((caught + 1))
  return 0
}

# --- M1: the unknown --target is accepted again ------------------------------
cp src/main.rs /tmp/reeflog-m1-main.rs
python3 - <<'PY'
import re
p = 'src/main.rs'
s = open(p).read()
before = s
# Remove the refusal in dlq list: the whole `if !in_config && !in_journal { ... }` block
s = s.replace("""        if !in_config && !in_journal {
            let known = targets
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            die(format!(
                "flag --target got `{id}` — no such target: not in this config and no pending dead letters under that id (known targets: {known})"
            ));
        }""", """        let _ = (in_config, in_journal);""")
assert s != before, "M1 did not change the tree"
open(p, 'w').write(s)
PY
check_mutation "M1 the unknown --target is accepted again" || true

# --- M2: pending counts the rows shown again ---------------------------------
cp src/main.rs /tmp/reeflog-m2-main.rs
python3 - <<'PY'
p = 'src/main.rs'
s = open(p).read()
before = s
s = s.replace("""        let pending = j
            .webhook_dead_letter_count(&t.id)
            .unwrap_or_else(|e| die(e));
        let rows = j
            .webhook_dead_letters(&t.id, limit)
            .unwrap_or_else(|e| die(e));
        total += pending;""", """        let rows = j
            .webhook_dead_letters(&t.id, limit)
            .unwrap_or_else(|e| die(e));
        let pending = rows.len() as i64;
        total += pending;""", 1)
assert s != before, "M2 did not change the tree"
open(p, 'w').write(s)
PY
check_mutation "M2 pending counts the rows shown again" || true

# --- M3: an orphaned letter is dropped from the report again ------------------
cp src/main.rs /tmp/reeflog-m3-main.rs
python3 - <<'PY'
p = 'src/main.rs'
s = open(p).read()
before = s
s = s.replace("""    for id in &journal_pending {
        if targets.iter().any(|t| &t.id == id) {
            continue;
        }""", """    for id in &journal_pending {
        if true {
            continue;
        }""", 1)
assert s != before, "M3 did not change the tree"
open(p, 'w').write(s)
PY
check_mutation "M3 an orphaned letter is dropped from the report" || true

# --- M4: a replay with an orphaned letter reads clean again -------------------
cp src/main.rs /tmp/reeflog-m4-main.rs
python3 - <<'PY'
p = 'src/main.rs'
s = open(p).read()
before = s
s = s.replace("""    if orphaned > 0 {
        eprintln!(
            "reeflog: {orphaned} dead letter(s) belong to target(s) no longer in this config — add them back to replay"
        );
        std::process::exit(3);
    }""", """    if false {
        eprintln!("unreachable");
    }""", 1)
assert s != before, "M4 did not change the tree"
open(p, 'w').write(s)
PY
check_mutation "M4 a replay with an orphaned letter reads clean" || true

# --- M5: the refusal no longer names the offending value ---------------------
cp src/main.rs /tmp/reeflog-m5-main.rs
python3 - <<'PY'
p = 'src/main.rs'
s = open(p).read()
before = s
s = s.replace(
  '"flag --target got `{id}` — no such target: not in this config and no pending dead letters under that id (known targets: {known})"',
  '"flag --target is not a target of this command"',
  1)
assert s != before, "M5 did not change the tree"
open(p, 'w').write(s)
PY
check_mutation "M5 the refusal no longer names the value" || true

echo ""
echo "=== the tree must be byte-identical again"
after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
  echo "THE TREE CHANGED — a mutation was left behind"
  exit 1
fi
echo "byte-identical"

echo ""
echo "=== $caught/$total mutations caught"
[ "$caught" -eq "$total" ] || exit 1
rm -rf "$SNAPSHOT"
SNAPSHOT=""
exit 0
