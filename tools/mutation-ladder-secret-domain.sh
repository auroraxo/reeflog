#!/usr/bin/env bash
# Mutation ladder for the secret domain (2026-11-11).
#
# The rule under test: ONE SECRET IS ONE CONSUMER. Two targets that share a
# secret while pointing at DIFFERENT endpoints must be REFUSED (exit 1)
# before a byte moves. Measured on the published v0.7.15 asset: with one
# shared secret, A's accepted bytes replay at B under a forged event-id
# header and pass B's own HMAC rule (4/4 faces,
# `tools/measure-secret-domain.mjs`).
#
# A pin that cannot be made red certifies nothing, so every mutation below
# has to turn the tree red in the expected direction, and the tree has to
# come back BYTE-IDENTICAL afterwards (never `git checkout` — that is how an
# earlier ladder destroyed the very fix it was supposed to prove).
#
# M1  the whole check disappears -> a shared secret on two endpoints is
#     accepted again (D1: the measured forgery returns);
# M2  only the empty secret is refused, the uniqueness rule is dropped -> the
#     same, one layer deeper;
# M3  the check is called on the wrong pair (the row itself instead of the
#     previous row) -> the second target compares with itself, the shared
#     secret passes (D1);
# M4  the refusal is downgraded to a silent success -> the config loads and
#     the delivery goes out (D1);
# M5  the refusal drops the target ids -> the operator cannot tell which pair
#     to fix (the pair-naming test goes red);
# M6  the refusal drops the repair line -> the message says what is wrong
#     but not what to do (D3);
# M7  the refusal echoes the secret (call-site debug wrap) -> a key lands in
#     logs, terminals and CI output (the no-echo assertion goes red);
# M8  the "same url" exemption disappears -> one consumer listed twice is
#     refused (the over-refusal guard G3 goes red);
# M9  the secret comparison disappears: EVERY pair is judged -> two targets
#     with distinct secrets are refused (guard G2 goes red — over-refusal).
#
# The search/replace patterns live in FILES, written with quoted heredocs:
# the Rust patterns contain backticks, `{}` forms and quotes that bash would
# expand or execute inside a double-quoted argument (measured 2026-11-11: a
# double-quoted pattern ran `{}` as a command).
set -euo pipefail
cd "$(dirname "$0")/.."

TEST="secret_domain"

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

PRISTINE="$(mktemp -d /tmp/ladder-secret-domain-pristine.XXXXXX)"
PATTERNS="$(mktemp -d /tmp/ladder-secret-domain-patterns.XXXXXX)"
mkdir -p "$PRISTINE/src"
for f in flags.rs webhook.rs; do
    cp "src/$f" "$PRISTINE/src/$f"
done
restore_all() {
    for f in flags.rs webhook.rs; do
        cp "$PRISTINE/src/$f" "src/$f"
        touch "src/$f"
    done
}
trap 'restore_all; rm -rf "$PATTERNS"' EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local pattern_file="$3"
    local from_file="$4"
    local to_file="$5"
    local expect="$6" # RED | GREEN

    assert_filter_matches "$test_fn"

    # The patterns live in FILES (quoted heredocs): the Rust text contains
    # backticks, `{}` forms and quotes that bash would expand or execute
    # inside a double-quoted argument (measured 2026-11-11: a double-quoted
    # pattern ran `{}` as a command). Read their CONTENTS here.
    local from to
    from="$(cat "$from_file")"
    to="$(cat "$to_file")"

    python3 - "$pattern_file" "$from" "$to" <<'PY'
import sys
path, frm, to = sys.argv[1], sys.argv[2], sys.argv[3]
p = 'src/' + path
s = open(p, encoding='utf-8').read()
if s.count(frm) != 1:
    print('LADDER BUG: anchor occurs %d times (must be exactly 1)' % s.count(frm), file=sys.stderr)
    sys.exit(1)
open(p, 'w', encoding='utf-8').write(s.replace(frm, to))
PY

    local out status
    set +e
    out="$(cargo test --release --test "$TEST" -- "$test_fn" 2>&1)"
    status=$?
    set -e

    # A compile error is NOT a caught mutation: it says nothing about the
    # behaviour. (measured 2026-11-11 on the endpoint-binding ladder)
    if printf '%s\n' "$out" | grep -q "error\[E"; then
        echo "" >&2
        echo "LADDER BUG ($name): the mutation does not compile — a compile error is not a caught mutation" >&2
        printf '%s\n' "$out" | grep -E "error\[E" | head -5 >&2
        restore_all
        exit 1
    fi

    if [ "$expect" = "RED" ]; then
        # the named test must FAIL
        if printf '%s\n' "$out" | grep -qE "^test $test_fn \.\.\. FAILED"; then
            echo "CAUGHT  $name ($test_fn is RED)"
        else
            echo "" >&2
            echo "ESCAPED $name — the mutation left \`$test_fn\` green: the pin certifies nothing" >&2
            printf '%s\n' "$out" | tail -20 >&2
            restore_all
            exit 1
        fi
    else
        # the named test must PASS (a guard: an over-refusal must stay visible)
        if printf '%s\n' "$out" | grep -qE "^test $test_fn \.\.\. ok"; then
            echo "CAUGHT  $name ($test_fn stays green)"
        else
            echo "" >&2
            echo "ESCAPED $name — the mutation broke the guard \`$test_fn\`" >&2
            printf '%s\n' "$out" | tail -20 >&2
            restore_all
            exit 1
        fi
    fi
    restore_all
}

