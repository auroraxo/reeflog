#!/usr/bin/env bash
# Mutation ladder for the refusal-voice fix (src/main.rs: die + escape_for_message).
#
# Falsification-first: each mutation weakens ONE rule, runs the exact test
# that pins it, and asserts RED. If the test passes under the mutation, the
# test is vacuous and the ladder fails.
#
# HARDENED the same way as tools/mutation-ladder-webhook-value.sh (which was
# hardened after two real instrument failures found by running it):
#
#   1. DESTROYER: the restore is a COPY, never `git checkout` — git would
#      reset to HEAD and WIPE the uncommitted fix.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="error_voice"

echo "=== refusal voice mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-ev-pristine.XXXXXX)"
mkdir -p "$PRISTINE/src"
cp src/main.rs "$PRISTINE/src/main.rs"
restore_all() {
    if [ -f "$PRISTINE/src/main.rs" ]; then
        cp "$PRISTINE/src/main.rs" src/main.rs
        touch src/main.rs
    fi
}
trap restore_all EXIT

mutate() {
    local name="$1"
    local test_fn="$2"
    local search="$3"
    local replace="$4"

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
' src/main.rs "$search" "$replace" || {
        echo "" >&2
        echo "LADDER: pattern missing in src/main.rs: $search" >&2
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

# 1. The escaping is dropped entirely -> the one-line test
mutate "drop the escaping entirely" \
    "a_refusal_whose_value_contains_a_newline_stays_one_line" \
    "eprintln!(\"reeflog: error: {}\", escape_for_message(&msg.to_string()));" \
    "eprintln!(\"reeflog: error: {}\", msg.to_string());"

# 2. Only the newline is escaped, the other control bytes stay raw ->
#    the raw-control-byte test
mutate "escape only the newline" \
    "a_refusal_never_emits_raw_control_bytes" \
    "            '\\n' => out.push_str(\"\\\\n\")," \
    "            '\\n' => out.push_str(\"\\\\n\"),
            _ => out.push(ch),"

# 3. ESC is not escaped -> the hostile-target test
mutate "leave the ESC byte raw" \
    "a_hostile_target_flag_value_is_named_in_escape_form_not_raw" \
    "            c if (c as u32) < 0x20 => out.push_str(&format!(\"\\\\x{:02x}\", c as u32))," \
    "            c if (c as u32) < 0x20 && c != '\\u{1b}' => out.push_str(&format!(\"\\\\x{:02x}\", c as u32)),"

# 4. HTAB is escaped too -> the guard that says it must stay literal
mutate "escape the tab as well" \
    "a_tab_is_not_escaped" \
    "            '\\t' => out.push(ch), // HTAB: cannot end a line, stays literal" \
    "            '\\t' => out.push_str(\"\\\\t\"),"

# 5. The refusal stops naming the value -> the test that pins the message
mutate "stop naming the value" \
    "the_refusal_still_names_flag_value_and_domain" \
    "fn escape_for_message(s: &str) -> String {" \
    "fn escape_for_message(s: &str) -> String { return String::from(\"value refused\");"

echo "=== ladder complete: all 5 mutations caught ==="

if ! cmp -s "$PRISTINE/src/main.rs" src/main.rs; then
    echo "LADDER BUG: src/main.rs was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
