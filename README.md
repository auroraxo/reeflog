# reeflog

**A durable local mailbox for Solana events.** A daemon subscribes to the
programs you pick, journals every log (and what it did to account balances)
into an append-only SQLite log with one monotone `seq`, and lets a consumer
resume *exactly where it stopped* — after a crash, a disconnect, or a reorg.

Solana's `logsSubscribe` is a live firehose: no delivery guarantee, no
durable cursor, no catch-up. Every project rebuilds reconnect, dedup,
gap-fill, and reorg handling, or silently loses events. reeflog makes that
one daemon, one file, one cursor.

**Status: v0.7.39 (2026-11-19), live against mainnet-beta — no Rust toolchain needed: `npm install reeflog-client`, then `import { bootstrap } from 'reeflog-client'; await bootstrap()` (downloads the prebuilt release archive and verifies its sha256 against what the GitHub release API publishes). The client itself is zero-dependency: one package, no `node_modules` of its own, and it installs without `git` on the machine.**

The same client from the tagged release:
`npm install github:auroraxo/reeflog#v0.7.39`. Or from source:
`cargo install --git https://github.com/auroraxo/reeflog --tag v0.7.39`.

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
2026-11-13 with the published v0.7.25 binary; your numbers will differ with
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
$BIN status --db reeflog.db                  # journal health as JSON (exit 3 if a payload is owed)
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
reeflog status  --db reeflog.db              # exit 3 if a row still owes its payload
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
| Gap ledger | one hole = one episode: the published v0.7.26 asset counted scans (one hole → two open rows, `gaps_open: 1` forever on a healed journal); measured on the published asset, fixed in v0.7.27 |
| Retraction status | the status report named only the event queue: a target with a drained event queue and 2 never-received retractions read as caught up; measured on the published v0.7.27 asset, fixed in v0.7.28 |
| Orphaned dead letters | the status report named only the configured targets: quarantined events of a target that left the config read as "nothing quarantined" while `push` counted them; measured on the published v0.7.28 asset, fixed in v0.7.29 |
| Debt exit code | the report printed two debts (`dead_letters: 1`, `last_error`) and exited 0 while `webhook push` on the same journal exited 3 for both; status and push disagreed on one fact; measured on the published v0.7.29 asset, fixed in v0.7.30 |
| Payload debt exit code | `status` printed `payload_pending: 1` and exited 0 while the payload-bearing `tail` held at the same row and exited 3; the health report a monitoring caller polls called an owed journal healthy; measured on the published v0.7.30 asset, fixed in v0.7.31 |
| Replay body | `dlq replay` re-sent the body frozen into the quarantine: after the chain unmade the transaction, `push` delivered `seal: "reverted"` and the replay delivered `seal: "finalized"` for the same seq; measured on the published v0.7.31 asset, fixed in v0.7.32 |
| Retraction block | a refused retraction was reported as a phantom event position (`at seq None`) on the event channel, the operator's line leaked Rust debug formatting, and the retraction's reason shared the event channel's `blocked_error` — the operator could not tell which queue was stuck; measured on the published v0.7.32 asset, fixed in v0.7.33 |
| Fan-out isolation | ONE target whose delivery state is bound to another endpoint aborted the whole webhook command: `push`/`status`/`dlq replay` exited 1 with NO report, the healthy sink starved (3 of 5 bodies), the reports a monitoring caller polls went dark; measured on the published v0.7.33 asset, fixed in v0.7.34 |
| Disconnect episodes | ONE outage left one OPEN gap row per refused connect attempt and healed only the last: a journal whose stream had returned reported `gaps_open: 6` forever, and every nightly network switch re-opened the alarm; measured on the published v0.7.34 asset, fixed in v0.7.35 |
| Webhook redirect | the poster followed HTTP redirects: a redirecting endpoint recorded every event as delivered (attempt status = the hop's own 200) while the configured consumer received ZERO event bodies and a bodyless GET carried the signature header to a host the config never named — the cursor advanced past the event, so it could never be re-delivered; measured on the published v0.7.35 asset, fixed in v0.7.36 |
| Two-queue reason | the two delivery queues kept ONE standing reason: a blocked event queue written after a blocked retraction queue OVERWROTE it, so `webhook status` showed the retraction's 404 where the event queue's repair (`hydrate`) belonged — the report a monitoring caller polls named the wrong queue's repair; measured on the published v0.7.36 asset, fixed in v0.7.37 |
| Zero cadence | the four cadence flags (`--seal-every`/`--reorg-every`/`--repair-every`/`--webhook-every`) turned a documented 0 into ONE SECOND: every pass ran at 1 Hz while the SAME run's shutdown gates read the same zero as "never" — and `--webhook-every 0` delivered signed bodies at 1 Hz; measured on the published v0.7.37 asset, fixed in v0.7.38 |
| Retraction refusal voice | the retraction queue formatted the refusal reason by hand instead of consulting the one mint: with an endpoint answering 302, the event queue said `refused with http 302 redirect — … repoint this target's url explicitly …` while the RETRACTION queue's four surfaces (the attempt record, the report's `reorg_error`, `webhook status`, the blocked line) all read the flat `refused with http 302` — the queue whose lateness costs money read a bare status with no repair; measured on the published v0.7.38 asset, fixed in v0.7.39 |
| Field use | the same cursor rule answered an open question in someone else's product: a LaserStream SDK issue asked whether reconnect `from_slot` can skip undelivered data — measured with a runnable mock (`379a6ba`, three variants + control): ticks at live head advance the SDK's resume cursor past payloads still being replayed, whatever their commitment status ([laserstream-sdk#115](https://github.com/helius-labs/laserstream-sdk/issues/115#issuecomment-7016745189), harness [auroraxo/laserstream-cursor-measure](https://github.com/auroraxo/laserstream-cursor-measure)) — the skip becomes a visible, backfillable gap row here (2026-11-19); the follow-up two-rule matrix measured the SDK maintainer's proposed fix (PR #114's `replay_cursor_slot`) against a compliant server: it closes the internal-tick face and the block-footer face, but the two faces the issue reports — a USER slot update at live head, with or without `filter_by_commitment` — still advance the cursor to 122 and lose slots 92..121 (per-cell artifacts + `research/laserstream-115-pr114-two-rule-matrix-2026-11-19.json`, pinned by `tests/laserstream_115_pr114_matrix.rs`) |

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
  Since v0.7.31 `status` holds that code too: a journal that still owes a
  row's content makes the health report exit 3, so the caller that polls it
  cannot read an owed journal as healthy.
- **A `--limit` is a budget, not a hint, and the push report is the queue.**
  `webhook push --limit 0` sends zero bytes and moves no cursor; a bounded
  pass reports `remaining_behind_cursor` and `truncated` so a page can never
  read as a drained mailbox (measured on the published v0.7.25 asset, fixed
  in v0.7.26 — see the deep dive).
- **One hole is one episode, and a heal closes it.** The gap ledger used to
  count SCANS: every `verify` over the same hole opened another `hole` row,
  and `repair` closed only the oldest one — so a journal the chain called
  whole still reported `gaps_open: 1`, forever. `gaps_open` is the field
  every operator and monitoring caller reads to decide whether the mailbox
  is whole; a permanent false alarm there trains the reader to ignore it.
  Measured on the published v0.7.26 asset, fixed in v0.7.27 (see the deep
  dive).
- **The retraction queue is a debt of its own, and the status report names
  it.** The reorg channel has its own cursor (`next_reorg_id`) on purpose:
  a retraction must still reach a consumer whose event queue is blocked.
  `webhook status` reported only the EVENT queue (`pending_events`), so a
  target whose event queue was drained read as "caught up" while the events
  the chain UNMADE had never reached it — on the same journal state
  `webhook push` named `remaining_reorgs_behind_cursor`. The status report
  now carries `pending_retractions` next to `pending_events`, counted from
  the journal, so neither queue can stand in for the other. Measured on the
  published v0.7.27 asset, fixed in v0.7.28 (see the deep dive).
- **The stranded dead letter belongs to a target, and the status report
  names it.** A pending dead letter belongs to a TARGET, not to a config
  entry: when a target is removed from the config, its quarantined events
  stay in the journal — and they can never be replayed from here, because
  the replay path needs that target's endpoint. `webhook push` reported this
  as `orphaned_dead_letters` and `webhook dlq list` listed the orphan with
  `in_config: false`, but `webhook status` walked the CONFIG's targets only,
  so the stranded letters were printed nowhere: the report said
  `dead_letters: 0` for the healthy target and exited 0 while the same
  journal read through `push` said `orphaned_dead_letters: 1`. The status
  report now carries `orphaned_dead_letters` and
  `orphaned_dead_letter_targets`, counted from the journal, and exits 3
  while a stranded letter is pending — this is the one debt that never
  drains by itself. Measured on the published v0.7.28 asset, fixed in
  v0.7.29 (see the deep dive).
- **The report that prints a debt holds the exit code.** `webhook status`
  is the surface a monitoring caller polls — it asks "is delivery healthy?"
  and reads the exit code. v0.7.29 gave that code to the ORPHANED dead
  letter on the stated ground that it is "the one debt that never drains by
  itself"; the premise does not separate the cases: a configured target's
  own quarantined events sit behind its cursor (`deliver_events` reads
  `events_after(next_seq)`), so `push` never re-attempts them either — only
  `dlq replay` hands them over, a human action. And the standing
  `last_error` verdict is written by the very blocked pass `push` reports
  as exit 3. On the published v0.7.29 bytes the same journal read
  `dead_letters: 1, exit 0` through `status` and `dead_letters_pending: 1,
  exit 3` through `push`. The report now carries `dead_letters_pending` and
  `failing_targets` (the same names `push` uses) and exits 3 for any debt it
  prints — configured quarantine, stranded orphan, or standing failure. A
  clean journal still exits 0. Measured on the published v0.7.29 asset,
  fixed in v0.7.30 (see the deep dive).
- **The health report holds the debt it prints, too (v0.7.31).** The same
  rule applies to the JOURNAL's own report: `status` printed
  `payload_pending: 1` and exited 0, while the default payload-bearing
  `tail` held at that exact row and exited 3 — the predicate is the same
  one (`logs IS NULL AND payload_state IS NULL`). `webhook push` has held
  its target cursor there since v0.7.23, `hydrate` exits 3 while rows are
  pending, and the daemon exits 3 at shutdown while the payload hydrate is
  incomplete; only `status` — the surface a monitoring caller polls — read
  the number as health. It now exits 3 and names `reeflog hydrate` on
  stderr, and the Node client's `status()` throws on that verdict with the
  report attached (`error.report`), so the facts are not lost with the
  verdict. Deliberately NOT part of the verdict: a recorded outcome
  (`unavailable`/`invalid` — the chain forgot the transaction, which
  `hydrate` counts as a completed pass) and the opt-in
  account/owner/account-data queues, which drain only behind
  `hydrate --accounts` / `tail --with-accounts` and would be a permanent
  alarm for a default consumer. Measured on the published v0.7.30 asset,
  fixed in v0.7.31 (see the deep dive).
- **The replay answers the journal, not the quarantine's snapshot
  (v0.7.32).** A dead letter stores the bytes that were owed at the moment
  the quarantine wrote them — a snapshot, not a fact. `webhook push` always
  rebuilt its body from the row, so the same seq could reach a consumer two
  different ways depending on which command delivered it: measured on the
  published v0.7.31 asset, the chain unmade the transaction between
  quarantine and replay, `push` delivered the retraction and
  `seal: "reverted"`, and `dlq replay` re-sent the frozen body claiming
  `seal: "finalized"` — the consumer's last word for that signature was a
  claim the chain had unmade. The replay now rebuilds the body from the
  journal row, so both surfaces send the same bytes for the same seq; an
  UNCHANGED letter still replays byte-for-byte, a letter with no journal
  row still replays its stored bytes (nothing is dropped), and the report
  names the rebuilt letters (`rebuilt`). Measured on the published v0.7.31
  asset, fixed in v0.7.32 (see the deep dive).
- **A blocked retraction is named as a retraction (v0.7.33).** The webhook
  fan-out has two queues per target — events (owed at a `seq`) and
  retractions (owed at a `reorg` id) — and a refused retraction was
  reported on the EVENT channel's line, at a position that did not exist
  (`at seq None`), with Rust debug formatting in the same line, and with
  the retraction's reason arriving on the event channel's `blocked_error`
  field. The operator could not tell which queue was stuck, and the queue
  that matters most (a late retraction costs money) was named least. The
  operator's line now names the retraction channel and the reorg id, the
  report carries `reorg_error` beside `error`, and the daemon prints the
  line too. A refused retraction is still never quarantined — that
  design rule is unchanged; this makes the block legible, not resolved.
  Measured on the published v0.7.32 asset, fixed in v0.7.33 (see the deep
  dive).
- **One refused target no longer silences the fan-out (v0.7.34).** The
  endpoint-binding refusal (give a repointed endpoint its OWN target id —
  v0.7.15) was raised at the config level: the first failing target aborted
  the whole command. `webhook push` and `webhook status` exited 1 with NO
  report, `webhook dlq replay` replayed nothing, and the healthy consumers
  behind the misconfigured one starved — the healthy sink got 3 of 5
  bodies. The refusal is now a fact about ONE consumer: it is a row of its
  own in every report (`refused: true`, the reason named), the other
  targets keep running, the refused target holds the exit code (3 =
  reported and held), and for it no byte moves and no cursor is read — the
  binding rule itself is untouched. Measured on the published v0.7.33
  asset ([blind](research/fanout-isolation-published-v0733-2026-11-17.json),
  5/8 faces, verdict `fanout-isolation-blind`) and re-measured on the
  published v0.7.34 asset
  ([held](research/fanout-isolation-published-v0734-2026-11-17.json), 8/8
  faces, verdict `fanout-isolation-held`).
- **One outage is ONE row, and the heal resolves it (v0.7.35).** The
  reconnect loop opened a fresh `connect_fail` gap row for EVERY refused
  connect attempt and closed only the last one on recovery. One episode
  with 6 refused attempts left 6 OPEN rows; the healed journal reported
  `gaps_open: 6` forever — the same permanent false alarm v0.7.27 removed
  from the hole rows — and a nightly network down-switch re-opened it
  every day. One episode is now ONE row per kind while the endpoint is
  down (the retries ride on the row's detail, so the outage stays loud),
  and the reconnect heal resolves the whole episode as history —
  `gaps_open: 0` when the stream is back, the fill count on one row so
  `SUM(backfill_count)` still counts fills. A truncated heal is not a
  heal: the episode stays open until a complete walk proves the claim.
  Measured on the published v0.7.34 asset
  ([7 defect faces](research/gap-episode-dedup-published-v0734-2026-11-18.json),
  verdict `gap-episode-rows-accumulate`) and re-measured on the published
  v0.7.35 asset
  ([held](research/gap-episode-dedup-published-v0735-2026-11-18.json), 8/8
  faces, verdict `gap-episode-dedup-held` — the fix is in the bytes a
  stranger installs).
- **A redirect is a refusal of THIS url, not a delivery (v0.7.36).** The
  webhook poster followed HTTP redirects: a redirecting endpoint recorded
  every event as delivered — the attempt status was the HOP's own 200 —
  while the configured consumer received zero event bodies, the cursor
  advanced past the event (so it could never be re-delivered), and a
  301/302/303 hop carried the signature header on a bodyless GET to a
  host the config never named. Delivery is bound to the url the config
  names: the redirect is now answered as a REFUSED verdict (its own 3xx
  status recorded), the hop is never followed, the refusal names the
  mechanism and the repair (repoint the target's url explicitly), and the
  target's queue holds the exit code (3). Measured on the published
  v0.7.35 asset
  ([5 defect faces](research/webhook-redirect-two-tree-2026-11-19.json),
  verdict `webhook-redirect-followed-and-recorded-delivered`) and
  re-measured on this tree
  ([held](research/webhook-redirect-two-tree-2026-11-19.json), 0 defect
  faces, verdict `webhook-redirect-refused-at-the-configured-endpoint`).
- **Each delivery queue keeps its own standing reason (v0.7.37).** The
  event queue and the retraction queue are independent — a retraction must
  still reach a consumer whose event queue is blocked, which is why `webhook
  push` reports them on two fields (`error` and `reorg_error`). The JOURNAL
  kept ONE standing reason per target: the retraction pass wrote the same
  column the event pass had filled, and the second write won. With both
  queues blocked at once, `webhook status` — the report a monitoring caller
  polls — showed the retraction's refusal where the event queue's repair
  (`hydrate`) belonged: the operator read a repair for the one queue whose
  stop was already visible and never learned the other queue owed a payload.
  Each channel's reason now lives in its own column (`last_error`,
  `last_reorg_error`), the status row carries the retraction's reason on
  `reorg_error` — the same name `push` uses — and the stderr lines name the
  channel they belong to. Measured on the published v0.7.36 asset
  ([3 defect faces](research/status-two-queue-reason-2026-11-19.json),
  verdict `status-two-queue-reason-one-field`) and re-measured on this tree
  (held, 0 defect faces).
- **A cadence of 0 means OFF, not every second (v0.7.38).** `reeflog
  start` takes four cadence flags (`--seal-every`, `--reorg-every`,
  `--repair-every`, `--webhook-every`), and the scheduler seeded its
  next-due instants as `now + max(cadence, 1)`: a documented 0 collapsed to
  ONE SECOND, every pass ran at 1 Hz — while the SAME run's shutdown gates
  read the same zero as "never" (`final_reorg: {skipped: true}`). One flag
  value, two contradictory meanings in one run, and the expensive one was
  the 1 Hz one: `--webhook-every 0` delivered signed bodies at 1 Hz. A
  cadence of 0 is now `None` as its next-due instant — never on a tick,
  never at a gate; positive cadences keep their old meaning (including the
  1 s floor). Measured on the published v0.7.37 asset
  ([5 defect faces](research/scheduler-zero-cadence-2026-11-19.json),
  verdict `scheduler-zero-cadence-busy`) and re-measured on this tree
  (held, 0 defect faces).
- **The retraction queue speaks the same refusal mint (v0.7.39).** The
  v0.7.36 rule made one mint the refusal voice (a 3xx names the redirect
  and the explicit repoint repair), but `deliver_reorgs` still formatted
  the reason by hand: with an endpoint answering 302, the event queue
  named the redirect and the repair while the RETRACTION queue's attempt
  record, report row, polled status and blocked line all read the flat
  `refused with http 302` — the one message whose lateness costs money
  read a bare status with no repair. The retraction queue now calls the
  same mint; a 4xx keeps the flat wording it always had, the
  classification does not move (blocked with its owed id, exit 3, the
  redirect never followed). Measured on the published v0.7.38 asset
  ([4 defect faces](research/reorg-refusal-voice-2026-11-19.json),
  verdict `reorg-refusal-voice-borrowed-flat-wording`) and re-measured on
  this tree (held, 0 defect faces).
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
  README measurement and contract history through v0.7.35, unbroken.
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
