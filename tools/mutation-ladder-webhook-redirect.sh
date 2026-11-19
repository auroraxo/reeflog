#!/usr/bin/env bash
# Mutation ladder for the webhook redirect fix (src/webhook.rs).
#
# Falsification-first: each mutation weakens ONE rule the fix rests on,
# runs the exact test that pins it, and asserts RED. If the test passes
# under the mutation, the test is vacuous and the ladder fails.
#
# HARDENED the same way as tools/mutation-ladder-webhook-value.sh after its
# two real instrument failures:
#
#   1. DESTROYER: the restore copies pristine bytes aside BEFORE the first
#      mutation and restores by copy (git is never used to restore a
#      working-tree file; a trap restores on any exit).
#   2. VACUOUS CATCHES: every filter is PROVEN to match at least one real
#      test BEFORE mutating — `cargo test -- --list` names them — or the
#      ladder fails loudly instead of reporting a CAUGHT for a no-op.
#
#   node tools/mutation-ladder-webhook-redirect.sh
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_redirect_domain"

echo "=== webhook redirect mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-pristine.XXXXXX)"
for f in src/webhook.rs src/main.rs; do
    mkdir -p "$PRISTINE/$(dirname "$f")"
    cp "$f" "$PRISTINE/$f"
done
restore_all() {
    for f in src/webhook.rs src/main.rs; do
        if [ -f "$PRISTINE/$f" ]; then
            cp "$PRISTINE/$f" "$f"
            touch "$f"
        fi
    done
}
trap restore_all EXIT

# mutate <name> <test-filter> <file> <search-marker> <replace-marker>
#
# The markers are UNIQUE SUBSTRINGS of the source, and the mutation swaps
# the region between them (inclusive). Markers are short and stable — the
# reason the ladder below uses them rather than whole comment blocks is
# that a comment carrying an apostrophe or parenthesis breaks shell quoting
# and turns the ladder into a syntax error instead of a measurement.
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
# 1. Follow redirects again: drop the .max_redirects(0) bound on the poster.
# ---------------------------------------------------------------------------
mutate "follow redirects again (drop the max_redirects(0) bound)" \
    "d1_a_302_answer_is_not_a_delivery" \
    "src/webhook.rs" \
    "            .max_redirects(0)
            .build();" \
    "            .build();"

# ---------------------------------------------------------------------------
# 2. The worst regression: a 3xx is recorded as a DELIVERY again (the
#    pre-fix behaviour at the report level). No future edit may let a
#    redirect land in the Delivered arm.
# ---------------------------------------------------------------------------
mutate "classify 3xx as Delivered (the pre-fix report lie)" \
    "d1_a_302_answer_is_not_a_delivery" \
    "src/webhook.rs" \
    "    if (200..300).contains(&status) {
        Attempt::Delivered(status)
    } else if status == 408 || status == 425 || status == 429 {" \
    "    if (200..400).contains(&status) {
        Attempt::Delivered(status)
    } else if status == 408 || status == 425 || status == 429 {"

# ---------------------------------------------------------------------------
# 3. A 3xx is a transient outage, not a verdict: the queue still holds but
#    the attempt records status NULL and the report cannot name the status.
# ---------------------------------------------------------------------------
mutate "treat 3xx as a transient outage instead of a verdict" \
    "d1_a_302_answer_is_not_a_delivery" \
    "src/webhook.rs" \
    "    } else if (300..400).contains(&status) {
        // A 3xx is the endpoint's VERDICT about this url: it did not accept
        // the delivery, it sent it somewhere else. It is a refusal of THIS
        // consumer, not an outage and not a delivery — and the redirect is
        // never followed (measured 2026-11-19, PULS AS: with ureq's defaults
        // the hop's own 200 was recorded as the delivery, the cursor moved on
        // and the configured consumer received nothing). The reason names the
        // mechanism and the repair: repoint the target's url EXPLICITLY.
        Attempt::Refused(status)
    } else if (400..500).contains(&status) {" \
    "    } else if (300..400).contains(&status) {
        Attempt::Transient(format!(\"http {status}\"))
    } else if (400..500).contains(&status) {"

# ---------------------------------------------------------------------------
# 4. Drop the redirect wording from the refusal reason: a 3xx keeps its
#    verdict but the operator is never told the mechanism or the repair.
# ---------------------------------------------------------------------------
mutate "drop the redirect wording from the refusal reason" \
    "d3_the_report_names_the_redirect_as_the_block" \
    "src/webhook.rs" \
    "    if (300..400).contains(&status) {
        format!(
            \"refused with http {status} redirect — the endpoint redirected the delivery; delivery follows the url AS CONFIGURED, never the redirect: repoint this target's url explicitly if the redirect destination is the real consumer\"
        )
    } else {
        format!(\"refused with http {status}\")
    }" \
    "    format!(\"refused with http {status}\")"

# ---------------------------------------------------------------------------
# 5. Let a redirecting endpoint exit 0 again: drop the block gate.
# ---------------------------------------------------------------------------
mutate "let a redirecting endpoint exit 0 (drop the block gate)" \
    "d1_a_302_answer_is_not_a_delivery" \
    "src/webhook.rs" \
    "    pub fn blocked(&self) -> bool {
        self.blocked_seq.is_some() || self.blocked_reorg_id.is_some() || self.refused
    }" \
    "    pub fn blocked(&self) -> bool {
        false
    }"

echo "=== webhook redirect mutation ladder: all mutations CAUGHT ==="
