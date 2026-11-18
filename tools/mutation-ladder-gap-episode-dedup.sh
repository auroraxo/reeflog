#!/usr/bin/env bash
# Mutation ladder for the gap-episode dedup fix (PULS AQ).
#
# A test that cannot go red proves nothing. Each mutation breaks ONE rule
# the fix introduced; the ladder must catch EVERY one of them, and the tree
# must come back byte-identical afterwards.
#
# The mutations:
#   M1  the episode guard is dropped for connect_fail: every refused
#       attempt opens its own row again (the defect itself)      [journal]
#   M2  the same for subscribe_fail                               [journal]
#   M3  the heal sweeps only `connect_fail`: an open `subscribe_fail` row
#       from the same episode survives the heal                   [journal]
#   M4  the heal forgets the episode: rows are deleted instead of
#       resolved (gaps must stay HISTORY)                         [journal]
#   M5  the sweep is unconditional on success: a TRUNCATED backfill also
#       closes the episode (the mirror: alarm becomes silence)    [daemon]
#   M6  the connect site stops reusing the open row (the daemon routes
#       around the journal rule)                                  [daemon]
#   M7  the daemon's heal sweeps only `connect_fail` — a planted
#       subscribe_fail row from a killed run survives the daemon   [daemon]
#
# Usage: tools/mutation-ladder-gap-episode-dedup.sh
# Exit:  0 all mutations caught, 1 a mutation survived, 2 setup failed.
set -uo pipefail

cd "$(dirname "$0")/.."

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
    cp -a "$SNAPSHOT/tests/." "$SNAPSHOT" >/dev/null 2>&1 || true
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

# mutate UNIT <module> <test-substring> <file> <search> <replace>
# mutate INT  <integration-test-substring> <file> <search> <replace>
mutate() {
  local name="$1" scope="$2" test_fn="$3" path="$4" search="$5" replace="$6"
  echo -n "mutation: $name ... "
  python3 -c "
import sys
path, search, replace = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path).read()
if text.count(search) < 1:
    print('VACUOUS: pattern not found'); sys.exit(3)
open(path, 'w').write(text.replace(search, replace, 1))
" "$path" "$search" "$replace" || { echo "VACUOUS — check the pattern"; restore_all; exit 2; }
  if [ "$scope" = "UNIT" ]; then
    cargo test --bin reeflog "$test_fn" > /tmp/ladder-run.log 2>&1
  else
    cargo test --release --test gap_episode_dedup "$test_fn" > /tmp/ladder-run.log 2>&1
  fi
  if [ $? -eq 0 ]; then
    echo "SURVIVED — a mutant walked past the gate"
    restore_all
    exit 1
  fi
  echo "CAUGHT"
  restore_all
}

# M1: the guard dedups connect_fail again (restore the defect for that kind).
mutate "M1 duplicate connect_fail rows return" UNIT "gap_episode_is_one_row_per_kind" \
  src/journal.rs \
  '        match self.gap_find_open(kind)? {
            Some(id) => {
                self.gap_append_detail(id, detail)?;
                Ok(id)
            }
            None => self.gap_open(kind, since_seq, since_slot, since_sig, detail),
        }' \
  '        self.gap_open(kind, since_seq, since_slot, since_sig, detail)'

# M2: the same for subscribe_fail — the subscribe row stops deduplicating.
mutate "M2 duplicate subscribe_fail rows return" UNIT "gap_episode_kinds_do_not_share_a_row" \
  src/journal.rs \
  '        match self.gap_find_open(kind)? {
            Some(id) => {
                self.gap_append_detail(id, detail)?;
                Ok(id)
            }
            None => self.gap_open(kind, since_seq, since_slot, since_sig, detail),
        }' \
  '        match self.gap_find_open(kind)? {
            Some(id) if kind == "connect_fail" => {
                self.gap_append_detail(id, detail)?;
                Ok(id)
            }
            None => self.gap_open(kind, since_seq, since_slot, since_sig, detail),
            Some(id) => Ok(id),
        }'

