#!/usr/bin/env bash
# Mutation ladder for the program-identity fix (src/main.rs:
# journal_program_refusal, resolve_journal_program, the init guard).
#
# Falsification-first: each mutation weakens ONE rule, runs the exact test
# that pins it, and asserts RED. If the test passes under the mutation, the
# test is vacuous and the ladder fails.
#
# HARDENED the same way as tools/mutation-ladder-error-voice.sh:
#
#   1. DESTROYER: the restore is a COPY, never `git checkout` — git would
#      reset to HEAD and WIPE the uncommitted fix.
#   2. VACUOUS CATCHES: every filter must select at least one real test
#      BEFORE mutating, and a COMPILE ERROR is not a behavioural catch.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="program_identity_domain"

echo "=== program identity mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-pi-pristine.XXXXXX)"
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

# 1. `init` re-points the journal again -> the init ladder test
mutate "let init re-point the journal at another program" \
    "init_must_not_repoint_a_journal_at_another_program" \
    "                    if recorded != program {
                        die(journal_program_refusal(&db, \"init\", &recorded, &program));
                    }" \
    "                    if false {
                        die(journal_program_refusal(&db, \"init\", &recorded, &program));
                    }"

# 2. `verify` silently ignores a mismatching program again
mutate "let verify ignore a different program" \
    "verify_must_not_silently_ignore_a_different_program" \
    "    let program = resolve_journal_program(&j, &db, \"verify\", arg(args, \"--program\").as_ref());" \
    "    let program = j.meta_get(\"program\").ok().flatten().unwrap_or_else(|| require(args, \"--program\"));"

# 3. `repair` silently ignores a mismatching program again
mutate "let repair ignore a different program" \
    "repair_must_not_silently_ignore_a_different_program" \
    "    let program = resolve_journal_program(&j, &db, \"repair\", arg(args, \"--program\").as_ref());" \
    "    let program = j.meta_get(\"program\").ok().flatten().unwrap_or_else(|| require(args, \"--program\"));"

# 4. The refusal stops naming the GIVEN program -> the naming test
mutate "stop naming the given program" \
    "verify_must_not_silently_ignore_a_different_program" \
    "        \"journal {} watches {recorded}, refusing {command} --program {given} — one journal, one program; the recorded program is the journal's identity, pass --db <other-journal> to work on a different one\"," \
    "        \"journal {} watches {recorded}, refusing {command} — one journal, one program; the recorded program is the journal's identity, pass --db <other-journal> to work on a different one\","

# 5. The refusal stops naming the RECORDED program -> the naming test
mutate "stop naming the recorded program" \
    "init_must_not_repoint_a_journal_at_another_program" \
    "        \"journal {} watches {recorded}, refusing {command} --program {given} — one journal, one program; the recorded program is the journal's identity, pass --db <other-journal> to work on a different one\"," \
    "        \"journal {} refuses {command} --program {given} — one journal, one program; the recorded program is the journal's identity, pass --db <other-journal> to work on a different one\","

echo "=== ladder complete: all 5 mutations caught ==="

if ! cmp -s "$PRISTINE/src/main.rs" src/main.rs; then
    echo "LADDER BUG: src/main.rs was not restored byte-identically" >&2
    exit 1
fi
echo "tree restored byte-identically"
