#!/usr/bin/env bash
# Mutation ladder for the SELF-DEPENDENCY fix (PULS AH, 2026-11-13).
#
# The rule under test: the published package must not depend on its own
# name, and carries no dependencies at all (every import in index.js is a
# node: builtin). Measured on the PUBLISHED v0.7.24 bytes
# (research/npm-self-dependency-published-v0724-2026-11-13.json, verdict
# self-dependency-present, 3/3 defect faces): the install cloned a second,
# older copy of reeflog-client over git+ssh and FAILED outright on a host
# without git.
#
# A gate that cannot be made red certifies nothing, so every mutation
# below must turn its exact test RED with a VALID manifest (a JSON error
# is not a behavioural catch), and the tree must come back
# BYTE-IDENTICAL afterwards (file-copying backup — never `git checkout`).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="npm-self-dependency"

echo "=== npm-self-dependency mutation ladder ==="

echo -n "unmutated tree ... "
if node --test "test/${TEST}.test.mjs" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: unmutated tree does not pass" >&2
    exit 1
fi

LIST="$(node --test --test-reporter=tap "test/${TEST}.test.mjs" 2>/dev/null | grep -E '^# Subtest: ' | sed 's/^# Subtest: //')"

assert_filter_matches() {
    local filter="$1"
    if ! printf '%s\n' "$LIST" | grep -qF "$filter"; then
        echo "" >&2
        echo "LADDER BUG: filter matches no test in test/${TEST}.test.mjs — CAUGHT would be vacuous" >&2
        printf '%s\n' "$LIST" | sed 's/^/    available: /' >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-sd-pristine.XXXXXX)"
cp package.json "$PRISTINE/package.json"
restore_all() {
    if [ -f "$PRISTINE/package.json" ]; then
        cp "$PRISTINE/package.json" package.json
        touch package.json
    fi
}
trap restore_all EXIT

# add_dep <test-name-filter> <dep-key> <dep-spec> <mutation-label>
add_dep() {
    local filter="$1" key="$2" spec="$3" label="$4"
    assert_filter_matches "$filter"
    echo -n "mutation: $label ... "

    # VACUUM GUARD: the pristine tree must have no dependencies at all,
    # otherwise "adding one" might be a no-op against a leftover.
    if node -e "const d=JSON.parse(require('fs').readFileSync('package.json','utf8')).dependencies||{}; process.exit(Object.keys(d).length===0?0:1)"; then
        :
    else
        echo "VACUUM (tree already declares dependencies before mutating)" >&2
        exit 1
    fi

    SD_KEY="$key" SD_SPEC="$spec" node -e "
      const fs=require('fs');
      const p=JSON.parse(fs.readFileSync('package.json','utf8'));
      p.dependencies = Object.assign(p.dependencies||{}, { [process.env.SD_KEY]: process.env.SD_SPEC });
      fs.writeFileSync('package.json', JSON.stringify(p,null,2)+'\n');
    "

    if ! node -e "JSON.parse(require('fs').readFileSync('package.json','utf8'))" 2>/dev/null; then
        echo "BROKEN (manifest no longer parses) — not a behavioural catch" >&2
        cp "$PRISTINE/package.json" package.json
        exit 1
    fi

    if node --test --test-name-pattern="$filter" "test/${TEST}.test.mjs" > /tmp/ladder-sd-run.log 2>&1; then
        # A pattern that matches nothing also exits 0 — verify a test ran.
        if grep -qE '^# pass [1-9]' /tmp/ladder-sd-run.log; then
            echo "ESCAPED — the gate did not catch it" >&2
            cp "$PRISTINE/package.json" package.json
            exit 1
        fi
        echo "ESCAPED — no test ran under this pattern" >&2
        cp "$PRISTINE/package.json" package.json
        exit 1
    fi

    cp "$PRISTINE/package.json" package.json
    if ! cmp -s "$PRISTINE/package.json" package.json; then
        echo "RESTORE MISMATCH — the tree is not byte-identical" >&2
        exit 1
    fi
    echo "CAUGHT"
}

# M1 — the self-dependency returns at the exact pin the defect shipped
# with (the v0.7.24 manifest's own line). T1 must go red.
add_dep "T1: the manifest declares no dependency on its own name" \
    "reeflog-client" "github:auroraxo/reeflog#v0.7.22" \
    "self-dependency returns (github: pin, the shipped defect)"

# M2 — the self-dependency returns pinned to the CURRENT release, so it
# is not stale. The defect is the dependency itself, not the lag: the
# gate must still catch it.
add_dep "T1: the manifest declares no dependency on its own name" \
    "reeflog-client" "github:auroraxo/reeflog#v0.7.25" \
    "self-dependency returns (pinned to the current release)"

# M3 — a semver self-dependency instead of a git URL: T1 keys on the NAME,
# not on the github: pattern.
add_dep "T1: the manifest declares no dependency on its own name" \
    "reeflog-client" "^0.7.0" \
    "self-dependency returns as a registry range"

# M4 — a FOREIGN dependency: the client needs nothing (T2 pins that), so
# any dependency at all is a regression of the zero-dep promise.
# NB: --test-name-pattern is a REGEX, so the filter must avoid the
# parentheses in the test title (a non-matching pattern would exit green
# and read as an escape).
add_dep "T2: the client carries no dependencies at all" \
    "left-pad" "1.3.0" \
    "a foreign dependency appears"

echo -n "control (restored tree) ... "
if node --test "test/${TEST}.test.mjs" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: restored tree does not pass" >&2
    exit 1
fi

if ! cmp -s "$PRISTINE/package.json" package.json; then
    echo "FINAL RESTORE MISMATCH — the tree is not byte-identical" >&2
    exit 1
fi

echo "=== ladder complete: 4/4 CAUGHT, control green, tree byte-identical"