# M3: the sweep drops the sibling kind — a subscribe_fail row from the same
# episode survives the heal.
mutate "M3 sweep misses subscribe_fail" UNIT "gap_episode_resolve_closes_the_whole_episode_as_history" \
  src/journal.rs \
  '        let mut closed = self.gap_close_all_open("connect_fail", until_slot, backfill_count, detail_suffix)?;
        closed += self.gap_close_all_open("subscribe_fail", until_slot, 0, detail_suffix)?;
        Ok(closed)' \
  '        self.gap_close_all_open("connect_fail", until_slot, backfill_count, detail_suffix)'

# M4: the heal forgets the episode instead of resolving it.
mutate "M4 heal deletes the episode rows" UNIT "gap_episode_resolve_closes_the_whole_episode_as_history" \
  src/journal.rs \
  '        let mut closed = self.gap_close_all_open("connect_fail", until_slot, backfill_count, detail_suffix)?;
        closed += self.gap_close_all_open("subscribe_fail", until_slot, 0, detail_suffix)?;
        Ok(closed)' \
  '        let mut closed = self.gap_close_all_open("connect_fail", until_slot, backfill_count, detail_suffix)?;
        Ok(closed)'

# M5: the sweep ignores truncation — a partial heal reads as a full one.
mutate "M5 sweep fires on a truncated backfill too" INT "a_truncated_heal_keeps_the_episode_open" \
  src/main.rs \
  '            if r.truncated || r.error.is_some() {
                if let Some(g) = g {
                    j.gap_update_detail(g, &format!("{note}; GAP STILL OPEN")).ok();
                    open_gap = Some(g);
                }
            } else {' \
  '            if false {
                if let Some(g) = g {
                    j.gap_update_detail(g, &format!("{note}; GAP STILL OPEN")).ok();
                    open_gap = Some(g);
                }
            } else {'

# M6: the daemon routes around the journal rule — the refused connect opens
# its own row again.
mutate "M6 daemon opens duplicate connect rows again" INT "one_episode_is_one_row_and_the_heal_closes_it" \
  src/main.rs \
  '                let g = j
                    .gap_episode_note(
                        "connect_fail",
                        j.max_seq().unwrap_or(0),
                        j.max_slot().unwrap_or(None),
                        j.last_signature().unwrap_or(None).as_deref(),
                        &format!("ws connect failed: {e}"),
                    )
                    .unwrap_or_else(|e| die(e));
                open_gap = Some(g);' \
  '                let g = j
                    .gap_open(
                        "connect_fail",
                        j.max_seq().unwrap_or(0),
                        j.max_slot().unwrap_or(None),
                        j.last_signature().unwrap_or(None).as_deref(),
                        &format!("ws connect failed: {e}"),
                    )
                    .unwrap_or_else(|e| die(e));
                open_gap = Some(g);'

# M7: the DAEMON's heal routes around the sibling kind — the planted
# subscribe_fail row of the episode survives (unit tests call the helper
# directly and would not see a daemon bypassing it).
mutate "M7 daemon heal misses subscribe_fail" INT "a_killed_runs_subscribe_fail_row_heals_with_the_episode" \
  src/main.rs \
  '                let _ = j
                    .gap_episode_resolve(j.max_slot().unwrap_or(None), r.inserted, &note)
                    .unwrap_or_else(|e| die(e));' \
  '                let _ = j
                    .gap_close_all_open("connect_fail", j.max_slot().unwrap_or(None), r.inserted, &note)
                    .unwrap_or_else(|e| die(e));'

after=$(find src tests -type f -name '*.rs' -exec sha256sum {} + | sort | sha256sum)
if [ "$before" != "$after" ]; then
  echo "TREE NOT BYTE-IDENTICAL after the ladder — refusing to report success"
  exit 2
fi
echo "ladder complete: 7/7 caught, tree byte-identical"
