#!/usr/bin/env bash
# MUTATION LADDER for test/registry-channel-surface.test.mjs (2026-11-13).
#
# Every mutation must turn the gate RED; the untouched CONTROL must be
# GREEN; the README must be byte-identical after each restore (cmp-guard).
# A vacuous gate (one that is red or green regardless of the mutation)
# fails the VACUUM check below.
set -uo pipefail
cd "$(dirname "$0")/.."
README=README.md
GATE=(node --test test/registry-channel-surface.test.mjs)
BACKUP=$(mktemp)
cp "$README" "$BACKUP"
restore() { cp "$BACKUP" "$README"; }
trap restore EXIT

run_gate() { # prints 0 when all gate tests pass
  "${GATE[@]}" >/dev/null 2>&1
}

fail=0

# VACUUM: the unmodified tree must pass, else every "caught" is vacuous.
if run_gate; then echo "CONTROL GREEN (vacuum ok)"; else echo "CONTROL RED — LADDER BUG"; fail=1; fi

mutate_then_expect_red() {
  local name="$1" sedexpr="$2" checkcmd="$3"
  # apply the mutation and prove it actually landed (no-op sed = vacuous)
  cp "$BACKUP" "$README"
  sed -i "$sedexpr" "$README"
  if ! eval "$checkcmd"; then
    echo "LADDER BUG ($name): mutation did not land"; fail=1
    cp "$BACKUP" "$README"; return
  fi
  if run_gate; then
    echo "ESCAPED ($name): mutation landed but gate stayed green"; fail=1
  else
    echo "CAUGHT ($name)"
  fi
  cp "$BACKUP" "$README"
  if ! cmp -s "$README" "$BACKUP"; then
    echo "LADDER BUG ($name): restore not byte-identical"; fail=1
  fi
}

# M1 — drop the registry command everywhere (the original defect shape)
mutate_then_expect_red "registry-command-removed" \
  's/`npm install reeflog-client`//g; s/npm install reeflog-client[[:space:]]*#*.*$/ /g' \
  "! grep -qE 'npm (install|i) reeflog-client' '$README'"

# M2 — keep a buried registry mention but strip it from the Status paragraph
mutate_then_expect_red "status-paragraph-registry-removed" \
  '0,/`npm install reeflog-client`/s//`npm install gitpkg`/' \
  "! grep -qE 'npm (install|i) reeflog-client' <(grep '\*\*Status:' '$README')"

# M3 — delete the git-tag channel (the fix must ADD, not replace)
mutate_then_expect_red "git-tag-path-deleted" \
  's#npm install github:auroraxo/reeflog#v0.7.20##g' \
  "! grep -qE 'npm (install|i) github:auroraxo/reeflog#v[0-9]+\.[0-9]+\.[0-9]+' '$README'"

# M4 — insert a publication denial (the signerd v0.2.18 sentence)
mutate_then_expect_red "publication-denial-inserted" \
  '0,/^# reeflog$/s//# reeflog\n\nNo package is published to npm yet.\n/' \
  "grep -q 'No package is published to npm yet' '$README'"

if cmp -s "$README" "$BACKUP"; then
  echo "TREE BYTE-IDENTICAL"
else
  echo "TREE DIRTY — restore failed"; fail=1
fi
trap - EXIT
rm -f "$BACKUP"
[ "$fail" -eq 0 ] && echo "LADDER 4/4 CAUGHT, control green, tree identical" || echo "LADDER FAILED"
exit "$fail"
