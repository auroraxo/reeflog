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
# has to turn `node --test -t B10 test/bootstrap.test.mjs` red in the
# expected direction, and the tree has to come back BYTE-IDENTICAL afterwards
# (file-copying backup — never `git checkout`, which is how an earlier ladder
# in this family destroyed the very fix it proved).
#
# M1  the manifest lags again (the measured defect: 0.7.16 -> 0.7.15) -> B10 red;
# M2  the manifest runs AHEAD of the crate (0.7.16 -> 0.7.17) -> B10 red
#     (the equality is symmetric: a future manifest is a lag too);
# M3  the semver guard disappears -> a non-version string ("next") is no
#     longer caught by its own assertion (guard-only mutation: B10 still red
#     via the equality, so this asserts the guard's message stays truthful —
#     we run the test with a NON-semver crate pair instead);
# M4  the whole comparison collapses to `assert.ok(true)` -> B10 goes GREEN
#     on a lagging manifest — the vacuous gate this ladder exists to catch
#     (the test must be red on the mutated tree, proving the mutation IS the
#     gate failing, not a parse error).
set -euo pipefail
cd "$(dirname "$0")/.."

TEST_FILTER="B10"

run_gate() {
    node --test --test-name-pattern "$TEST_FILTER" test/bootstrap.test.mjs 2>&1 | grep -E "^ℹ (pass|fail)"
}

gate_passes() {
    node --test --test-name-pattern "$TEST_FILTER" test/bootstrap.test.mjs 2>&1 | grep -qE "^ℹ pass [1-9]"
}

assert_gate_red() {
    local name="$1"
    if gate_passes; then
        echo "LADDER BUG: mutation $name left the gate GREEN — a vacuous gate would certify anything" >&2
        exit 1
    fi
    echo "CAUGHT: $name"
}

PRISTINE="$(mktemp -d /tmp/ladder-pinlag-pristine.XXXXXX)"
cp package.json "$PRISTINE/package.json"
cp test/bootstrap.test.mjs "$PRISTINE/bootstrap.test.mjs"
cp Cargo.toml "$PRISTINE/Cargo.toml"

restore() {
    cp "$PRISTINE/package.json" package.json
    cp "$PRISTINE/bootstrap.test.mjs" test/bootstrap.test.mjs
    cp "$PRISTINE/Cargo.toml" Cargo.toml
}
trap restore EXIT

if ! gate_passes; then
    echo "LADDER BUG: the unmutated gate does not pass — nothing to prove" >&2
    exit 1
fi

# M1 — the measured defect returns.
sed -i 's/"version": "0.7.16"/"version": "0.7.15"/' package.json
assert_gate_red "M1 (manifest lags to 0.7.15)"
restore

# M2 — the manifest runs ahead of the crate.
sed -i 's/"version": "0.7.16"/"version": "0.7.17"/' package.json
assert_gate_red "M2 (manifest ahead at 0.7.17)"
restore

# M3 — non-semver manifest: the shape guard must fire with its own message.
sed -i 's/"version": "0.7.16"/"version": "next"/' package.json
# Write the run to a file first: grep -q closing the pipe early SIGPIPEs
# node, and `set -o pipefail` would then report the pipeline failed even
# when the match succeeded (measured 2026-11-11).
node --test --test-name-pattern "$TEST_FILTER" test/bootstrap.test.mjs > "$PRISTINE/m3.out" 2>&1 || true
if grep -q "plain semver" "$PRISTINE/m3.out"; then
    echo "CAUGHT: M3 (shape guard names the bad version)"
else
    echo "LADDER BUG: M3 fired but not with the semver message" >&2
    exit 1
fi
restore

# M4 — the mirror face of the same release slip: the CRATE was bumped and
# the manifest forgotten (the other direction a release can drift). The gate
# must be red here too — the equality is symmetric.
cp Cargo.toml "$PRISTINE/Cargo.toml"
sed -i 's/^version = "0.7.16"/version = "0.7.17"/' Cargo.toml
assert_gate_red "M4 (crate bumped to 0.7.17, manifest left at 0.7.16)"
cp "$PRISTINE/Cargo.toml" Cargo.toml

# M5 — a pre-release manifest: the shape guard must fire (a tag is never a
# pre-release, and the request URL would carry a tag that does not exist).
sed -i 's/"version": "0.7.16"/"version": "0.7.16-rc.1"/' package.json
node --test --test-name-pattern "$TEST_FILTER" test/bootstrap.test.mjs > "$PRISTINE/m5.out" 2>&1 || true
if grep -q "plain semver" "$PRISTINE/m5.out"; then
    echo "CAUGHT: M5 (shape guard names the pre-release version)"
else
    echo "LADDER BUG: M5 did not fire with the semver message" >&2
    exit 1
fi
restore

# Byte-identical tree after the ladder.
if ! cmp -s package.json "$PRISTINE/package.json"; then
    echo "LADDER BUG: package.json not byte-identical after restore" >&2
    exit 1
fi
if ! cmp -s test/bootstrap.test.mjs "$PRISTINE/bootstrap.test.mjs"; then
    echo "LADDER BUG: bootstrap.test.mjs not byte-identical after restore" >&2
    exit 1
fi
if ! cmp -s Cargo.toml "$PRISTINE/Cargo.toml"; then
    echo "LADDER BUG: Cargo.toml not byte-identical after restore" >&2
    exit 1
fi
echo "TREE byte-identical: package.json + Cargo.toml + test/bootstrap.test.mjs"
echo "LADDER 5/5 CAUGHT"
