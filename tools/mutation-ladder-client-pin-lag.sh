#!/usr/bin/env bash
# Mutation ladder for the client pin-lag gate (2026-11-11).
#
# The rule under test: THE NPM MANIFEST MUST NAME THE SAME RELEASE AS THE
# CRATE. Measured on the published v0.7.16 tag: manifest 0.7.15, crate
# 0.7.16, bootstrap() asked the release API for v0.7.15 — a stranger who
# installed the README's pin got the previous binary (B10,
# research/client-pin-lag-published-v0716-2026-11-11.json).
#
# A gate that cannot be made red certifies nothing, so every mutation below
# has to turn `node --test --test-name-pattern B10 test/bootstrap.test.mjs`
# red in the expected direction, and the tree has to come back
# BYTE-IDENTICAL afterwards (file-copying backup — never `git checkout`,
# which is how an earlier ladder in this family destroyed the very fix it
# proved; measured 2026-11-11 in this pulse: a debug `git checkout
# package.json` reverted the manifest fix itself).
#
# Versions are read from the tree, never hard-coded: a literal silently
# no-ops every sed after the next release bump (measured: M1 "lagged"
# 0.7.16 while package.json read 0.7.17, matched nothing, and the gate
# stayed green on an UN-MUTATED tree). Every sed is followed by a
# file-compare guard so a no-op counts as a LADDER BUG, not a catch.
#
# M1  the manifest lags behind the crate (the measured defect class) -> B10 red;
# M2  the manifest runs AHEAD of the crate -> B10 red (equality is symmetric);
# M3  a non-semver manifest ("next") -> the shape guard fires BY NAME;
# M4  the CRATE was bumped and the manifest forgotten (the mirror direction
#     a release can drift) -> B10 red;
# M5  a pre-release manifest (N-rc.1) -> the shape guard fires by name.
set -euo pipefail
cd "$(dirname "$0")/.."

TEST_FILTER="B10"

CRATE="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
PKG="$(node -e 'console.log(JSON.parse(require("fs").readFileSync("package.json","utf8")).version)')"
if [ -z "$CRATE" ] || [ "$CRATE" != "$PKG" ]; then
    echo "LADDER BUG: tree not at a single release (crate $CRATE, manifest $PKG) — set package.json back to the crate version and rerun" >&2
    exit 1
fi

gate_passes() {
    # Output to a file first: `grep -q` closing the pipe early SIGPIPEs node,
    # and `set -o pipefail` then reports the pipeline failed even when the
    # match succeeded (measured 2026-11-11).
    node --test --test-name-pattern "$TEST_FILTER" test/bootstrap.test.mjs > /tmp/gate-ladder.out 2>&1 || true
    grep -qE "^ℹ pass [1-9]" /tmp/gate-ladder.out
}

gate_output() {
    node --test --test-name-pattern "$TEST_FILTER" test/bootstrap.test.mjs > /tmp/gate-ladder.out 2>&1 || true
    cat /tmp/gate-ladder.out
}

assert_gate_red() {
    local name="$1"
    if gate_passes; then
        echo "LADDER BUG: mutation $name left the gate GREEN — a vacuous gate would certify anything" >&2
        exit 1
    fi
    echo "CAUGHT: $name"
}

assert_changed() {
    local name="$1" file="$2" pristine="$3"
    if cmp -s "$file" "$pristine"; then
        echo "LADDER BUG: $name matched nothing — a no-op mutation is vacuous" >&2
        exit 1
    fi
}

PRISTINE="$(mktemp -d /tmp/ladder-pinlag-pristine.XXXXXX)"
cp package.json "$PRISTINE/package.json"
cp test/bootstrap.test.mjs "$PRISTINE/bootstrap.test.mjs"
cp Cargo.toml "$PRISTINE/Cargo.toml"

