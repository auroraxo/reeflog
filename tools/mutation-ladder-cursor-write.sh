#!/usr/bin/env bash
# Mutation ladder for the cursor-write durability fix (src/main.rs:
# write_cursor — stage, fsync, rename).
#
# Falsification-first: each mutation weakens ONE rule, runs the exact test
# that pins it, and asserts RED. If the test passes under the mutation, the
# test is vacuous and the ladder fails.
#
# Hardened the same way as tools/mutation-ladder-cursor-provenance.sh:
#
#   1. DESTROYER: the restore is a COPY, never `git checkout` — git would
#      reset to HEAD and wipe the fix.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="cursor_write_durability"

echo "=== cursor write durability mutation ladder ==="

echo -n "unmutated tree ... "
if cargo test --release --test "$TEST" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: unmutated tree does not pass" >&2
    exit 1
fi

LIST="$(cargo test --release --test "$TEST" -- --list 2>/dev/null | awk -F': ' '/: test$/{print $1}')"

assert_filter_matches() {
    local filter="$1"
    if ! printf '%s\n' "$LIST" | grep -q "^${filter}"; then
        echo "" >&2
        echo "LADDER BUG: filter \`$filter\` matches no test in tests/$TEST.rs — a CAUGHT verdict would be vacuous" >&2
        printf '%s\n' "$LIST" | sed 's/^/    available: /' >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-cursor-write-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/main.rs "$PRISTINE/src/main.rs"
restore_all() {
    if [ -f "$PRISTINE/src/main.rs" ]; then
        cp "$PRISTINE/src/main.rs" "src/main.rs"
        touch "src/main.rs"
    fi
}
trap restore_all EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local pattern="$3"
    assert_filter_matches "$test_fn"
    cp "$PRISTINE/src/main.rs" "src/main.rs"
    # M1: the OLD truncating write runs again, before the swap — the exact
    # defect (O_TRUNC then write) restored in its original position.
    # M2: the staged file is never renamed over the cursor (the cursor
    #     never advances — d4's retry must show it move).
    # M3: a failed stage no longer removes its temp file.
    # M4: the refusal no longer carries the redelivery promise.
    # M5: the refusal no longer names the cursor file.
    # M6: the swap writes value 0 instead of the delivered position.
    # M7: HALF-FIX — only the events channel goes through the durable swap;
    #     the reorg channel is back to the truncating write (catches d2).
    python3 - "$pattern" <<'PY'
import sys
pattern = sys.argv[1]
src = open('src/main.rs').read()
orig = src
if pattern == 'STAGE_DIRECT_WRITE':
    src = src.replace(
        '''    match std::fs::File::create(&tmp) {''',
        '''    let _ = std::fs::write(path, content.as_bytes());
    match std::fs::File::create(&tmp) {''')
elif pattern == 'NO_RENAME':
    src = src.replace(
        '''            if let Err(e) = std::fs::rename(&tmp, path) {
                stage(e.to_string());
            }''',
        '''            // the temp file is left behind and never swapped in''')
elif pattern == 'NO_TEMP_CLEANUP':
    src = src.replace(
        '''    let stage = |e: String| -> ! {
        let _ = std::fs::remove_file(&tmp);''',
        '''    let stage = |e: String| -> ! {
        // the temp file is left behind''')
elif pattern == 'NO_PROMISE':
    src = src.replace('the cursor file is UNCHANGED and still holds its previous position; the rows printed above were delivered and will REDLIVER on the next run (delivery is at-least-once), so simply run again', 'the write failed')
elif pattern == 'NO_FILE_NAME':
    src = src.replace('cursor write {display}: {e}', 'cursor write: {e}')
elif pattern == 'RENDERS_ZERO':
    src = src.replace('let content = cursorfile::render(channel, &id, seq);',
                      'let content = cursorfile::render(channel, &id, 0);')
elif pattern == 'REORG_HALF_FIX':
    src = src.replace(
        'write_cursor(&cursor_path, &j, cursorfile::Channel::Reorgs, last.id);',
        '''{ let id = j.journal_id().unwrap_or_else(|e| die(format!("journal id: {e}")));
            std::fs::write(&cursor_path, cursorfile::render(cursorfile::Channel::Reorgs, &id, last.id))
                .unwrap_or_else(|e| die(format!("cursor write: {e}"))); }''')
else:
    raise SystemExit(f'unknown mutation {pattern}')
if src == orig:
    raise SystemExit(f'LADDER BUG: mutation {pattern} matched no anchor — its CAUGHT verdict would be vacuous')
open('src/main.rs','w').write(src)
PY
    echo -n "mutation $name ($test_fn) ... "
    if cargo test --release --test "$TEST" "$test_fn" > /tmp/ladder-cursor-write.log 2>&1; then
        echo "NOT CAUGHT" >&2
        echo "LADDER FAILED: the test stayed GREEN under mutation $name — it is vacuous" >&2
        tail -30 /tmp/ladder-cursor-write.log >&2
        exit 1
    fi
    if grep -q "error\[" /tmp/ladder-cursor-write.log; then
        echo "COMPILE ERROR (not a behavioural catch)" >&2
        tail -20 /tmp/ladder-cursor-write.log >&2
        exit 1
    fi
    if ! grep -qE "test result: FAILED" /tmp/ladder-cursor-write.log; then
        echo "NO FAILED TEST (vacuous)" >&2
        tail -20 /tmp/ladder-cursor-write.log >&2
        exit 1
    fi
    echo "CAUGHT"
}

mutate "M1 the cursor write goes back to the truncating form" \
    d1_tail_failed_write_keeps_the_old_cursor STAGE_DIRECT_WRITE
mutate "M2 the staged file is never renamed over the cursor" \
    d4_retry_after_failed_write_redelivers_and_advances NO_RENAME
mutate "M3 a failed write leaves its temp file behind" \
    d3_first_batch_failed_write_leaves_no_file_at_the_cursor_path NO_TEMP_CLEANUP
mutate "M4 the refusal loses the redelivery promise" \
    d1_tail_failed_write_keeps_the_old_cursor NO_PROMISE
mutate "M5 the refusal loses the cursor file's name" \
    d1_tail_failed_write_keeps_the_old_cursor NO_FILE_NAME
mutate "M6 the swap writes value 0 instead of the delivered position" \
    g1_successful_write_still_lands_the_structured_cursor RENDERS_ZERO
mutate "M7 half-fix: the reorg channel is back to the truncating write" \
    d2_reorg_tail_failed_write_keeps_the_old_cursor REORG_HALF_FIX

# restore and prove the tree is byte-identical and green again
cp "$PRISTINE/src/main.rs" src/main.rs
touch src/main.rs
if ! cmp -s "$PRISTINE/src/main.rs" src/main.rs; then
    echo "LADDER FAILED: tree not restored byte-identically" >&2
    exit 1
fi
echo -n "restored tree ... "
if cargo test --release --test "$TEST" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: restored tree does not pass" >&2
    exit 1
fi
echo "7/7 mutations caught; tree restored byte-identically"
