# reeflog

A durable local mailbox for Solana events. A daemon journals logs, account changes, and transactions for selected programs into a cursor-addressed append-only log, so a consumer resumes without gaps after a crash, disconnect, or reorg.

**Status: v0 working (October 2026).** Live against mainnet-beta: see *Measured results* below.

## The idea

Solana event consumption is normally a live firehose. `logsSubscribe` has no delivery guarantee, no durable cursor, and no catch-up after disconnection. Every project rebuilds reconnect, deduplication, gap-fill, and reorg handling, or it silently loses events.

reeflog treats events as a mailbox instead of a phone call:

- **ingest:** subscribe to configured programs over WebSocket.
- **seal:** record events as confirmed, promote them when finalized, mark failures as errored.
- **gap-fill:** catch up on signatures the journal missed before resuming. A gap must be reported; silent loss is a protocol error.
- **journal:** append-only SQLite with one monotone `seq` and a portable file.
- **deliver:** a cursor-based CLI iterator that resumes exactly where it stopped.

## Shape

```bash
reeflog init  --db reeflog.db --program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
reeflog start --db reeflog.db --seconds 60      # ingest over WS
reeflog seal  --db reeflog.db                   # promote confirmed -> finalized
reeflog tail  --db reeflog.db --cursor ./c.txt  # resume from a cursor
reeflog verify --db reeflog.db                  # find holes
reeflog status --db reeflog.db
```

The planned core is a Rust daemon and a TypeScript client. Decoding stays pluggable; reeflog does not itself decide what bytes mean.

## Measured results

Checked live on 2026-10-27 against `api.mainnet-beta.solana.com`, program `whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc` (Whirlpool):

| Measurement | Result |
|---|---|
| WS ingest | 3122 events in 75 s, 0 disconnects |
| Coverage in the claimed slot window | 224 / 226 chain signatures = **99.12 %** |
| Tail gap after a restart | 9 / 9 deleted events refilled |
| Middle hole detection | 17 missing reported = 15 deleted + 2 baseline, exactly |
| Cursor resume | no replay, monotone advance, no overlap |
| Unit tests | 23 / 23 |

Kill test (600 s on Raydium AMM v4, SIGKILL every 60 s): see *The kill test*
below — the first run **falsified** the delivery claim; the fixes are now
re-verified in the **same 600 s configuration** that falsified it
(`full-after-fix`, verdict `contract_survived`).

## The kill test

`tools/killtest.mjs` is a falsifying harness, not a demo. It runs three
consumers against the same program for 600 s:

- **reeflog** — SIGKILLed every 60 s, restarted after 4 s of downtime.
- **naive-killed** — a bare `logsSubscribe` consumer, killed the same way.
- **naive-live** — a bare `logsSubscribe` consumer, **never killed**: the
  control that separates "the kill caused it" from "the endpoint drops events
  anyway".

It then fetches the chain's own signature list for the window independently
and holds it against every consumer's seen-set: a truth signature that *some*
consumer received on the log stream is deliverable, so missing it is a real
loss; a signature nobody on the stream received is classified with
`getTransaction` instead of assumed — only a transaction that actually
invokes the program (`invoked`) can count as a loss, never a
`referenced-only` one.

Run `sync` (2026-10-28, same harness, 120 s / 4 kills on Raydium AMM v4)
re-tested the claim and it **survived**: every check passed, verdict
`contract_survived` (exit 0).

| Check | Result |
|---|---|
| reeflog real losses | **0** of 1,112 deliverable events |
| naive-killed (control) real losses | **169** — the kill alone loses events |
| naive-live (control, never killed) real losses | **3** — the public WS drops events without any kill |
| verify before repair | 14 missing, `complete: true`, exit 0 |
| repair | `found_missing: 14`, `filled: 14`, `complete: true` |
| verify after repair | `missing_count: 0`, `complete: true`, exit 0 |
| dedup | 1,215 rows = 1,215 distinct signatures |
| cursor resume | second iteration: 0 rows (no replay) |
| incomplete scan | never exits 0 |

Run `full-after-fix` (2026-10-28, 600 s / 9 kills on Raydium AMM v4 — the
exact configuration that falsified the claim, re-run against the fixed
binary) **survived**: verdict `contract_survived`, exit 0, every product check
passed. This is the confirmation run the falsification owed.

