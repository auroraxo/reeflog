# reeflog v0.1.0 — release report (2026-11-01)

## What shipped

- Tag `v0.1.0` → commit `af40ee5` (main). GitHub release with
  `reeflog-linux-amd64.tar.gz` and `reeflog-macos-arm64.tar.gz`.
- Install path for anyone, no checkout needed:
  `cargo install --git https://github.com/auroraxo/reeflog --tag v0.1.0`
- `--version` answers `reeflog 0.1.0` (before this release the binary had no
  version flag at all; the tag was the only version statement and `usage()`
  printed "reeflog v0" in prose).
- CI (`ci.yml`) now also runs on `v*` tags; `release.yml` builds per-platform
  binaries, refuses to publish when the tag name and the binary's own
  `--version` disagree, and attaches the archives.

## The release guard held — both gates fired on the broken intermediate state

The ladder run for the new flag was meant to build the OLD binary in a
scratch worktree; `git worktree add` had failed, so the "old tree" was the
real tree and commit `a7af319` shipped the README install claim WITHOUT the
flag. Two independent gates refused it, and no broken release went out:

- CI on main, run `41341640627`: `version_flag_prints_the_crate_version` and
  `version_flag_needs_no_arguments_and_no_database` both FAILED (the binary
  exited 64 with usage text).
- Release workflow on the first tag push, run `41341703680`:
  `tag v0.1.0 does not match binary version ` — no assets published,
  `gh release list` stayed empty until the fixed tag.

Fix `af40ee5` restores the flag; CI run `41342130779` and Release run
`41342287805` are green.

## Stranger proof: install from the tag, then real mainnet ingest

Fresh empty directory (`<tmp>/reeflog-stranger`, throwaway; no checkout, no
reeflog sources, no PATH entry):

```bash
cargo install --git https://github.com/auroraxo/reeflog --tag v0.1.0 --root ./install
./install/bin/reeflog --version        # reeflog 0.1.0
./install/bin/reeflog init --db ./demo.db --program whirLb…uctyCc
./install/bin/reeflog start --db ./demo.db --seconds 20
./install/bin/reeflog seal  --db ./demo.db
./install/bin/reeflog status --db ./demo.db
```

Result: 535 events journaled live from mainnet-beta (Whirlpool), 379
finalized, `gaps_open: 0`, `max_slot 464211843`, journal file ~1.4 MB. The
tag-installed binary is the same code path every future consumer uses; the
two existing consumers (whirlpool-watcher, sememe) run a checkout build of
the same source.

## Limits

- Still user zero, not user one: both consumers are my own repositories.
  This release removes the *install* blocker for a third party; adoption is
  the distribution card's work, not this artifact's.
- The release workflow builds macOS from `macos-latest` (arm64). The linux
  build is the one exercised end to end here; the macOS archive is untested
  beyond CI's tag/version check.
- The stranger run consumed the public rate-limited mainnet endpoint, as the
  README's *Free and paid* section assumes.
