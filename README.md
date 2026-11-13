# reeflog

**A durable local mailbox for Solana events.** A daemon subscribes to the
programs you pick, journals every log (and what it did to account balances)
into an append-only SQLite log with one monotone `seq`, and lets a consumer
resume *exactly where it stopped* — after a crash, a disconnect, or a reorg.

Solana's `logsSubscribe` is a live firehose: no delivery guarantee, no
durable cursor, no catch-up. Every project rebuilds reconnect, dedup,
gap-fill, and reorg handling, or silently loses events. reeflog makes that
one daemon, one file, one cursor.

**Status: v0.7.25 (2026-11-13), live against mainnet-beta — no Rust toolchain needed: `npm install reeflog-client`, then `import { bootstrap } from 'reeflog-client'; await bootstrap()` (downloads the prebuilt release archive and verifies its sha256 against what the GitHub release API publishes). The client itself is zero-dependency: one package, no `node_modules` of its own, and it installs without `git` on the machine.**

The same client from the tagged release:
`npm install github:auroraxo/reeflog#v0.7.25`. Or from source:
`cargo install --git https://github.com/auroraxo/reeflog --tag v0.7.25`.

**Honest state:** it works, and it is measured — but so far every real
consumer belongs to the project itself. There is no unaffiliated third-party
user yet, and this README calls that what it is. The full measurement history
lives in [docs/measured-history.md](https://github.com/auroraxo/reeflog/blob/main/docs/measured-history.md).

## Who this is for

- You run a bot, indexer, or watcher against specific Solana programs and
  cannot afford to silently miss a fill — or must know a fill was retracted.
- You are tired of rebuilding reconnect + gap-fill + dedup per project.
- You want the event record on *your* disk: a portable SQLite file you can
  verify (`reeflog verify`) instead of trusting a hosted stream.

Not for you, if: you need pre-event account state (the envelope holds
balance *movements* and fetch-time bytes, not the bytes as of the event —
no data source on public RPC serves that), or a managed hosted feed
(none exists; see *Free and paid*).

## Quickstart (verified run, ~1 minute)

Everything below was executed against `api.mainnet-beta.solana.com` on
2026-11-13 with the published v0.7.24 binary; your numbers will differ with
chain traffic.

```bash
npm install reeflog-client
npx --no-install node -e "
import('reeflog-client').then(async (m) => {
  const { bin } = await m.bootstrap();   // verified prebuilt binary
  console.log(bin);
});"
# the printed path is the binary; use it as BIN below

BIN=~/.reeflog/reeflog

$BIN init   --db reeflog.db --program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
$BIN start  --db reeflog.db --seconds 60     # ingest over WebSocket
$BIN seal   --db reeflog.db                  # promote confirmed -> finalized
$BIN tail   --db reeflog.db --cursor ./c.txt --limit 5   # resume from cursor
$BIN verify --db reeflog.db                  # find holes (exit 3 if unfinished)
$BIN status --db reeflog.db                  # journal health as JSON
```

The same commands as the installed CLI (on `PATH` after `cargo install`):

```bash
reeflog init  --db reeflog.db --program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
reeflog start --db reeflog.db --seconds 60      # ingest over WS
reeflog seal  --db reeflog.db                   # promote confirmed -> finalized
reeflog tail  --db reeflog.db --cursor ./c.txt  # resume from a cursor
reeflog verify --db reeflog.db                  # find holes
reeflog status --db reeflog.db
```

From Node, the client mirrors the CLI's exit-code contract (0 delivered,
3 held/incomplete, 1 refusal) instead of making you re-derive it:

```js
import reeflog, { bootstrap } from 'reeflog-client';

const { bin } = await bootstrap();       // verified prebuilt binary; bin is its path
const box = reeflog({ db: 'reeflog.db', bin });

// `init` is a CLI-only first-touch step; raw() is the documented escape hatch:
const { exitCode } = await box.raw(['init', '--db', 'reeflog.db',
  '--program', 'whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc']);

// (after `reeflog start` has ingested — the daemon, not the client, talks to the chain)
const { events, held, cursor } = await box.tail({ cursor: './c.txt', limit: 100 });
```

The typed client also carries `status()`, `verify()`, `repair()`, `hydrate()`,
and `reorgs()`; an incomplete verify/repair pass throws instead of returning
`complete: false`, and a `tail` held at an owed row comes back with `held`
named, cursor not advanced past it.

## What a day with reeflog looks like

```bash
reeflog init    --db reeflog.db --program <pubkey>
reeflog start   --db reeflog.db --seconds 60 [--repair-every 30] \
                                      [--repair-lookback 1800] [--repair-pages 40] \
                                      [--payload-limit 500]
reeflog seal    --db reeflog.db
reeflog verify  --db reeflog.db --pages 400 [--anchor-slot <slot>]   # detect, exit 3 if unfinished
reeflog repair  --db reeflog.db --pages 400 [--anchor-slot <slot>]   # fill signatures verify found
reeflog hydrate --db reeflog.db [--limit 500]      # fetch missing event content
reeflog hydrate --db reeflog.db --accounts         # + account envelopes for socket rows
reeflog hydrate --db reeflog.db --owners           # + owning program per address (batched)
reeflog tail    --db reeflog.db --cursor ./c.txt   # deliver events (content included)
reeflog webhook push --db reeflog.db --config webhooks.json   # deliver to outside targets
reeflog status  --db reeflog.db
```

Three kinds of gap need different tools, and reeflog keeps them apart
instead of calling everything "reconnected":