| Check | Result |
|---|---|
| reeflog real losses | **0** of 5,374 deliverable events |
| naive-killed (control) real losses | **455** across 9 kill windows |
| naive-live (control, never killed) real losses | **4** — the public WS drops events without any kill |
| verify before repair | 1 missing, `complete: true`, exit 0 |
| repair | `found_missing: 1`, `filled: 1`, `complete: true` |
| verify after repair | `missing_count: 0`, `complete: true`, exit 0 |
| dedup | 5,636 rows = 5,636 distinct signatures |
| cursor resume | second iteration: 0 rows (no replay) |
| live repair during the run | 19 gaps reported, `gaps_open: 0`, 380 backfilled |

Run `full` (2026-10-28, the earlier 600 s run) **falsified** the same claim
and is kept here because it is what exposed the defects:

| Check | Result |
|---|---|
| reeflog lost deliverable events | **32 real losses** of 11,275 deliverable |
| naive-live lost deliverable events | 158 — the public WS drops events even with no kill |
| `reeflog verify` | reported `missing_count: 0`, `truncated: true`, **exit 0** |

Two defects, both measured against the RPC and fixed:

1. **Phantom anchor.** The journal's newest held signature was a
   confirmed-then-forked-out transaction. `getSignatureStatuses` returns
   `null` for it, and `before=<it>` makes `getSignaturesForAddress` answer
   `-32020 Transaction not found` for the whole walk. verify therefore read
   **zero pages** and printed `missing_count: 0` — a hole detector that goes
   blind exactly when holes exist. Every walk now starts at the chain tip and
   pages only with signatures the chain itself returned; the tail stop is on
   the boundary **slot**, not the boundary signature.
2. **Nothing repaired mid-window holes.** The tail backfill only reaches
   forward from the newest held slot. Events the socket dropped *while
   connected* sit behind that frontier — all 32 losses did — and nothing ever
   filled them. `start` now runs a bounded repair pass over a rolling window
   (default every 30 s, 1800 slots back) and once more at shutdown.

Both are covered by regression tests, and the honesty rule is explicit:
`Report::complete()` requires no error, no truncation, at least one page read,
**and** measured coverage of the claimed window (first row at or above the
ceiling, last row at or below the floor). An unfinished scan exits 3 and can
never be mistaken for a clean one.

## RPC semantics, measured

`getSignaturesForAddress` does not behave the way the parameter names suggest. Measured with an anchor at the 4th of 10 tip signatures:

| Parameter | Observed behaviour |
|---|---|
| `before=<sig>` | returns signatures **older** than the anchor — the backwards-paging primitive |
| `until=<sig>` | returns signatures **newer** than the anchor |
| `since=<sig>` | behaves like `until` — **newer** |
| `before` + `until` together | rejected with `-32020 Transaction not found` for an inverted window |
| `before=<phantom>` | `-32020 Transaction not found` — a held signature can be a transaction the address index no longer knows |
| `until=<phantom>` | same `-32020`; neither parameter tolerates a phantom anchor |
| `since=<slot>` | silently **ignored** by the public endpoint (a typo'd parameter returns the same rows), so a slot-bounded walk cannot be expressed at all |
| `getSignatureStatuses` | returns `null` for a phantom, a status for a real signature — the reliable way to tell them apart |
| throughput | 19 of 40 consecutive `limit=1000` pages answered HTTP 429, ~0.63 pages/s sustained |

Consequence: a backfill written with `since=<newest signature we hold>` returns an empty window and reports no loss while filling nothing. reeflog pages backwards with `before` and stops at the boundary **slot** instead, always starting from the chain tip so no held signature is ever an anchor. This was found by measuring the endpoint, not by reading the docs, and it is recorded in `src/backfill.rs`.

## Event content (payload hydration)

A signature alone is not an event — a mailbox that delivers "something
happened at slot N" without saying what happened forces every consumer back
onto the RPC, which is the fragility reeflog exists to remove.

The gap machinery (`backfill.rs`) recovers *signatures* from
`getSignaturesForAddress`, which carries slot and error but no `logs`.
Measured 2026-10-28 on a 60 s live run against mainnet: 13 rows filled by
`repair`, all 13 stored with `logs IS NULL`, while all 2,082 rows the socket
delivered held payload. That asymmetry meant the rows the journal had to
*recover* were exactly the rows a consumer could not read.

reeflog hydrates missing content (`src/payload.rs`) with `getTransaction` and
distinguishes four states:

- **`held`** — log lines stored as a JSON array. Both socket capture and
  hydration store into this state; hydration never overwrites captured logs.
- **`unavailable`** — the chain does not know the transaction (forked out or
  pruned). Keeps `logs = null` and records the reason; an empty array would
  claim "no logs", which is a different statement.
- **`invalid`** — the endpoint refuses the key (-32602). A corrupted row,
  recorded separately from a fork-out so an operator can tell them apart.
  Never folded into `unavailable`.
- **`pending`** — still unknown. Any pending row makes a pass incomplete;
  incomplete passes exit 3 (same honesty rule as `verify` and `repair`).

`reeflog tail` delivers the log array by default; `--no-logs` keeps the
compact identity-only shape. A background worker hydrates recovered rows
continuously, beside the ingest loop — not once at startup, because the
periodic repair pass keeps recovering rows while the daemon runs, and a
killed run never reaches a shutdown pass. A final pass runs at shutdown.
Verified live: `tools/e2e-payload.mjs` checks all 13 conditions against
mainnet, including independent byte-for-byte comparison of stored payloads
against freshly fetched transactions.

## First external consumer: a fill watcher

Until this section existed, every claim was checked by reeflog's own
harness. `tools/fill-watcher.mjs` is a consumer in the sense a bot cares
about: it reads `reeflog tail --cursor` and turns rows into **fills** —
signature, slot, direction, `amount_in`, `out_amount`, pool reserves — and it
does that from local bytes, with no RPC call of its own.

The decode is not guessed. Raydium AMM v4 emits `ray_log:` as a
bincode-serialized struct (`SwapBaseInLog` / `SwapBaseOutLog` in
`raydium-amm/program/src/log.rs`), so the field order is the program's own.
The wire layout was then verified against the chain: for every sampled swap,
`pool_coin + amount_in` equals the pool's coin-vault balance after the swap
and `pool_pc − out_amount` equals its pc-vault balance, both read from the
transaction's pre/post token balances. A line the decoder cannot read is
reported as `undecodable`, never guessed at.

`tools/e2e-fillwatch.mjs` runs this watcher against the alternative — the
classic `getSignaturesForAddress` + `getTransaction` poller — over the **same
window on the same endpoint**, and fails if:

1. `hydrate` leaves any row unreadable (`pending` must be 0, exit 0);
2. any fill the poller decoded **inside the daemon's slot window** is
   missing from reeflog's stream;
3. a fill both consumers saw decodes to **different** amounts (a payload that
   exists but disagrees with the chain is not delivery);
4. the watcher's own decoder drops a held leg;
5. the local decode phase makes any RPC call.

The comparison is **slot-aligned**, and the reason is worth stating: a naive
page fetch reaches back ~1000 signatures into history, while the daemon's
window starts when its WebSocket subscription delivers its first event. The
first run of this harness compared the raw sets and reported 21 "missing"
fills — every sampled one sat in a slot *before* the journal's first slot,
i.e. before the daemon had connected. That is not a loss, and treating it as
one would be exactly the kind of plausible-wrong-number this project refuses
to publish. Fills outside the window are now counted and reported
separately, never as losses.

Measured 2026-10-28, 60 s live window on mainnet, Raydium AMM v4:

| Measurement | reeflog | naive poller |
|---|---|---|
| fills decoded | **155** | 12 |
| RPC calls for that content | **0** | 44 (4 signature pages + 40 transactions) |
| in-window fills missing from reeflog | **0** of 7 | — |
| fills decoding to identical amounts | **7 / 7** | — |
| rows drained | 474 rows in 316 ms | — |

The naive consumer is budgeted on purpose: the public endpoint answers
roughly one `getTransaction` per second under load, so a poller that wants
content pays for it — every call above is counted, not estimated. reeflog
paid for its content once, at ingest, and the watcher then reads bytes it
already had.

`tools/test-decoder.mjs` is the offline half: three real mainnet fixtures
with every field asserted, the 57-byte rule enforced, a non-swap log type
refused, and a row without a swap proven not to become a fill. It runs in CI
and needs no network.

## Reorgs: a retraction a consumer can actually receive

A mailbox that delivers an event and then loses it is only half a mailbox.
The other half is saying *that event no longer stands* — and saying it in a
way a consumer can act on.

reeflog marks a reorg only when **two independent sources refuse the same
transaction**. That rule exists because of what a single source can do:

- `getSignatureStatuses` answers `null` for a signature it cannot answer for,
  and a transport hiccup looks exactly like a fork-out;
- a rate limit looks like silence;
- an endpoint that refuses the *key* (-32602) says nothing at all about the
  chain — that is a corrupted row, not a retracted event.

So the states stay apart, the way `hydrate` keeps `unavailable`, `invalid` and
`pending` apart:

| Both sources | Verdict | What happens |
|---|---|---|
| at least one serves the transaction | known | nothing changes |
| both refuse it | **reverted** | recorded in `reorgs`, event's seal becomes `reverted` |
| one refuses the key | invalid | no chain verdict, the pass stays complete |
| one fails | undecided | the pass exits 3 — a failure is not a verdict |

```bash
reeflog reorg check --db reeflog.db --limit 200   # detect + record
reeflog reorg tail  --db reeflog.db --cursor ./reorgs.txt
```

**The check runs by itself.** `reeflog start` seals pending events and
re-checks them on a schedule (`--seal-every`, default 15 s; `--reorg-every`,
default 60 s), and once more at shutdown. A retraction that only exists when
an operator remembers to run a command is not a product: the consumer who
acted on a fill at 14:02 has to find out at 14:02, not at 18:00. The daemon's
window is bounded in **slots** (`--reorg-lookback`, default 300 behind the
tip), because the chain — not the journal — decides how far a fork can reach.

The daemon's summary reports what the reorg work actually did
(`reorg_passes`, `reorg_considered`, `reorg_reverted`,
`reorg_incomplete_passes`), so "0 reverts" can be told apart from "the check
never ran" — the difference between evidence and silence.

Reorgs get **their own channel and their own cursor**, because a consumer
that already read an event cannot learn about its retraction from the event
stream: the event keeps its `seq` and is delivered exactly once. `tail`
reports the event's *current* seal, `reorg tail` reports the retraction
itself. A consumer that only reads events learns that a row now reads
`reverted`; one that reads the reorg channel learns it at the moment it
happened. Neither is optional if a consumer is to undo a fill.

`tools/e2e-reorg.mjs` proves both directions against live mainnet, because
the dangerous direction is the false positive — a detector that retracts a
live transaction is worse than no detector, since a consumer would undo a
real fill. Measured 2026-10-28:

- **negative:** real finalized transactions harvested from mainnet → **0
  reverts**, twice: 179 in the first run and 189 in the confirming run (the
  count moves with the harvest, the verdict does not);
- **positive:** a well-formed signature the chain does not know → detected,
  recorded, delivered exactly once on the reorg channel, and the second read
  delivered nothing (strict cursor);
- **honesty:** an unreachable endpoint → exit 3, `complete: false`, **0
  reverts** — a transport failure never becomes a fact about the chain.

The check is batched: one `getSignatureStatuses` call covers 200 rows, and
`getTransaction` is spent only on rows the first source already refused. On a
healthy chain a pass costs one call.

The same two-source rule now also guards `seal`: a `null` from the status
endpoint no longer writes `missing` on its own. Before this, one silent
source could turn a transaction the chain still knows into a recorded
"missing" — the same false statement a reorg marker must never publish.
`missing` now means *both* sources refuse the transaction; if the second
source cannot answer either, the row stays pending and the pass reports
`undecided`.

## Undoing a fill: the retraction reaches the consumer

Detecting a revert is only half the job. The other half is the question a
consumer actually has to answer: **which of my actions must be undone?**

`tools/fill-watcher.mjs` now keeps a ledger of the fills it read, and
`watchRetractions()` joins the reorg channel against that ledger:

- a retraction for a signature the consumer **acted on** comes back marked
  `undoable`, carrying the original fill's data — so the consumer reverses
  precisely what it did, amounts included;
- a retraction for a signature it **never read** comes back marked
  `undoable: false` — reported, never silently dropped, and never turned into
  an unfill of some *other* fill;
- the retracted fill leaves the open ledger and enters the undone ledger, so
  the same reversal is never applied twice.

The ledger is written BigInt-safe: the amount fields are u64 and exceed
`Number` range, and a ledger that quietly loses digits would make every later
comparison meaningless. (The watcher's own CLI used to crash on its first
printed fill for exactly that reason — `JSON.stringify` has no answer for a
BigInt.)

`tools/e2e-retraction.mjs` proves the whole path against live mainnet: the
daemon's own check finds the revert, the retraction arrives on its own
channel, the ledger join marks it undoable and carries the original fill,
an unrelated retraction is reported but not unfilled, and a second read
delivers nothing.

## Two kinds of gap

They need different tools, and conflating them is how a "we never lose events" claim turns out to be false:

- **Tail gap** — events after the newest signature the journal holds (crash, disconnect, restart). Fixed by walking back from the chain tip with `before` until the boundary **slot**. Bounded and cheap. reeflog closes this automatically on `start`.
- **Deep hole** — events missing from the middle of the journal's history. Filling it costs one RPC page per 1000 signatures (the endpoint's maximum), and public endpoints rate-limit: measured 2026-10-28, 19 of 40 consecutive `getSignaturesForAddress` calls answered HTTP 429 at ~0.63 pages/s. `start` therefore repairs a **rolling window** rather than all of history, and `reeflog repair --pages N` does a bounded pass on demand over whatever window you ask for (`--since-slot` raises the floor).