restore() {
    # Tolerate a completed run: the success path removes $PRISTINE before
    # the EXIT trap fires, so `cp` from a gone backup must not turn a passing
    # ladder into a failure (measured 2026-11-11).
    [ -d "$PRISTINE" ] || return 0
    cp "$PRISTINE/package.json" package.json
    cp "$PRISTINE/bootstrap.test.mjs" test/bootstrap.test.mjs
    cp "$PRISTINE/Cargo.toml" Cargo.toml
}
trap restore EXIT

if ! gate_passes; then
    echo "LADDER BUG: the unmutated gate does not pass — nothing to prove" >&2
    exit 1
fi

# M1 — the manifest lags one release behind the crate.
LAGGED="$(node -e 'const p="'"$CRATE"'".split("."); p[1]=String(Math.max(0,+p[1]-1)); console.log(p.join("."))')"
sed -i "s/\"version\": \"$CRATE\"/\"version\": \"$LAGGED\"/" package.json
assert_changed "M1" package.json "$PRISTINE/package.json"
assert_gate_red "M1 (manifest lags at $LAGGED)"
restore

# M2 — the manifest runs ahead of the crate.
AHEAD="$(node -e 'const p="'"$CRATE"'".split("."); p[1]=String(+p[1]+1); console.log(p.join("."))')"
sed -i "s/\"version\": \"$CRATE\"/\"version\": \"$AHEAD\"/" package.json
assert_changed "M2" package.json "$PRISTINE/package.json"
assert_gate_red "M2 (manifest ahead at $AHEAD)"
restore

# M3 — non-semver manifest: the shape guard must fire with its own message.
sed -i "s/\"version\": \"$CRATE\"/\"version\": \"next\"/" package.json
assert_changed "M3" package.json "$PRISTINE/package.json"
gate_output > "$PRISTINE/m3.out"
if grep -q "plain semver" "$PRISTINE/m3.out"; then
    echo "CAUGHT: M3 (shape guard names the bad version)"
else
    echo "LADDER BUG: M3 did not fire with the semver message" >&2
    exit 1
fi
restore

# M4 — the mirror face: the CRATE was bumped, the manifest forgotten.
NEXT="$(node -e 'const p="'"$CRATE"'".split("."); p[1]=String(+p[1]+1); console.log(p.join("."))')"
sed -i "s/^version = \"$CRATE\"/version = \"$NEXT\"/" Cargo.toml
assert_changed "M4" Cargo.toml "$PRISTINE/Cargo.toml"
assert_gate_red "M4 (crate bumped to $NEXT, manifest left at $CRATE)"
restore

# M5 — a pre-release manifest: the shape guard must fire by name (a git tag
# is never a pre-release, and the request URL would carry a tag that does
# not exist).
sed -i "s/\"version\": \"$CRATE\"/\"version\": \"$CRATE-rc.1\"/" package.json
assert_changed "M5" package.json "$PRISTINE/package.json"
gate_output > "$PRISTINE/m5.out"
if grep -q "plain semver" "$PRISTINE/m5.out"; then
    echo "CAUGHT: M5 (shape guard names the pre-release version)"
else
    echo "LADDER BUG: M5 did not fire with the semver message" >&2
    exit 1
fi
restore

# Byte-identical tree after the ladder (backups are stored FLAT: the test
# file's backup is $PRISTINE/bootstrap.test.mjs, not a nested path — a
# nested check against a missing file reads as drift, measured 2026-11-11).
for pair in "package.json:package.json" "Cargo.toml:Cargo.toml" "test/bootstrap.test.mjs:bootstrap.test.mjs"; do
    f="${pair%%:*}"; b="${pair##*:}"
    if ! cmp -s "$f" "$PRISTINE/$b"; then
        echo "LADDER BUG: $f not byte-identical after restore" >&2
        exit 1
    fi
done
rm -rf "$PRISTINE" /tmp/gate-ladder.out
echo "TREE byte-identical: package.json + Cargo.toml + test/bootstrap.test.mjs"
echo "LADDER 5/5 CAUGHT"