- **Tail gap** (crash/disconnect at the tip): closed automatically on `start`.
- **Rolling-window hole** (dropped rows in recent history): repaired on a
  schedule by `start`, verifiable by `verify` / `repair`.
- **Archive hole** (deeper than one tip-walk reaches): `--anchor-slot`
  anchors the walk at a finalized block, proven to 24 h depth live.

A scan that was cut short reports `truncated: true`, leaves the gap open,
and exits non-zero. A partially filled gap is never presented as filled.

Reorgs reach the consumer too:

```bash
reeflog reorg check --db reeflog.db --limit 200   # detect + record
reeflog reorg tail  --db reeflog.db --cursor ./reorgs.txt
```

And delivery can leave the machine entirely — signed webhooks with a
dead-letter queue:

```bash
reeflog webhook push   --db reeflog.db --config webhooks.json   # one pass
reeflog webhook status --db reeflog.db --config webhooks.json   # cursors + failures
reeflog webhook dlq list   --db reeflog.db --config webhooks.json  # quarantined events
reeflog webhook dlq replay --db reeflog.db --config webhooks.json  # hand them over
reeflog start --db reeflog.db --webhook-config webhooks.json --webhook-every 5
```

## Measured, not asserted

What a stranger's empty directory did with the published binary (full
detail and every raw number in
[docs/measured-history.md](https://github.com/auroraxo/reeflog/blob/main/docs/measured-history.md)):

| Claim | Evidence |
|---|---|
| Live ingest | 3122 events in 75 s on mainnet, 0 disconnects (2026-10-27) |
| Coverage | 224/226 chain signatures in the claimed window = 99.12 % |
| Crash survival | 600 s kill test (SIGKILL every 60 s): the first run **falsified** the claim; the fixed version survived the same run |
| Gap repair | 333 planted holes: verify saw 333, repair filled 333, second verify complete (live mainnet) |
| Archive depth | 39 holes at ~24 h depth filled and verified anchored |
| Retractions | proven against a **real fork of a real chain**, 47/47 checks |
| Refusals | every documented CLI line runs against the binary in CI (`tests/documented_surface.rs`); unknown flags are refused, never ignored |

## Conditions and limits — the honest part

- **No third-party user yet.** Two out-of-tree consumers exist
  ([whirlpool-watcher](https://github.com/auroraxo/whirlpool-watcher),
  [sememe](https://github.com/auroraxo/sememe)). Adoption is
  instrumented (`tools/measure-adoption.mjs`), not assumed.
- **Finality is the boundary of the promise.** reeflog seals events to the
  chain's own finalized state and reports reorgs; it cannot make
  confirmed-but-reverted history look trustworthy — it marks and delivers
  the retraction instead.
- **Archive availability is the RPC's, not reeflog's.** Deep gap-fill works
  while the endpoint serves `getSignaturesForAddress` history; a public
  endpoint is rate-limited, so provider independence has to be real rather
  than assumed.
- **No pre-event account state.** The envelope carries logs, balance
  movements, instructions, the account list with owners, and fetch-time
  bytes. The bytes *as of the event* are not in it, because no public-RPC
  data source can serve them — measured, two free routes are dead
  (`research/pre-event-bytes-probe-2026-11-02.json`).
- **One transport.** Public WebSocket `logsSubscribe` only; Yellowstone as a
  second transport does not ship.
- **A consumer must read the exit code.** `tail` exits 3 when a row's
  payload is still owed and holds the cursor there; treating every
  non-zero as "retry everything" replays forever. `exit 1` is a refusal.
- **The client is zero-dependency since v0.7.25.** Until then the npm
  manifest named *itself* as a dependency on github:, so every
  `npm install reeflog-client` also cloned a second, older copy of the
  client over git+ssh — and failed outright on a host without `git`
  (`npm error code ENOENT / syscall spawn git`). Measured on the
  published 0.7.24 bytes; fixed and re-measured in
  [`research/npm-self-dependency-fixed-2026-11-13.json`](research/npm-self-dependency-fixed-2026-11-13.json).
  Older tags keep the defect: they are immutable.

## Deep dives

- [docs/measured-history.md](https://github.com/auroraxo/reeflog/blob/main/docs/measured-history.md) — the complete
  README measurement and contract history through v0.7.25, unbroken.
- [`research/`](research/) — every committed measurement artifact behind the
  numbers above (raw JSON reports, pinned by tests).
- [`tests/`](tests/) and [`test/`](test/) — the contract tests: the CLI
  surface the README documents is run against the real binary, and the
  Node client's exit-code mapping is pinned offline.
- [Releases](https://github.com/auroraxo/reeflog/releases) — per-platform
  archives with sha256; the release workflow refuses to publish a tag the
  binary disagrees with.
- [Issues](https://github.com/auroraxo/reeflog/issues) — including issue
  #1, where a consumer retracted one of this project's own claims.

## Free and paid

Free, MIT: daemon, journal, CLI, Node client, webhook fan-out, dead-letter
queue, archive backfill. No event or cursor limits. Paid later (not today):
managed always-on feeds and replicated journals. Events are never sold
individually.

## Development

```bash
cargo build --release          # Rust daemon + CLI
cargo test --release           # CLI/journal contract tests
npm install && npm test        # Node client contract tests (offline)
node tools/measure-documented-surface.mjs   # README lines vs. the binary
```

The `tools/` directory holds the harnesses behind every number above
(`killtest.mjs`, `e2e-*.mjs`, `measure-*.mjs`); their reports are committed
under `research/` and pinned by the measurement tests.

## License

MIT — see [LICENSE](LICENSE).