A walk that is cut short is reported as truncated, the gap stays **open**, and the command exits non-zero. A partially filled gap is never presented as filled, and a scan that read nothing cannot report `missing_count: 0` as if it had checked.

## Repeated use

```bash
reeflog init    --db reeflog.db --program <pubkey>
reeflog start   --db reeflog.db --seconds 60 [--repair-every 30] \
                                      [--repair-lookback 1800] [--repair-pages 40] \
                                      [--payload-limit 500]
reeflog seal    --db reeflog.db
reeflog verify  --db reeflog.db --pages 400        # detect, exit 3 if unfinished
reeflog repair  --db reeflog.db --pages 400        # fill signatures verify found
reeflog hydrate --db reeflog.db [--limit 500]      # fetch missing event content
reeflog tail    --db reeflog.db --cursor ./c.txt   # deliver events (content included)
reeflog status  --db reeflog.db
```

`tools/e2e-repair.mjs` runs detect → repair → prove against live mainnet on a
journal with planted holes and fails if any step misreports; it passed on
2026-10-28 with 333 planted holes: verify saw 333, repair filled 333, the
second verify returned `missing_count: 0, complete: true`, and a deliberately
truncated scan exited 3.

The raw report of the confirmation kill test is committed at
`research/killtest-full-after-fix-2026-10-28.json` (compact form: the
per-signature loss samples are truncated, every number and every check verdict
is the harness's own).

## Free and paid

- Free, MIT: daemon, journal, CLI, SDKs, webhook fan-out, dead-letter queue, archive backfill. No event or cursor limits.
- Paid later: managed always-on feeds, replicated journals with delivery objectives, and an enterprise mailbox with access control and a delivery audit trail. Events are never sold individually.

## Evidence and limits

Checked on 2026-10-23: Solana client errors already name the failure modes, including closed WebSockets, missing subscription IDs, and corrupt iterators. Yellowstone is a strong live transport but explicitly ephemeral. The closest general-chain project, Chainhook, is archived and does not cover Solana. One developer rebuilt reliable replay into Postgres as a four-star personal pipeline.

reeflog does not recover account bytes from before an event. That is a different problem. Public WebSocket endpoints are also rate-limited, so provider independence has to be real rather than assumed.

Still open: Yellowstone as a second transport, webhook fan-out, and a real
consumer other than my own RFQ fill polling and Whirlpool watcher. Reorg
handling marks a retracted event, delivers the retraction, runs the check
inside the daemon, and joins retractions against the fill watcher's ledger —
but the check still looks only at sealed rows near the tip, and a real fork
on mainnet has not been observed end to end.

## Next qualification

1. Put my own RFQ fill polling and Whirlpool watcher on `reeflog tail`. The fill watcher above is the prototype of it — it proves the mailbox yields fills cheaper and without loss, and it now also proves it can be un-done — but the production glue still polls, so user zero is earned, not assumed.
2. Archive gap-fill for holes deeper than the RPC window.
3. Yellowstone source and webhook fan-out.
4. Observe a real mainnet fork end to end: the detector has only ever been
   proven against a planted, well-formed signature the chain does not know.
   The daemon's loop makes that plausible to catch, but only a real fork
   proves it.
