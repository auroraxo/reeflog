#!/usr/bin/env bash
# Mutation ladder for the webhook value domain fix (flags.rs / webhook.rs):
# target id, secret, url.
#
# Falsification-first: each mutation weakens one rule, runs the exact test
# that pins it, and asserts RED. If the test passes under the mutation, the
# test is vacuous and the ladder fails.
#
# HARDENED 2026-11-10 after two real instrument failures found by running it:
#
#   1. DESTROYER: the restore used to be `git checkout "$file"`, which resets
#      to HEAD and WIPED the uncommitted fix on the first mutation run. The
#      fix was recovered from the captured diff. Now the ladder copies the
#      pristine bytes aside BEFORE the first mutation and restores by copy
#      (git is never used to restore working-tree files; a trap restores on
#      any exit).
#
#   2. VACUOUS CATCHES: five of six test filters matched ZERO tests (they were
#      written against test names that never existed), so "cargo test -- filter"
#      passed by matching nothing and the ladder reported CAUGHT anyway —
#      only 1 of 6 mutations was ever really caught. The ladder now PROVES
#      each filter matches at least one real test BEFORE mutating, and fails
#      loudly if not.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="webhook_value_domain"

echo "=== webhook value domain mutation ladder ==="

# Baseline: the test must pass on the unmutated tree
echo -n "unmutated tree ... "
if cargo test --release --test "$TEST" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: unmutated tree does not pass" >&2
    exit 1
fi

# Every filter must select at least one real test, or the catch below is a
# no-op. `cargo test -- --list` prints "<name>: test" for each test.
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

# Pristine copies of every file the ladder touches, taken BEFORE any mutation.
PRISTINE="$(mktemp -d /tmp/ladder-pristine.XXXXXX)"
for f in src/flags.rs src/webhook.rs; do
    mkdir -p "$PRISTINE/$(dirname "$f")"
    cp "$f" "$PRISTINE/$f"
done
restore_all() {
    for f in src/flags.rs src/webhook.rs; do
        if [ -f "$PRISTINE/$f" ]; then
            cp "$PRISTINE/$f" "$f"
            touch "$f"
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

    # The mutation must COMPILE: a compile error is not a behavioural catch.
    if ! cargo build --release > /dev/null 2>&1; then
        echo "COMPILE ERROR (not a behavioural catch) — mutation must compile" >&2
        restore_all
        exit 1
    fi

    # Run the targeted test; it MUST fail.
    if cargo test --release --test "$TEST" -- "$test_fn" > /dev/null 2>&1; then
        echo "VACUOUS! (test passed under mutation)" >&2
        restore_all
        exit 1
    else
        echo "CAUGHT (went red, as expected)"
    fi
    restore_all
}

# 1. Target ID: drop leading-whitespace check -> an_id_with_leading_whitespace_is_refused_by_the_config
mutate "allow leading whitespace in target id" \
    "an_id_with_leading_whitespace_is_refused_by_the_config" \
    "src/flags.rs" \
    "if value.starts_with([' ', '\t']) {" \
    "if false && value.starts_with([' ', '\t']) {"

# 2. Target ID: let the header validator judge a constant instead of the
#    operator's id -> forbidden control byte test. The mutation COMPILES and
#    only the behaviour can catch it (an earlier version of this mutation was
#    caught by a compile error, and an even earlier one by a zero-test filter).
mutate "allow control bytes in target id" \
    "an_id_with_a_forbidden_control_byte_is_refused_by_the_config" \
    "src/flags.rs" \
    "if let Err(e) = http::HeaderValue::from_str(value) {" \
    "if let Err(e) = http::HeaderValue::from_str(\"ok-value\") {"

# 3. Target ID: forbid internal space (over-refusal mutation) -> plain/internal space guard
mutate "over-refuse internal space in target id" \
    "an_id_with_an_internal_space_or_tab_still_loads" \
    "src/flags.rs" \
    "if value.starts_with([' ', '\t']) {" \
    "if value.contains(' ') || value.starts_with([' ', '\t']) {"

# 4. URL: drop port-0 check -> port 0 refusal test
mutate "allow port 0 in URL" \
    "a_url_with_port_zero_is_refused_by_the_config" \
    "src/flags.rs" \
    "Ok(0) => {" \
    "Ok(0) => {} _ => {"

# 5. URL: drop non-ASCII check -> non-ASCII refusal test (predicate never
#    matches: the mutation compiles, only the behaviour catches it)
mutate "allow raw non-ASCII in URL path" \
    "a_url_with_a_non_ascii_character_is_refused_by_the_config" \
    "src/flags.rs" \
    "before_fragment.char_indices().find(|(_, c)| !c.is_ascii())" \
    "before_fragment.char_indices().find(|(_, _)| false)"

# 6. Secret: allow empty secret -> empty secret refusal test
mutate "allow empty secret" \
    "an_empty_secret_is_refused_by_the_config" \
    "src/flags.rs" \
    "pub fn check_secret_domain(label: &str, value: &str) -> Result<(), String> {" \
    "pub fn check_secret_domain(label: &str, value: &str) -> Result<(), String> { return Ok(());"

echo "=== ladder complete: all 6 mutations caught ==="

# Prove the tree came back byte-identical to what we started with.
for f in src/flags.rs src/webhook.rs; do
    if ! cmp -s "$PRISTINE/$f" "$f"; then
        echo "LADDER BUG: $f was not restored byte-identically" >&2
        exit 1
    fi
done
echo "tree restored byte-identically"
