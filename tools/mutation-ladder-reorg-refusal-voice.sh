#!/usr/bin/env bash
# Mutation ladder for the retraction refusal voice fix (src/webhook.rs).
#
# Falsification-first: each mutation weakens ONE rule the fix rests on,
# runs the exact test that pins it, and asserts RED. If the test passes
# under the mutation, the test is vacuous and the ladder fails.
#
# Hardened like tools/mutation-ladder-scheduler-zero-cadence.sh:
#
#   1. DESTROYER: pristine bytes are copied aside BEFORE the first mutation
#      and restored by copy (git never restores a working-tree file; a trap
#      restores on any exit).
#   2. VACUOUS CATCHES: every test filter is PROVEN to match at least one
#      real test BEFORE mutating (`cargo test -- --list` names them), or the
#      ladder fails loudly instead of reporting a CAUGHT for a no-op.
#   3. COMPILE: a mutation that does not compile is an error, not a catch.
#   4. The RESTORE comparison is against the PRISTINE SNAPSHOT, never
#      `git HEAD` — a dirty tree or a mid-run commit makes HEAD the wrong
#      reference.
#
#   node tools/mutation-ladder-reorg-refusal-voice.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_reorg_refusal_voice"

echo "=== retraction refusal voice mutation ladder ==="

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
    if ! printf '%s\n' "$LIST" | grep -q "^${filter}$"; then
        echo "" >&2
        echo "LADDER BUG: filter \`$filter\` matches no test in tests/$TEST.rs — a CAUGHT verdict would be vacuous" >&2
        printf '%s\n' "$LIST" | sed 's/^/    available: /' >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-pristine-reorg-voice.XXXXXX)"
for f in src/webhook.rs; do
    mkdir -p "$PRISTINE/$(dirname "$f")"
    cp "$f" "$PRISTINE/$f"
done
restore_all() {
    for f in src/webhook.rs; do
        if [ -f "$PRISTINE/$f" ]; then
            cp "$PRISTINE/$f" "$f"
            touch "$f"
        fi
    done
}
trap restore_all EXIT

# mutate <name> <test-filter> <file> <search-marker> <replace-marker>
mutate() {
    local name="$1"
    local test_fn="$2"
    local file="$3"
    local search="$4"
    local replace="$5"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    python3 - "$file" "$search" "$replace" <<'PY' >/dev/null 2>/tmp/ladder-err.txt
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
PY
    local py_rc=$?
    if [ "$py_rc" -ne 0 ]; then
        echo "" >&2
        echo "LADDER: mutation did not apply in $file: $search" >&2
        cat /tmp/ladder-err.txt >&2
        exit 1
    fi

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

# ---------------------------------------------------------------------------
# 1. The measured defect, restored: the retraction queue borrows the flat
#    4xx wording again, so a 3xx on the RETRACTION channel names neither
#    the mechanism nor the repair.
# ---------------------------------------------------------------------------
mutate "the retraction queue borrows the flat 4xx wording again" \
    "d1_the_retraction_attempt_names_the_redirect_and_the_repair" \
    "src/webhook.rs" \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(refusal_reason(*s)))," \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(format!(\"refused with http {s}\"))),"

# ---------------------------------------------------------------------------
# 2. The report surface: the row the operator polls names the redirect.
# ---------------------------------------------------------------------------
mutate "the report's retraction reason stops naming the redirect" \
    "d2_the_report_reorg_error_names_the_redirect" \
    "src/webhook.rs" \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(refusal_reason(*s)))," \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(format!(\"refused with http {s}\"))),"

# ---------------------------------------------------------------------------
# 3. The operator's blocked line.
# ---------------------------------------------------------------------------
mutate "the operator's retraction block line stops naming the redirect" \
    "d3_the_operator_blocked_line_names_the_redirect" \
    "src/webhook.rs" \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(refusal_reason(*s)))," \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(format!(\"refused with http {s}\"))),"

# ---------------------------------------------------------------------------
# 4. The polled `webhook status` surface.
# ---------------------------------------------------------------------------
mutate "the polled retraction reason stops naming the redirect" \
    "d4_the_polled_status_names_the_redirect_on_the_retraction_queue" \
    "src/webhook.rs" \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(refusal_reason(*s)))," \
    "                    Attempt::Refused(s) => (Some(*s as i64), Some(format!(\"refused with http {s}\"))),"

# ---------------------------------------------------------------------------
# 5. ONE mint: the mint itself stops naming the redirect (the event channel
#    borrows the flat wording too) — the two queues must read the same.
# ---------------------------------------------------------------------------
mutate "the refusal mint stops naming the redirect" \
    "d1_the_retraction_attempt_names_the_redirect_and_the_repair" \
    "src/webhook.rs" \
    "pub fn refusal_reason(status: u16) -> String {
    if (300..400).contains(&status) {" \
    "pub fn refusal_reason(status: u16) -> String {
    if false {"

# ---------------------------------------------------------------------------
# 6. The mint names the mechanism but NOT the repair — a reason that names
#    the redirect and leaves the operator without the config change.
# ---------------------------------------------------------------------------
mutate "the refusal mint names the redirect but not the repair" \
    "d1_the_retraction_attempt_names_the_redirect_and_the_repair" \
    "src/webhook.rs" \
    "            \"refused with http {status} redirect — the endpoint redirected the delivery; delivery follows the url AS CONFIGURED, never the redirect: repoint this target's url explicitly if the redirect destination is the real consumer\"" \
    "            \"refused with http {status} redirect\""

# ---------------------------------------------------------------------------
# 7. The flat wording is restored ONLY for the 4xx class — a 4xx must keep
#    reading exactly as it always read (the guard).
# ---------------------------------------------------------------------------
mutate "the mint grows redirect wording for the 4xx class too" \
    "g2_a_404_refusal_keeps_the_flat_wording" \
    "src/webhook.rs" \
    "pub fn refusal_reason(status: u16) -> String {
    if (300..400).contains(&status) {" \
    "pub fn refusal_reason(status: u16) -> String {
    if true {"

echo "=== ladder done ==="
