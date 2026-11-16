#!/usr/bin/env bash
# Mutation ladder for the `status` payload-debt exit fix (PULS AM).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the payload debt leaves the EXIT condition — the v0.7.30 behaviour,
#       where the report prints `payload_pending: 1` and calls it health
#   M2  `status` exits 3 unconditionally — the "always alarm" mirror,
#       caught by the clean-journal guard (a report that never says 0 is
#       not a report)
#   M3  a RECORDED outcome (unavailable/invalid) joins the EXIT condition —
#       no pass will ever fill those rows, so the verdict could never come
#       back down
#   M4  the opt-in account queue joins the EXIT condition — it drains only
#       behind `hydrate --accounts` / `tail --with-accounts`, so a default
#       consumer would carry a permanent alarm
#   M5  `gaps_open` joins the EXIT condition — the same class of permanent
#       false alarm v0.7.27 had to remove from this very field
#   M6  the stderr line drops the command that pays the debt — the verdict
#       arrives with no way to act on it
#
# Usage: tools/mutation-ladder-status-payload-debt-exit.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="status_payload_debt_exit"

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
    find src tests -type f -name '*.rs' -exec touch {} + 2>/dev/null || true
  else
    echo "restore: no snapshot — refusing to fall back to git checkout (it would destroy uncommitted work)"
    return 1
  fi
}

trap 'restore >/dev/null 2>&1 || true; [ -n "$SNAPSHOT" ] && rm -rf "$SNAPSHOT"' EXIT

snapshot_tree || exit 2

restore_all() {
  restore || exit 2
}

mutate() {
  local name="$1" test_fn="$2" path="$3" search="$4" replace="$5"

  echo -n "mutation: $name ... "
  python3 -c '
import sys
path, search, repl = sys.argv[1:4]
with open(path, "r") as f:
    text = f.read()
if search not in text:
    print(f"pattern not found: {search}", file=sys.stderr)
    sys.exit(2)
text = text.replace(search, repl, 1)
with open(path, "w") as f:
    f.write(text)
' "$path" "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing in $path: $search" >&2
        exit 1
    }

  if ! cargo build --release > /dev/null 2>&1; then
    echo "COMPILE ERROR (not a behavioural catch) — mutation must compile" >&2
    restore_all
    exit 1
  fi

  if cargo test --release --test "$TEST" -- "$test_fn" > /dev/null 2>&1; then
    echo "VACUOUS! (test passed under mutation)" >&2
    restore_all
    exit 1
  else
    echo "CAUGHT (went red, as expected)"
  fi
  restore_all
}

# M1: the payload debt leaves the EXIT condition — v0.7.30 behaviour again.
mutate "the payload debt leaves the exit condition" \
    "a_journal_that_owes_a_payload_exits_3_and_names_the_command_that_pays_it" \
    "src/main.rs" \
    '    if c.payload_pending > 0 {
        eprintln!(
            "reeflog: {} row(s) still owe their payload — the journal holds a signature, not what \
             happened; run `reeflog hydrate`",
            c.payload_pending
        );
        std::process::exit(3);
    }' \
    '    if false {
        eprintln!(
            "reeflog: {} row(s) still owe their payload — the journal holds a signature, not what \
             happened; run `reeflog hydrate`",
            c.payload_pending
        );
        std::process::exit(3);
    }'

# M2: status exits 3 UNCONDITIONALLY — the always-alarm mirror. The clean
# guard catches it (a report that never says 0 is not a report).
mutate "status exits 3 unconditionally" \
    "a_journal_without_payload_debt_still_exits_0" \
    "src/main.rs" \
    '    if c.payload_pending > 0 {' \
    '    if true || c.payload_pending > 0 {'

# M3: a RECORDED outcome joins the verdict — the alarm could never clear.
mutate "a recorded outcome joins the exit condition" \
    "a_recorded_outcome_is_not_a_debt" \
    "src/main.rs" \
    '    if c.payload_pending > 0 {' \
    '    if c.payload_pending > 0 || c.payload_unavailable > 0 || c.payload_invalid > 0 {'

# M4: the opt-in account queue joins the verdict — a permanent alarm for a
# default consumer.
mutate "the opt-in account queue joins the exit condition" \
    "the_opt_in_envelope_queues_do_not_set_the_exit_code" \
    "src/main.rs" \
    '    if c.payload_pending > 0 {' \
    '    if c.payload_pending > 0 || c.accounts_missing > 0 {'

# M5: gaps_open joins the verdict — the false alarm class v0.7.27 removed.
mutate "gaps_open joins the exit condition" \
    "an_open_gap_does_not_set_the_status_exit_code" \
    "src/main.rs" \
    '    if c.payload_pending > 0 {' \
    '    if c.payload_pending > 0 || c.gaps_open > 0 {'

# M6: the stderr line drops the command that pays the debt — the verdict
# arrives with no action attached. D1 requires the word `hydrate`.
mutate "the stderr line drops the paying command" \
    "a_journal_that_owes_a_payload_exits_3_and_names_the_command_that_pays_it" \
    "src/main.rs" \
    '        eprintln!(
            "reeflog: {} row(s) still owe their payload — the journal holds a signature, not what \
             happened; run `reeflog hydrate`",
            c.payload_pending
        );' \
    '        eprintln!(
            "reeflog: {} row(s) still owe their payload — the journal holds a signature, not what happened",
            c.payload_pending
        );'

echo "=== ladder complete: all 6 mutations caught ==="

# The mutation build left a MUTANT binary in target/release. The source is
# restored, but the binary is not — and a later measurement that runs
# target/release/reeflog without rebuilding would measure the mutant, not
# the tree. Rebuild so the tree and its binary agree again.
cargo build --release > /dev/null 2>&1 || {
  echo "rebuild after restore failed" >&2
  exit 1
}

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
  echo "TREE NOT RESTORED: checksums differ" >&2
  exit 1
fi
echo "tree byte-identical after restore"
