#!/usr/bin/env bash
# Mutation ladder for the endpoint binding (2026-11-11).
#
# The rule under test: a webhook target's delivery state and receipts belong
# to ONE endpoint. A repointed id must be REFUSED (exit 1) before a byte
# moves; a legacy row (no endpoint recorded) must be ADOPTED by the url it
# meets first, exactly once.
#
# A pin that cannot be made red certifies nothing, so every mutation below
# has to turn the tree red in the expected direction, and the tree has to
# come back BYTE-IDENTICAL afterwards (never `git checkout` — that is how an
# earlier ladder destroyed the very fix it was supposed to prove).
#
# M1  the endpoint comparison disappears -> a repointed id inherits the old
#     endpoint's cursors again (the measured starvation, D1);
# M2  the refusal is downgraded to silent success (D1);
# M3  `webhook status` stops enforcing the binding -> it prints the OLD
#     endpoint's delivery record next to the NEW url again (D2);
# M4  `webhook dlq replay` stops enforcing the binding -> quarantined bytes
#     follow a repointed url under the same id (D3);
# M5  the refusal names only the configured endpoint: the operator cannot
#     tell which consumer holds the state (D1);
# M6  the refusal drops the repair: the message does not say what to do (D1);
# M7  legacy rows are adopted by EVERY url: the old single-endpoint
#     forgiveness stays open forever (D4);
# M8  legacy rows are NEVER adopted: the first push after an upgrade is
#     refused against the operator's own, unchanged url (D4);
# M9  the adopted row is stamped but the receipts are not: a receipt written
#     for the old endpoint can still silence a delivery (D4).
#
# The search/replace patterns live in FILES, written with quoted heredocs:
# the Rust patterns contain backticks, `{}` forms and single quotes that
# bash would expand or execute inside a double-quoted argument (measured
# 2026-11-11: a double-quoted pattern ran `{}` as a command, and a deleted
# match arm left the tree non-compiling — a deletion mutation must keep the
# match exhaustive to be a behavioural change at all).
set -euo pipefail
cd "$(dirname "$0")/.."

TEST="endpoint_binding_domain"

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

PRISTINE="$(mktemp -d /tmp/ladder-endpoint-binding-pristine.XXXXXX)"
PATTERNS="$(mktemp -d /tmp/ladder-endpoint-binding-patterns.XXXXXX)"
mkdir -p "$PRISTINE/src"
for f in journal.rs webhook.rs main.rs; do
    cp "src/$f" "$PRISTINE/src/$f"
done
restore_all() {
    for f in journal.rs webhook.rs main.rs; do
        cp "$PRISTINE/src/$f" "src/$f"
        touch "src/$f"
    done
}
trap 'restore_all; rm -rf "$PATTERNS"' EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local file="$3"
    local search_file="$4"
    local replace_file="$5"

    assert_filter_matches "$test_fn"

    echo -n "mutation: $name ... "
    python3 -c '
import sys
path, search_file, repl_file = sys.argv[1:4]
with open(path) as f:
    text = f.read()
with open(search_file) as f:
    search = f.read().rstrip(chr(10))
with open(repl_file) as f:
    repl = f.read().rstrip(chr(10))
if search not in text:
    print(f"pattern not found in {path}", file=sys.stderr)
    sys.exit(2)
text = text.replace(search, repl, 1)
with open(path, "w") as f:
    f.write(text)
' "$file" "$search_file" "$replace_file" || {
        echo "" >&2
        echo "LADDER: pattern missing in $file" >&2
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

D1="d1_repainted_url_is_refused_instead_of_starved"
D2="d2_status_refuses_a_repointed_url"
D3="d3_dlq_replay_refuses_a_repointed_url"
D4="d4_a_legacy_journal_is_adopted_by_its_url_then_refused_on_repoint"

# --- M1: the equality disappears -------------------------------------------
cat > "$PATTERNS/m1.s" <<'EOF'
        Some((recorded, _, _)) if recorded == target.url => Ok(false),
EOF
cat > "$PATTERNS/m1.r" <<'EOF'
        Some((recorded, _, _)) if recorded == recorded => Ok(false),
EOF
mutate "stop comparing the endpoint a target is bound to" \
    "$D1" "src/webhook.rs" "$PATTERNS/m1.s" "$PATTERNS/m1.r"

# --- M2: the refusal is downgraded to silent success ----------------------
# (The match stays exhaustive: the Err arm becomes an Ok arm — that is the
# behavioural mutation "warn but continue", not a syntax break.)
cat > "$PATTERNS/m2.s" <<'EOF'
        Some((recorded, _, _)) => Err(format!(
            "target `{}` is bound to endpoint `{recorded}` in the journal, but this config points it at `{}` — delivery state and receipts are bound to the endpoint; give the new endpoint its own target id (measured v0.7.14: a repointed id starved the new consumer silently behind exit 0)",
            target.id, target.url
        )),