# --- the ladder -------------------------------------------------------------

cat > "$PATTERNS/m1_call.txt" <<'EOF'
                reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
EOF
cat > "$PATTERNS/m1_no.txt" <<'EOF'
                let _ = (&prev.id, &prev.url, id, url);
EOF

cat > "$PATTERNS/m2_secret_cmp.txt" <<'EOF'
            if prev.secret == secret {
EOF
cat > "$PATTERNS/m2_never.txt" <<'EOF'
            if false && prev.secret == secret {
EOF

cat > "$PATTERNS/m3_self.txt" <<'EOF'
                reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
EOF
cat > "$PATTERNS/m3_selfpair.txt" <<'EOF'
                reeflog::flags::check_secret_uniqueness(id, url, id, url)?;
EOF

cat > "$PATTERNS/m4_call.txt" <<'EOF'
                reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
EOF
cat > "$PATTERNS/m4_ignored.txt" <<'EOF'
                let _ = reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url);
EOF

cat > "$PATTERNS/m5_ids.txt" <<'EOF'
        "targets {id_a:?} and {id_b:?} share one secret but point at different endpoints \
EOF
cat > "$PATTERNS/m5_noids.txt" <<'EOF'
        "two targets share one secret but point at different endpoints \
EOF
cat > "$PATTERNS/m6_fix.txt" <<'EOF'
         (measured v0.7.15: A's accepted bytes replayed at B under a forged event-id \
         header pass B's own rule). fix: give each endpoint its own secret, or use the \
         same url on both rows"
EOF
cat > "$PATTERNS/m6_nofix.txt" <<'EOF'
         (measured v0.7.15: A's accepted bytes replayed at B under a forged event-id \
         header pass B's own rule)"
EOF

cat > "$PATTERNS/m7_call.txt" <<'EOF'
                reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
EOF
cat > "$PATTERNS/m7_echo.txt" <<'EOF'
                if let Err(e) = reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url) {
                    return Err(format!("{e} — the shared secret is {secret}"));
                }
EOF

cat > "$PATTERNS/m8_same_url.txt" <<'EOF'
    if url_a == url_b {
        return Ok(());
    }
EOF
cat > "$PATTERNS/m8_never.txt" <<'EOF'
    if false {
        return Ok(());
    }
EOF

cat > "$PATTERNS/m9_pair.txt" <<'EOF'
        for prev in &out {
            if prev.secret == secret {
                reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
            }
        }
EOF
cat > "$PATTERNS/m9_every.txt" <<'EOF'
        for prev in &out {
            reeflog::flags::check_secret_uniqueness(&prev.id, &prev.url, id, url)?;
        }
EOF

mutate "M1 the uniqueness check disappears" \
    two_targets_with_one_secret_and_different_urls_are_refused \
    webhook.rs "$PATTERNS/m1_call.txt" "$PATTERNS/m1_no.txt" RED

mutate "M2 the shared-secret comparison is disabled" \
    two_targets_with_one_secret_and_different_urls_are_refused \
    webhook.rs "$PATTERNS/m2_secret_cmp.txt" "$PATTERNS/m2_never.txt" RED

mutate "M3 the check compares the row with itself" \
    two_targets_with_one_secret_and_different_urls_are_refused \
    webhook.rs "$PATTERNS/m3_self.txt" "$PATTERNS/m3_selfpair.txt" RED

mutate "M4 the refusal is swallowed" \
    two_targets_with_one_secret_and_different_urls_are_refused \
    webhook.rs "$PATTERNS/m4_call.txt" "$PATTERNS/m4_ignored.txt" RED

mutate "M5 the refusal drops the target ids" \
    two_targets_with_one_secret_and_different_urls_are_refused \
    flags.rs "$PATTERNS/m5_ids.txt" "$PATTERNS/m5_noids.txt" RED

mutate "M6 the refusal drops the repair" \
    the_refusal_is_one_line_the_operator_can_act_on \
    flags.rs "$PATTERNS/m6_fix.txt" "$PATTERNS/m6_nofix.txt" RED

mutate "M7 the refusal echoes the secret" \
    two_targets_with_one_secret_and_different_urls_are_refused \
    webhook.rs "$PATTERNS/m7_call.txt" "$PATTERNS/m7_echo.txt" RED

mutate "M8 the same-url exemption disappears (over-refusal)" \
    one_secret_on_two_targets_with_the_same_url_stays_legal \
    flags.rs "$PATTERNS/m8_same_url.txt" "$PATTERNS/m8_never.txt" RED

mutate "M9 every pair is judged, not only same-secret pairs (over-refusal)" \
    two_targets_with_distinct_secrets_still_work \
    webhook.rs "$PATTERNS/m9_pair.txt" "$PATTERNS/m9_every.txt" RED

echo ""
echo "ladder: 9/9 caught, tree byte-identical"
