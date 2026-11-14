#!/usr/bin/env bash
# Mutation ladder for the GAP-LEDGER fix (PULS AJ, 2026-11-14).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  `verify` opens a fresh gap row per scan again (the defect itself)
#   M2  `repair` closes only the oldest open hole again (the defect itself)
#   M3  the repair sweep closes the whole episode -> only the oldest row
#   M4  a complete clean verify stops healing the stale open rows
#   M5  the repair sweep ignores the window it actually scanned
#
# Usage: tools/mutation-ladder-gap-ledger.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."
TEST="gap_ledger"

before=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)

SNAPSHOT=""
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

# M1: `verify` opens a fresh gap row per scan (the published defect).
mutate "verify opens a fresh gap row per scan again" \
    "two_verifies_over_one_hole_leave_one_gap_row" \
    "src/main.rs" \
    "        match j.gap_find_open(\"hole\") {
            Ok(Some(id)) => {
                let _ = j.gap_update_detail(id, &format!(\"; {detail}\"));
            }
            _ => {" \
    "        match j.gap_find_open(\"hole\") {
            Ok(Some(id)) if false => {
                let _ = j.gap_update_detail(id, &format!(\"; {detail}\"));
            }
            _ => {"

# M2: `repair` closes only the oldest open hole (the published defect).
mutate "repair closes only the oldest open hole again" \
    "a_repair_closes_every_open_row_of_the_episode_it_filled" \
    "src/main.rs" \
                    "                if covers_all {
                    let _ = j.gap_close_all_open(
                        \"hole\",
                        j.max_slot().unwrap_or(None),
                        r.filled,
                        &format!(\"; repaired {} row(s), journal re-check clean\", r.filled),
                    );
                } else {" \
                    "                if covers_all {
                    let _ = j.gap_close(
                        g,
                        j.max_slot().unwrap_or(None),
                        r.filled,
                        &format!(\"; repaired {} row(s), journal re-check clean\", r.filled),
                    );
                } else {"

# M3: the repair sweep closes only the first row of the episode.
mutate "the repair sweep closes only the first open row" \
    "a_repair_closes_every_open_row_of_the_episode_it_filled" \
    "src/journal.rs" \
    "        for (i, id) in ids.iter().enumerate() {
            let count = if i == 0 { backfill_count } else { 0 };
            self.gap_close(*id, until_slot, count, detail_suffix)?;
        }" \
    "        for (i, id) in ids.iter().enumerate() {
            if i > 0 { break; }
            let count = if i == 0 { backfill_count } else { 0 };
            self.gap_close(*id, until_slot, count, detail_suffix)?;
        }"

# M4: a complete clean verify stops healing the stale open rows.
mutate "a complete clean verify stops healing the stale rows" \
    "a_complete_clean_verify_heals_the_stale_rows_a_pre_fix_verify_left" \
    "src/main.rs" \
    "    else if r.complete() && scan_covers_whole_window(&j, since_slot) {" \
    "    else if false && r.complete() && scan_covers_whole_window(&j, since_slot) {"

# M5: the repair sweep ignores the window it actually scanned.
mutate "the repair sweep ignores the window it scanned" \
    "a_narrowed_repair_may_not_close_an_episode_it_did_not_prove" \
    "src/main.rs" \
    "                let covers_all = scan_covers_whole_window(j, since_slot);" \
    "                let covers_all = true;"

echo "=== ladder complete: all 5 mutations caught ==="

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
    echo "LADDER BUG: the tree was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
