#!/usr/bin/env bash
# Mutation ladder for the webhook CONFIG value domain.
#
# The ladder exists because a green test suite proves nothing on its own: it
# proves the tests pass, not that they would FAIL if the fix were removed.
# Each mutation removes or weakens exactly one part of the fix, the ladder
# must go red, and the tree must be restored byte-identically afterwards.
#
# The ladder:
#   M1  the timeout ceiling check is gone      -> timeout_ms above the
#                                                  ceiling is accepted again
#   M2  the "0 is not a timeout" check is gone -> 0 is silently re-read as
#                                                  1 ms by the poster
#   M3  the value is no longer named in the
#       refusal                                  -> the operator cannot see
#                                                  WHICH value was wrong
#   M4  the dead_letter_after domain check is
#       gone                                     -> a negative count is
#                                                  accepted
#   M5  the whole timeout check is gone         -> every timeout value is
#                                                  accepted, including the
#                                                  one that hangs the pass
#
# Usage: tools/mutation-ladder-webhook-config.sh
# Exit: 0 all mutations were caught and the tree was restored, 1 a mutation
#       slipped through or the tree was not restored.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
SRC="src/webhook.rs"
SUITE="cargo test --test webhook_config_domain --release --locked"

BEFORE=$(sha256sum "$SRC" | cut -d' ' -f1)
echo "ladder: tree $SRC $BEFORE"

restore() {
  if [ -f /tmp/reeflog-webhook-config-mutation.bak ]; then
    cp /tmp/reeflog-webhook-config-mutation.bak "$SRC"
    rm -f /tmp/reeflog-webhook-config-mutation.bak
  fi
}
trap restore EXIT

mutate() {
  cp "$SRC" /tmp/reeflog-webhook-config-mutation.bak
  python3 - "$1" <<'PY'
import sys
mutation = sys.argv[1]
path = "src/webhook.rs"
c = open(path, encoding="utf-8").read()

if mutation == "M1":  # drop the ceiling check
    old = """                if n > MAX_TIMEOUT_MS {
                    return Err(format!(
                        "target #{i}: timeout_ms must not exceed {MAX_TIMEOUT_MS} ms (1 hour), got {n} — out of domain"
                    ));
                }
"""
    assert old in c, "M1 anchor not found"
    c = c.replace(old, "", 1)
elif mutation == "M2":  # drop the zero check
    old = """                if n == 0 {
                    return Err(format!(
                        "target #{i}: timeout_ms must be greater than zero, got 0 — a timeout of 0 would fail every delivery immediately"
                    ));
                }
"""
    assert old in c, "M2 anchor not found"
    c = c.replace(old, "", 1)
elif mutation == "M3":  # stop naming the value
    old = (
        '                        "target #{i}: timeout_ms must be a positive '
        'integer in milliseconds, got {} (e.g. 5000)",\n'
        '                        format_value(v)'
    )
    new = (
        '                        "target #{i}: timeout_ms must be a positive '
        'integer in milliseconds, got a value (e.g. 5000)"'
    )
    assert old in c, "M3 anchor not found"
    c = c.replace(old, new, 1)
elif mutation == "M4":  # drop the dead_letter_after domain check
    old = """                if n < 0 {
                    return Err(format!(
                        "target #{i}: dead_letter_after must not be negative, got {n}"
                    ));
                }
"""
    assert old in c, "M4 anchor not found"
    c = c.replace(old, "", 1)
elif mutation == "M5":  # drop the whole timeout value check
    old = """            Some(v) => {
                let n = v.as_u64().ok_or_else(|| {
                    format!(
                        "target #{i}: timeout_ms must be a positive integer in milliseconds, got {} (e.g. 5000)",
                        format_value(v)
                    )
                })?;"""
    new = """            Some(v) => {
                let n = v.as_u64().unwrap_or(DEFAULT_TIMEOUT_MS);"""
    assert old in c, "M5 anchor not found"
    c = c.replace(old, new, 1)
else:
    raise SystemExit(f"unknown mutation {mutation}")

open(path, "w", encoding="utf-8").write(c)
PY
}

caught=0
total=0
for m in M1 M2 M3 M4 M5; do
  total=$((total + 1))
  mutate "$m"
  if $SUITE > /tmp/reeflog-mutation-$m.log 2>&1; then
    echo "MUTATION $m SURVIVED — the ladder does not see it (log: /tmp/reeflog-mutation-$m.log)"
  else
    echo "mutation $m caught"
    caught=$((caught + 1))
  fi
  restore
done

AFTER=$(sha256sum "$SRC" | cut -d' ' -f1)
if [ "$BEFORE" != "$AFTER" ]; then
  echo "TREE NOT RESTORED ($BEFORE -> $AFTER)"
  exit 1
fi
echo "ladder: $caught/$total mutations caught, tree restored byte-identically"
[ "$caught" -eq "$total" ]
