#!/usr/bin/env bash
# Mutation ladder for the webhook CONFIG URL domain.
#
# Proves that the test suite fails if any piece of the URL domain rule is
# removed or weakened.
#
# Mutations:
#   M1  drop the host requirement (authority.is_empty -> Ok)
#       -> `http://` accepted again, dies in transport
#   M2  make the scheme check case-sensitive
#       -> `HTTP://…` refused again, breaks consumer
#   M3  drop the scheme check entirely (any scheme accepted)
#       -> `ftp://…` accepted
#   M4  stop naming the value in the refusal
#       -> operator cannot see what was wrong
#   M5  revert webhook.rs back to the prefix check
#       -> both defects return
#
# Usage: tools/mutation-ladder-webhook-url.sh
# Exit: 0 all mutations caught and tree restored, 1 otherwise.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
ROOT=$(pwd)
SRC_FLAGS="src/flags.rs"
SRC_WEBHOOK="src/webhook.rs"
SUITE="cargo test --test webhook_url_domain --release --locked"

BEFORE_FLAGS=$(sha256sum "$SRC_FLAGS" | cut -d' ' -f1)
BEFORE_WEBHOOK=$(sha256sum "$SRC_WEBHOOK" | cut -d' ' -f1)
echo "ladder: tree $SRC_FLAGS $BEFORE_FLAGS / $SRC_WEBHOOK $BEFORE_WEBHOOK"

restore() {
  if [ -f /tmp/reeflog-whurl-flags.bak ]; then
    cp /tmp/reeflog-whurl-flags.bak "$SRC_FLAGS"
    touch "$SRC_FLAGS"
    rm -f /tmp/reeflog-whurl-flags.bak
  fi
  if [ -f /tmp/reeflog-whurl-webhook.bak ]; then
    cp /tmp/reeflog-whurl-webhook.bak "$SRC_WEBHOOK"
    touch "$SRC_WEBHOOK"
    rm -f /tmp/reeflog-whurl-webhook.bak
  fi
}
trap restore EXIT

mutate() {
  cp "$SRC_FLAGS" /tmp/reeflog-whurl-flags.bak
  cp "$SRC_WEBHOOK" /tmp/reeflog-whurl-webhook.bak
  python3 - "$1" <<'PY'
import sys
mutation = sys.argv[1]

if mutation == "M1":  # drop host check
    path = "src/flags.rs"
    c = open(path, encoding="utf-8").read()
    old = """    if authority.is_empty() {
        return Err(format!(
            "{label} expects a URL with a host — got `{value}` (e.g. {example})"
        ));
    }"""
    assert old in c, "M1 anchor not found"
    c = c.replace(old, "    // M1: host check dropped", 1)
    open(path, "w", encoding="utf-8").write(c)

elif mutation == "M2":  # scheme case-sensitive
    path = "src/flags.rs"
    c = open(path, encoding="utf-8").read()
    old = "if !schemes.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {"
    new = "if !schemes.iter().any(|s| *s == scheme) {"
    assert old in c, "M2 anchor not found"
    c = c.replace(old, new, 1)
    open(path, "w", encoding="utf-8").write(c)

elif mutation == "M3":  # drop scheme check
    path = "src/flags.rs"
    c = open(path, encoding="utf-8").read()
    old = """    if !schemes.iter().any(|s| s.eq_ignore_ascii_case(scheme)) {
        return Err(format!(
            "{label} expects a {} URL — got `{value}` (scheme `{scheme}` is not spoken by this consumer; e.g. {example})",
            schemes.join(" or ")
        ));
    }"""
    assert old in c, "M3 anchor not found"
    c = c.replace(old, "    // M3: scheme check dropped", 1)
    open(path, "w", encoding="utf-8").write(c)

elif mutation == "M4":  # stop naming value in host refusal
    path = "src/flags.rs"
    c = open(path, encoding="utf-8").read()
    old = '"{label} expects a URL with a host — got `{value}` (e.g. {example})"'
    new = '"{label} expects a URL with a host (e.g. {example})"'
    assert old in c, "M4 anchor not found"
    c = c.replace(old, new, 1)
    open(path, "w", encoding="utf-8").write(c)

elif mutation == "M5":  # revert webhook to prefix check
    path = "src/webhook.rs"
    c = open(path, encoding="utf-8").read()
    old = """        reeflog::flags::check_url_domain(
            &format!("target #{i} url"),
            url,
            &["http", "https"],
            "http://127.0.0.1:8080/hook",
        )?;"""
    new = """        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!("target #{i}: \\\"url\\\" must be http:// or https://"));
        }"""
    assert old in c, "M5 anchor not found"
    c = c.replace(old, new, 1)
    open(path, "w", encoding="utf-8").write(c)

else:
    raise SystemExit(f"unknown mutation {mutation}")
PY
}

caught=0
total=0
for m in M1 M2 M3 M4 M5; do
  total=$((total + 1))
  mutate "$m"
  if $SUITE > /tmp/reeflog-whurl-mutation-$m.log 2>&1; then
    echo "MUTATION $m SURVIVED — the ladder does not see it (log: /tmp/reeflog-whurl-mutation-$m.log)"
  else
    echo "mutation $m caught"
    caught=$((caught + 1))
  fi
  restore
done

AFTER_FLAGS=$(sha256sum "$SRC_FLAGS" | cut -d' ' -f1)
AFTER_WEBHOOK=$(sha256sum "$SRC_WEBHOOK" | cut -d' ' -f1)
if [ "$BEFORE_FLAGS" != "$AFTER_FLAGS" ] || [ "$BEFORE_WEBHOOK" != "$AFTER_WEBHOOK" ]; then
  echo "TREE NOT RESTORED"
  echo "  flags: $BEFORE_FLAGS -> $AFTER_FLAGS"
  echo "  webhook: $BEFORE_WEBHOOK -> $AFTER_WEBHOOK"
  exit 1
fi
echo "ladder: $caught/$total mutations caught, tree restored byte-identically"
[ "$caught" -eq "$total" ]
