#!/usr/bin/env bash
# Mutation ladder for the cursor-provenance fix (src/cursorfile.rs +
# src/main.rs: read_cursor / honour_cursor / write_cursor).
#
# Falsification-first: each mutation weakens ONE rule, runs the exact test
# that pins it, and asserts RED. If the test passes under the mutation, the
# test is vacuous and the ladder fails.
#
# HARDENED the same way as tools/mutation-ladder-dlq-replay-page.sh:
#
#   1. DESTROYER: the restore is a COPY, never `git checkout` — git would
#      reset to HEAD and WIPE the uncommitted fix.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="cursor_provenance"

echo "=== cursor provenance mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-cursor-prov-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/main.rs "$PRISTINE/src/main.rs"
cp src/cursorfile.rs "$PRISTINE/src/cursorfile.rs"
restore_all() {
    for f in main.rs cursorfile.rs; do
        if [ -f "$PRISTINE/src/$f" ]; then
            cp "$PRISTINE/src/$f" "src/$f"
            touch "src/$f"
        fi
    done
}
trap restore_all EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local file="$3"
    local search="$4"
    local replace="$5"

    assert_filter_matches "$test_fn"

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
' "$file" "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing in $file: $search" >&2
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

# 1. The channel check disappears: an event cursor may be read by the
#    retraction channel again (the D1 starvation face).
mutate "stop checking the channel a cursor belongs to" \
    "d1_an_event_cursor_handed_to_the_retraction_channel_is_refused" \
    "src/cursorfile.rs" \
    "    if wrote_channel != want || wrote_journal != journal_id {" \
    "    if false {"

# 2. The journal check disappears: a cursor copied to another journal is
#    honoured again (the D3 silent-gap face).
mutate "stop checking the journal a cursor belongs to" \
    "d3_a_cursor_copied_to_a_different_journal_is_refused" \
    "src/cursorfile.rs" \
    "    if wrote_channel != want || wrote_journal != journal_id {" \
    "    if false {"

# 3. Only the channel is checked, not the journal — the half-fix that looks
#    right and still loses a copied cursor.
mutate "check the channel but not the journal" \
    "d3_a_cursor_copied_to_a_different_journal_is_refused" \
    "src/cursorfile.rs" \
    "    if wrote_channel != want || wrote_journal != journal_id {" \
    "    if wrote_channel != want {"

# 4. The crossing check runs, but its verdict is ignored: the caller keeps
#    the position anyway (the refusal that never refuses).
mutate "ignore the crossing verdict" \
    "d1_an_event_cursor_handed_to_the_retraction_channel_is_refused" \
    "src/main.rs" \
    "    match cursorfile::honour(c, channel, &id, &path.display().to_string()) {
        Ok(_) => c.value,
        Err(why) => die(why),
    }" \
    "    let _ = cursorfile::honour(c, channel, &id, &path.display().to_string());
    c.value"

# 5. A crossing refuses but REWRITES the cursor to 0 — the refusal that
#    destroys the evidence and silently restarts the consumer.
mutate "a crossing refusal resets the cursor to 0" \
    "d4_a_crossing_refusal_leaves_the_cursor_file_untouched" \
    "src/main.rs" \
    "        Err(why) => die(why),
    }
}

fn write_cursor" \
    "        Err(why) => {
            let _ = std::fs::write(path, \"0\n\");
            die(why)
        }
    }
}

fn write_cursor"

# 6. The bare hand-written number becomes a refusal — the over-strict fix
#    that breaks the operator's deliberate rewind.
mutate "refuse a bare hand-written number" \
    "g2_a_bare_hand_written_number_stays_legal" \
    "src/main.rs" \
    "    if !c.structured {
        return c.value;
    }" \
    "    if !c.structured {
        die(format!(
            \"cursor {} holds a bare number — refusing to resume\",
            path.display()
        ));
    }"

# 7. The written form loses its provenance: reeflog writes a bare number
#    again, so the whole rule stops existing at the source.
mutate "write cursors without provenance" \
    "d1_an_event_cursor_handed_to_the_retraction_channel_is_refused" \
    "src/cursorfile.rs" \
    "    format!(
        \"{{\\\"v\\\":1,\\\"channel\\\":\\\"{}\\\",\\\"journal\\\":\\\"{}\\\",\\\"value\\\":{}}}\\n\",
        channel.as_str(),
        journal_id,
        value
    )" \
    "    format!(\"{}\n\", value)"

echo "=== ladder complete: all 7 mutations caught ==="

if ! cmp -s "$PRISTINE/src/main.rs" src/main.rs || ! cmp -s "$PRISTINE/src/cursorfile.rs" src/cursorfile.rs; then
    echo "LADDER: the tree was not restored" >&2
    exit 1
fi
