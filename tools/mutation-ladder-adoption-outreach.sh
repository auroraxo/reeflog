#!/usr/bin/env bash
# Mutation ladder for the ADOPTION-OUTREACH classification (PULS AH
# follow-up, 2026-11-13).
#
# The rule under test: an unsolicited outreach/invitation issue from a
# foreign agent is NOT consumer evidence. The adoption probe flipped to
# user-one-evidence on issue #2 (webbeep / SloppableAI, 2026-11-03) because
# the old filter excluded only logins ending in "[bot]" and outreach
# senders omit the suffix. External users remained 0 the whole time.
#
# A gate that cannot be made red certifies nothing, so every mutation below
# must turn its exact test RED with a VALID file (a syntax error is not a
# behavioural catch), and the tree must come back BYTE-IDENTICAL afterwards
# (file-copying backup — never `git checkout`).
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

TEST="adoption-outreach-classification"
TARGET="tools/adoption-classify.mjs"

echo "=== adoption-outreach-classification mutation ladder ==="

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

PRISTINE="$(mktemp -d /tmp/ladder-adopt-pristine.XXXXXX)"
cp "$TARGET" "$PRISTINE/adoption-classify.mjs"
restore_all() {
    if [ -f "$PRISTINE/adoption-classify.mjs" ]; then
        cp "$PRISTINE/adoption-classify.mjs" "$TARGET"
        touch "$TARGET"
    fi
}
trap restore_all EXIT

# mutate <test-name-filter> <label> <python-mutation-body>
# The mutation body receives the source as `s` and must assign to `s`.
mutate() {
    local filter="$1" label="$2" body="$3"
    assert_filter_matches "$filter"
    echo -n "mutation: $label ... "

        cp "$TARGET" /tmp/ladder-adopt-before.mjs
    SD_BODY="$body" python3 - "$TARGET" <<'PY'
import os, sys
path = sys.argv[1]
src = open(path).read()
ns = {'s': src}
exec(os.environ['SD_BODY'], ns)
if ns['s'] == src:
    sys.exit(0)
open(path, 'w').write(ns['s'])
PY
    if cmp -s /tmp/ladder-adopt-before.mjs "$TARGET"; then
        echo "NO-OP (mutation pattern did not change the source) — the ladder itself is broken" >&2
        exit 1
    fi

    if ! node --check "$TARGET" 2>/dev/null && ! node -e "import('./$TARGET')" 2>/dev/null; then
        echo "BROKEN (module no longer loads) — not a behavioural catch" >&2
        restore_all
        exit 1
    fi

    if node --test --test-name-pattern="$filter" "test/${TEST}.test.mjs" > /tmp/ladder-adopt-run.log 2>&1; then
        if grep -qE '^# pass [1-9]' /tmp/ladder-adopt-run.log; then
            echo "ESCAPED — the gate did not catch it" >&2
            restore_all
            exit 1
        fi
        echo "ESCAPED — no test ran under this pattern" >&2
        restore_all
        exit 1
    fi

    restore_all
    if ! cmp -s "$PRISTINE/adoption-classify.mjs" "$TARGET"; then
        echo "RESTORE MISMATCH — the tree is not byte-identical" >&2
        exit 1
    fi
    echo "CAUGHT"
}

# M1 — the classifier never excludes anything: the exact pre-fix behaviour
# (every outsider issue counted as user-one evidence). A1/A2/A4 must go red.
mutate "A1 the literal webbeep issue" \
    "the classifier never excludes (pre-fix behaviour)" \
    "s = s.replace(\"  return reasons.length > 0 ? { kind: 'outreach', reasons } : { kind: 'usage', reasons: [] };\", \"  return { kind: 'usage', reasons: [] };\")"

# M2 — the TITLE rule is removed: an invitation whose body is neutral must
# still be excluded. A1b must go red.
mutate "A1b the title alone identifies an invitation" \
    "the title rule is dropped" \
    "s = s.replace(\"  if (OUTREACH_TITLE.test(title)) reasons.push('title:invite/outreach');\", \"  // title rule removed\")"

# M3 — the BODY rules are removed: the literal issue #2 still has an
# inviting TITLE, so A1 stays green — but A4 (body-only invitation) must
# go red. This pins that both channels work.
mutate "A4 outreach recognition does not need the magic word in the title" \
    "the body rules are dropped" \
    "s = s.replace('  for (const re of OUTREACH_BODY) {', '  for (const re of []) {')"

# M4 — the split function puts everything into the usage bucket (a
# plausible refactor bug: the classifier is right but the wiring is not).
# A2 must go red.
mutate "A2 splitIssuesByUsage removes it and keeps the census honest" \
    "the split drops the outreach bucket (wiring bug)" \
    "s = s.replace('    (c.kind === \'outreach\' ? outreach : usage).push(row);', '    usage.push(row);')"

# M5 — over-blocking: the classifier excludes EVERYTHING, so genuine
# usage is silently swallowed. A3 must go red. This is the mirror failure
# of M1 — a gate that only knows one direction is not a gate.
mutate "A3 guards: genuine outsider usage is never excluded" \
    "the classifier excludes everything (over-blocking)" \
    "s = s.replace(\"  return reasons.length > 0 ? { kind: 'outreach', reasons } : { kind: 'usage', reasons: [] };\", \"  return { kind: 'outreach', reasons: reasons.length ? reasons : ['body:default'] };\")"

echo -n "control (restored tree) ... "
if node --test "test/${TEST}.test.mjs" > /dev/null 2>&1; then
    echo "OK (green)"
else
    echo "FAILED: restored tree does not pass" >&2
    exit 1
fi

if ! cmp -s "$PRISTINE/adoption-classify.mjs" "$TARGET"; then
    echo "FINAL RESTORE MISMATCH — the tree is not byte-identical" >&2
    exit 1
fi

echo "=== ladder complete: 5/5 CAUGHT, control green, tree byte-identical"
