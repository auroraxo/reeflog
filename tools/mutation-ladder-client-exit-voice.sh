#!/usr/bin/env bash
# Mutation ladder for the EXIT-CODE VOICE fix (PULS W, 2026-11-12).
#
# The rule under test: run() accepts ONLY the documented codes (0/1/3) on
# the typed API; every other close — exit 2 (journal write failure), 64
# (usage/skew), 101 (panic), signal (code null) — is a refusal carrying the
# REAL code and the binary's words; raw() stays the unclassified hatch.
#
# Measured on the published v0.7.19 tree: all six defect faces visible
# (research/client-exit-voice-2026-11-12.json). A gate that cannot be made
# red certifies nothing, so every mutation below has to turn
# `node --test --test-name-pattern <pin> test/exit-code-voice.test.mjs`
# red in the expected direction, and the tree has to come back
# BYTE-IDENTICAL afterwards (file-copying backup — never `git checkout`).
#
# Every mutation is followed by a cmp guard so a no-op sed counts as a
# LADDER BUG, not a catch (the vacuous-mutation class measured in this
# family before: a hard-coded pattern that silently stopped matching).
set -euo pipefail
cd "$(dirname "$0")/.."

TARGET="index.js"
BACKUP="$(mktemp /tmp/reeflog-ladder-XXXXXX.bak)"
cp "$TARGET" "$BACKUP"
restore() { cp "$BACKUP" "$TARGET"; }
trap restore EXIT

gate_red() {
    # $1 = test-name pattern. Output to a file first (grep -q closing the
    # pipe early SIGPIPEs node under `set -o pipefail`).
    # "Red" means at least one FAIL line: the summary can read `fail 1`
    # with `pass 0` when every matching test fails — the summary alone is
    # not the verdict (measured: M1 produced pass 0 / fail 1 and the
    # summary-only predicate misread it as green).
    node --test --test-name-pattern "$1" test/exit-code-voice.test.mjs > /tmp/ladder-ecv.out 2>&1 || true
    grep -qE "^✖ " /tmp/ladder-ecv.out
}

gate_green() {
    node --test --test-name-pattern "$1" test/exit-code-voice.test.mjs > /tmp/ladder-ecv.out 2>&1 || true
    grep -qE "^ℹ fail 0$" /tmp/ladder-ecv.out
}

# The BASELINE (unmutated fixed tree) must be GREEN before mutating —
# otherwise red proves nothing about the mutation.
if ! gate_green 'F1|F2|F3|F4|G1|G2|G3|G4'; then
    echo "LADDER BUG: baseline gate is not green before any mutation" >&2
    cat /tmp/ladder-ecv.out >&2
    exit 1
fi

mutate() {
    local name="$1"
    shift
    cp "$BACKUP" "$TARGET"
    python3 - "$@" <<'PY'
import sys
path = 'index.js'
old, new = sys.argv[1], sys.argv[2]
s = open(path).read()
n = s.count(old)
if n != 1:
    sys.exit(f'LADDER BUG: pattern matched {n} times (must be exactly 1): {old[:60]!r}')
open(path, 'w').write(s.replace(old, new))
PY
    if cmp -s "$BACKUP" "$TARGET"; then
        echo "LADDER BUG: mutation $name changed nothing (vacuous)" >&2
        exit 1
    fi
}

assert_red() {
    local name="$1" pin="$2"
    if ! gate_red "$pin"; then
        echo "LADDER BUG: mutation $name left the gate GREEN on '$pin' — a vacuous gate would certify anything" >&2
        cat /tmp/ladder-ecv.out >&2
        exit 1
    fi
    echo "CAUGHT: $name (red on '$pin')"
}

# M1 — the measured defect class: non-contract codes resolve as success.
mutate 'M1 unknown codes resolve as success' \
    "      if (!unclassified && exitCode !== 0 && exitCode !== 3) {" \
    "      if (false && !unclassified && exitCode !== 0 && exitCode !== 3) {"
assert_red 'M1' 'F1'

# M2 — the forgery class: a kill goes back to a minted "exit 1".
mutate 'M2 signal death forges exit 1' \
    "      if (!unclassified && code == null) {" \
    "      if (false && !unclassified && code == null) {"
assert_red 'M2' 'F4'

# M3 — the verdict names no words: stderr dropped from the refusal.
mutate 'M3 verdict drops the binary words' \
    "        const first = stderr.split('\n').find((l) => l.trim()) || stderr.trim();" \
    "        const first = '';"
assert_red 'M3' 'F1'

# M4 — raw() loses its escape hatch (the documented contract of raw).
mutate 'M4 raw() becomes classified' \
    "    return run(this.bin, args, { allowExit3: true, unclassified: true });" \
    "    return run(this.bin, args, { allowExit3: true });"
assert_red 'M4' 'G4'

# M5 — the code in the verdict is a lie (always claims 0).
mutate 'M5 verdict mints exitCode 0' \
    "          \`reeflog exited with code \${exitCode}\${hint} — refusing to call it a verdict\${first ? \`: \${first}\` : ''}\`," \
    "          \`reeflog exited with code 0\${hint} — refusing to call it a verdict\${first ? \`: \${first}\` : ''}\`,"
assert_red 'M5' 'F1'

# M6 — SIGKILL/SIGTERM forgery inside the signal message itself
# (claims the signal but keeps no real exitCode discipline): we drop the
# signal NAME, which is the whole face of F4.
mutate 'M6 signal message loses the signal name' \
    "          \`reeflog was killed by \${signal ?? 'an unknown signal'} — no verdict was produced\${stderr.trim() ? \`: \${stderr.trim()}\` : ''}\`," \
    "          \`reeflog was killed — no verdict was produced\${stderr.trim() ? \`: \${stderr.trim()}\` : ''}\`,"
assert_red 'M6' 'F4'

# Restore explicitly BEFORE checking: `trap ... EXIT` fires only at script
# end, and the last mutation is still on disk at this point (measured: the
# final cmp caught its own tree in the M6 state).
restore

# All mutations caught — the tree must be byte-identical to the backup.
if ! cmp -s "$BACKUP" "$TARGET"; then
    echo "LADDER BUG: $TARGET not restored byte-identically" >&2
    exit 1
fi
# And the fixed gate must be green again after every restore.
if ! gate_green 'F1|F2|F3|F4|G1|G2|G3|G4'; then
    echo "LADDER BUG: gate not green after restores" >&2
    cat /tmp/ladder-ecv.out >&2
    exit 1
fi
echo "LADDER 6/6 CAUGHT, $TARGET byte-identical, gate green"