EOF
cat > "$PATTERNS/m2.r" <<'EOF'
        Some((recorded, _, _)) => {
            let _ = recorded;
            Ok(false)
        }
EOF
mutate "downgrade the refusal to silent success" \
    "$D1" "src/webhook.rs" "$PATTERNS/m2.s" "$PATTERNS/m2.r"

# --- M3: status stops enforcing the binding -------------------------------
cat > "$PATTERNS/m3.s" <<'EOF'
        // belong to the configured url.
        webhook::bind_endpoint(&j, t).unwrap_or_else(|e| die(e));
EOF
cat > "$PATTERNS/m3.r" <<'EOF'
        // belong to the configured url.
EOF
mutate "status stops enforcing the binding" \
    "$D2" "src/main.rs" "$PATTERNS/m3.s" "$PATTERNS/m3.r"

# --- M4: dlq replay stops enforcing the binding ---------------------------
cat > "$PATTERNS/m4.s" <<'EOF'
        // before the first letter moves.
        webhook::bind_endpoint(&j, t).unwrap_or_else(|e| die(e));
EOF
cat > "$PATTERNS/m4.r" <<'EOF'
        // before the first letter moves.
EOF
mutate "dlq replay stops enforcing the binding" \
    "$D3" "src/main.rs" "$PATTERNS/m4.s" "$PATTERNS/m4.r"

# --- M5: the refusal names only the configured endpoint --------------------
cat > "$PATTERNS/m5.s" <<'EOF'
"target `{}` is bound to endpoint `{recorded}` in the journal, but this config points it at `{}` — delivery state
EOF
cat > "$PATTERNS/m5.r" <<'EOF'
"target `{}` is bound to another endpoint in the journal, but this config points it at `{}` — delivery state
EOF
mutate "name only the configured endpoint in the refusal" \
    "$D1" "src/webhook.rs" "$PATTERNS/m5.s" "$PATTERNS/m5.r"

# --- M6: the refusal drops the repair -------------------------------------
cat > "$PATTERNS/m6.s" <<'EOF'
delivery state and receipts are bound to the endpoint; give the new endpoint its own target id (measured v0.7.14: a repointed id starved the new consumer silently behind exit 0)
EOF
cat > "$PATTERNS/m6.r" <<'EOF'
delivery state and receipts are bound to the endpoint
EOF
mutate "drop the repair from the refusal" \
    "$D1" "src/webhook.rs" "$PATTERNS/m6.s" "$PATTERNS/m6.r"

# --- M7: every row is treated as legacy -----------------------------------
# The is_empty() check disappears: EVERY state row is "adopted" by whatever
# url meets it, and the arm returns Ok — a repointed id inherits the old
# endpoint's cursors again (D4's push_b must still refuse).
cat > "$PATTERNS/m7.s" <<'EOF'
        Some((recorded, _, _)) if recorded.is_empty() => {
EOF
cat > "$PATTERNS/m7.r" <<'EOF'
        Some((recorded, _, _)) if recorded.is_empty() || true => {
EOF
mutate "adopt every url unconditionally" \
    "$D4" "src/webhook.rs" "$PATTERNS/m7.s" "$PATTERNS/m7.r"

# --- M8: legacy rows are NEVER adopted ------------------------------------
cat > "$PATTERNS/m8.s" <<'EOF'
        Some((recorded, _, _)) if recorded.is_empty() => {
            j.webhook_adopt_endpoint(&target.id, &target.url)
        }
EOF
cat > "$PATTERNS/m8.r" <<'EOF'
        Some((recorded, _, _)) if recorded.is_empty() => Ok(false),
EOF
mutate "never adopt legacy rows" \
    "$D4" "src/webhook.rs" "$PATTERNS/m8.s" "$PATTERNS/m8.r"

# --- M9: the adopted row is stamped, its receipts are NOT -----------------
cat > "$PATTERNS/m9.s" <<'EOF'
                    "UPDATE webhook_receipts SET endpoint=?2 WHERE target=?1 AND endpoint=''",
EOF
cat > "$PATTERNS/m9.r" <<'EOF'
                    "UPDATE webhook_receipts SET endpoint='' WHERE target=?1 AND endpoint=''",
EOF
mutate "stamp the adopted row but not its receipts" \
    "$D4" "src/journal.rs" "$PATTERNS/m9.s" "$PATTERNS/m9.r"

echo "=== ladder complete: all 9 mutations caught ==="

if ! cmp -s "$PRISTINE/src/journal.rs" src/journal.rs \
    || ! cmp -s "$PRISTINE/src/webhook.rs" src/webhook.rs \
    || ! cmp -s "$PRISTINE/src/main.rs" src/main.rs; then
    echo "LADDER: the tree was not restored" >&2
    exit 1
fi
echo "tree restored byte-identical"
