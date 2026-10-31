# reeflog

A durable local mailbox for Solana events. A daemon journals logs and what
they did to account balances for selected programs into a cursor-addressed
append-only log, so a consumer resumes without gaps after a crash,
disconnect, or reorg.

**Status: v0 working (October 2026).** Live against mainnet-beta: see *Measured results* below. Webhook fan-out, a dead-letter queue, and archive backfill ship in this tree; Yellowstone as a second transport does not.

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
| Unit tests | 58 / 58 |

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

Consequence: a backfill written with `since=<newest signature we hold>` returns an empty window and reports no loss while filling nothing. reeflog pages backwards with `before` and stops at the boundary **slot** instead. The default walk starts from the chain tip so no held signature is ever an anchor; `--anchor-slot` starts it from a signature the CHAIN returned (the last signature of a finalized block), which is the same rule from the other side. This was found by measuring the endpoint, not by reading the docs, and it is recorded in `src/backfill.rs`.

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

## What the event did to accounts

Until 2026-10-31 the opening line of this README claimed the daemon journals
"account changes" — and `grep -ci account src/*.rs` returned 0 across every
source file. The claim was checked against the code for the first time in
the feature's own commit message, and the code was brought up to the claim
in the same commit.

The reason this is nearly free: hydration already pays for the whole
`getTransaction` response and kept only `meta.logMessages`. Measured with
`tools/probe-accounts.mjs` on live mainnet (Raydium AMM v4, 4 transactions,
2026-10-31): the discarded remainder of that same response averages 11.9 KB
per transaction and carries ~33.5 account keys, ~5.5 lamport-delta accounts
and ~3.5 changed SPL token balances per transaction. `src/accounts.rs`
extracts that into an envelope stored alongside the logs — at no extra RPC
cost for rows the journal had to recover.

The envelope contains: `lamport_changes` (index, address, before, after,
delta), `token_changes` (index, address, mint, owner, before, after,
delta) — amounts as exact decimal strings, never floats — plus
`instructions` and `err`.

`instructions` is the transaction's instruction bytes: every outer
instruction in execution order, then the CPIs each one spawned (`inner:
true`, tagged with the parent's index in `outer` and their own position in
`inner_index`), each with the resolved `program_id`, resolved `accounts`,
and the verbatim base58 `data` exactly as the RPC returned it — reeflog
never re-encodes bytes it did not produce. Account indices resolve through
the same canonical list the balances use (so a lookup table cannot shift an
argument to the wrong address); an index that does not resolve becomes an
explicit `null` at its own position, because position is information and a
shifted list would put an argument under the wrong name downstream. `err`
is `meta.err` verbatim (`null` = success), so a consumer never has to guess
whether a failed transaction's envelope describes an effect that happened.

The envelope does NOT contain account bytes or pre-event account state; see
*Evidence and limits*.

Two correctness traps the extractor is built against, each pinned by a test:

- **Lookup-table indexing.** `preBalances`/`postBalances` are indexed over
  static keys ++ loaded writable ++ loaded readonly, while a versioned
  transaction's `message.accountKeys` lists only the static keys. Indexing
  balances by that field silently attaches every changed balance to the
  wrong address. The account list is rebuilt in canonical order instead
  (`a_lookup_table_does_not_shift_the_balances_onto_the_wrong_addresses`).
- **A closed token account.** An account present in `preTokenBalances` and
  absent from `postTokenBalances` closed mid-transaction. The first live
  cross-check caught exactly this: the delta was encoded as an empty string.
  Absence on one side means zero on that side, so the delta is `-before`
  (`a_closed_token_account_reports_the_full_negative_delta`).

Delivery and enrichment:

- `hydrate --accounts` fetches envelopes for rows whose logs came from the
  WebSocket (the `logsNotification` carries no balances). Same honesty
  contract as payload: a rate limit leaves the debt, exit 3 reports it; the
  chain forgetting a transaction is recorded as `account_state`
  `unavailable`, never a pending-forever row and never an empty envelope.
- `tail` delivers the envelope as `accounts` on every row; `account_state`
  says why one is missing. `tail --with-accounts` delivers only enriched
  rows — and prints how many it skipped instead of swallowing them (they
  stay consumed; the cursor advances past them).
- `webhook push` bodies carry the envelope in the same signed delivery.
- `status` counts `accounts_held` / `accounts_missing` /
  `accounts_unreachable`.

Verified live (`tools/e2e-accounts.mjs`, mainnet, 22/22): envelopes from the
free path and the paid path, idempotence, delivery shape, cursor semantics,
and a field-by-field cross-check of every stored envelope — balances,
instructions and outcome — against an independent re-fetch and re-parse of
the same transactions from the chain. Two claims do not depend on live
traffic, because a sample of healthy transactions cannot show them: a local
stub serves a transaction with two CPIs (the parent tag and the position
inside it are pinned) and a failed transaction (`InstructionError [0,
{Custom: 42}]` must arrive verbatim; on successful transactions the live
comparison is `null === null` and would stay green if the field were
dropped). Against the pre-fix binary (741b622) the same harness reports
16/19 — exactly the instruction checks go red, and the 16 checks that
existed before stay green on both trees.

`tools/e2e-accounts-mutations.mjs` mutates the extractor six ways (address
shift, dropped delta, empty delta, wrong mint, instruction data shifted, CPI
position dropped) and each mutant must fail the live harness — a green
harness that cannot go red proves nothing. The probe counts only exit 1 (a
named check failed) as a catch: exit 2 is the harness failing to set up
against the rate-limited public endpoint, and counting that as a catch
would report a mutant as caught by a verdict that never ran.

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

### A real fork, end to end

`tools/e2e-reorg.mjs`'s positive case plants a well-formed signature the
chain never saw. That proves the two-source rule fires on an *unknown* key —
not that it fires on a transaction that really was executed, really reached
`confirmationStatus=finalized` with the root past its slot, and was then
taken back. That is the only case a consumer can actually lose money on,
and it was the open gap.

`tools/e2e-fork.mjs` manufactures it honestly on a local agave chain
(`node tools/e2e-fork.mjs`; `AGAVE_BIN` to point at a release). It runs a real
validator, sends real system transfers, and rolls the ledger back to a
copy of the blockstore taken *after* a first transaction and *before* a
second one. Nothing is planted — the signature the detector retracts is one
the chain itself made and then unmade. From the measured run (slots and
hashes differ run to run; they are recorded per run in the report):

- the **survivor** (slot 45) sits inside the snapshot: after the rollback its
  `getTransaction` still serves it and the block hash at its slot is
  byte-identical (`Cs4JNVar…` before and after);
- the **forked transaction** (slot 108) is finalized, sealed, and promised as
  final — then both sources refuse it, and the block at that same slot is a
  different block (`CgbDQNLd…` → `FGJcD4zT…`);
- the **running daemon** — not a one-shot CLI call — reports
  `1 event(s) REVERTED`, with `reorg_reverted=1`, `reorg_incomplete=0` over
  12 passes, and the journal ends with exactly one reorg record naming that
  signature while the survivor and a post-fork transaction both stay
  `finalized`;
- the retraction is delivered **exactly once**, the second read delivers
  nothing, and the consumer's ledger undoes the acted-on event and remembers
  it as retracted;
- controls: with the chain intact the detector finds **0** reverts (CLI and
  daemon), and a new transaction on the forked chain is ingested, sealed, and
  never retracted.

Measured 2026-10-29: **47/47 checks pass**, twice
(`research/fork-e2e-report.json`, verdict `passed`). The harness asserts the
toolchain gap as `skipped` (exit 3) rather than a pass when an agave binary
or the built binary is missing.

A real fork on **mainnet** has still not been observed end to end — that
requires watching long enough for mainnet to produce one.

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

One bug in the harnesses themselves is worth naming, because it hides in
every measurement made with them. The consumer helper resolved its child
process on Node's `'exit'` event, which fires when the process *dies* — not
when its piped stdout has been read. A `reorg tail` run could therefore
resolve with an empty output while the child had already written its own
cursor file, i.e. it had really delivered the row the harness could not see.
The consumer then reported **0 retractions for a retraction that was there**,
and the strict cursor made it unrecoverable on the next read. Every harness
now resolves on `'close'` (stdio drained) and fails loudly on a spawn error.
Measured 2026-10-29: this was the difference between 44/47 and 47/47 on the
same harness, same code.

## Webhook fan-out: a consumer that is not on this machine

`reeflog tail` makes the journal a local tool: the consumer has to run next
to the daemon and read a file. A webhook makes it a mailbox in the actual
sense — an outside service gets the events pushed to it, and the delivery
cursor survives a crash on either side.

```bash
cat > webhooks.json <<'EOF'
{
  "targets": [
    { "id": "fill-service", "url": "https://example.com/reeflog", "secret": "whsec_…", "timeout_ms": 5000 }
  ]
}
EOF

reeflog webhook push   --db reeflog.db --config webhooks.json   # one pass
reeflog webhook status --db reeflog.db --config webhooks.json   # cursors + failures
reeflog webhook dlq list   --db reeflog.db --config webhooks.json  # quarantined events
reeflog webhook dlq replay --db reeflog.db --config webhooks.json  # hand them over
reeflog start --db reeflog.db --webhook-config webhooks.json --webhook-every 5
```

**The contract, in order of importance:**

1. **A delivery is recorded only when the endpoint answered 2xx.** A 403,
   a timeout or a rate limit is recorded as a *failed attempt* with the
   status that caused it. `webhook push` exits 3 while a target is blocked,
   exactly like `verify` and `hydrate`: silence is never health.
2. **At-least-once, in order.** Per target the deliveries follow the
   journal's `seq` and stop at the first failure, so a consumer sees events
   in chain order and never with a hole in front of it. It may see one
   twice; the stable id (`X-Reeflog-Event-Id: <target>:<kind>:<seq>`) is
   what a consumer dedups on for exactly-once.
3. **The payload is authenticated.** Every request carries
   `X-Reeflog-Signature: sha256=<hex>` = HMAC-SHA256(secret, body) over the
   exact bytes sent (canonical JSON — `serde_json`'s sorted keys, so the same
   event always produces the same signature). The receiver checks it with
   one line of its own language:

   ```python
   import hmac, hashlib
   want = "sha256=" + hmac.new(secret, body, hashlib.sha256).hexdigest()
   ```
4. **A retraction is delivered too**, on its own cursor, and it is delivered
   *even when the event queue is blocked behind a refusal* — a retraction is
   the one message whose lateness costs money.
5. **Receipts make the crash window safe.** The state (per-target cursor)
   and the receipts (which exact body was accepted, by sha256) live in the
   journal. A daemon that dies between "POST succeeded" and "cursor advanced"
   does not deliver the event twice on the next pass.
6. **A refusal stops being a verdict only when it stops repeating.** One
   4xx can be an accident (mid-deploy, a config reload), so the refusal is
   counted. After `dead_letter_after` refused attempts on the same event
   (default 5, `0` disables it) the event moves to the **dead-letter queue**:
   it is quarantined with the exact bytes it was owed, and the target's
   cursor moves past it, so one consumer that will never accept one event
   does not stop its own mailbox forever while the events behind it are
   never delivered. A dead letter is **not** a delivery — no receipt is
   written, `push` reports it as `dead_lettered`, and `webhook status` shows
   it as `dead_letters` until it is replayed. Transient failures (5xx, 429,
   timeout) are never quarantined: an outage is not a verdict. Retractions
   are never quarantined either: a late retraction costs money, so its queue
   stays blocked rather than being papered over.

   ```bash
   reeflog webhook dlq list   --db reeflog.db --config webhooks.json
   reeflog webhook dlq replay --db reeflog.db --config webhooks.json
   ```

   The replay sends the stored body byte for byte, so the signature over it
   is the same signature it would have had — a receiver deduping on
   `X-Reeflog-Event-Id` sees the same event it rejected (or missed), not a
   new one. A replay that is refused again stays in the queue and exits 3;
   nothing is ever dropped.

### Measured, against an independent receiver

`tools/e2e-webhook.mjs` (2026-10-29, live mainnet, Raydium AMM v4): a
separate receiver process that reimplements the signature check with
`node:crypto`, decides its own answers, and logs what it actually received —
**14/14 checks passed** over 118 real events:

| Check | Result |
|---|---|
| Fan-out to two independent targets | 118/118 delivered to **each**, in seq order, 118 distinct |
| Signature verification at the receiver | 236 bodies, all valid (independent `node:crypto`) |
| Delivered bodies carry content | 6 of 118 with real non-empty `logs` |
| A **403** endpoint | 0 delivered, `blocked_seq: 1`, exit 3, cursor parked at 0, 0 receipts, attempt recorded as status 403 |
| A body signed with the **wrong secret** | receiver rejects, 0 delivered, blocked |
| A **429** endpoint | blocks the queue instead of dropping; recovery delivers 118 exactly once (118 accepted, 118 distinct) |
| Caught-up queue | delivers nothing on a repeat pass |
| Crash between POST and cursor (receipt without cursor) | 117 of 118 delivered, the owed event **not** re-sent, cursor catches up |
| Retraction while the event queue is blocked | reorg delivered and verified, event queue still blocked, exit 3 |
| Third implementation | Python `hmac` recomputes 476 bodies, **0 mismatches** against what the receiver accepted |

Report: `research/webhook-e2e-report.json`.

### The daemon path, killed mid-run

`tools/e2e-webhook-daemon.mjs` (2026-10-29, live mainnet): `reeflog start
--webhook-config` ingests over a live WebSocket and pushes to the receiver on
its own cadence. The daemon is **SIGKILLed** after 35 s — a mailbox that only
survives a clean shutdown is not a mailbox. **7/7 checks passed**:

| Check | Result |
|---|---|
| The daemon delivers what it ingested | 968 of 1.069 journal events delivered live (the rest were ingested after the last pass before the kill) |
| Order and no repeats | 968 deliveries, 968 distinct, seqs 1..968 |
| Signatures at the receiver | 968/968 valid (independent `node:crypto`) |
| The cursor survives the SIGKILL | cursor 968 = last delivered seq, 968 receipts |
| The follow-up resumes only the owed remainder | 101 delivered — exactly the 101 that were still owed, **0 re-sends** |
| The consumer ends up holding the whole journal | 1.069 distinct of 1.069 journal events |

Report: `research/webhook-daemon-e2e-report.json`.

### The dead-letter queue, measured against the same receiver

`tools/e2e-dlq.mjs` (2026-10-29, live mainnet, Raydium AMM v4): the receiver
permanently refuses ONE event (seq 1) and accepts everything behind it —
the exact case that used to stop that target's mailbox forever.
**13/13 checks passed**:

| Check | Result |
|---|---|
| A 503 is retried, not quarantined | blocked, 0 dead letters, 0 pending — an outage is not a verdict |
| The refused event lands in the dead-letter queue | 1 letter, status 403, `attempts: 3` (= `dead_letter_after`), state `pending` |
| The push reports the quarantine | `dead_lettered: 1`, `blocked: false` — the queue is no longer stuck |
| The events BEHIND the refused one are delivered | 54 of 54, in seq order — what the old design lost |
| The refused event is never counted as delivered | 0 receipts for seq 1, cursor at the tip |
| The quarantined body is the canonical event body | same type/seq/signature/slot as the journal row |
| The quarantined body carries its own signature | `stuck:event:1`, HMAC over exactly those bytes |
| Replay after recovery | `replayed: 1`, `failed: 0` |
| The replayed event verifies at the receiver | 1 accepted, signature valid |
| The dead letter is marked replayed | `state: replayed`, `replayed_at` set |
| **The replay sends byte-identical bytes** | 287 = 287 bytes — a replay is the same event, not a new one |
| A second replay has nothing left to send | `replayed: 0` |
| A wrong secret fails the receiver's own check | 55 requests, 0 with a valid signature |

Report: `research/dlq-e2e-report.json`.

### The harness can fail

A green harness proves nothing if it cannot go red.
`tools/e2e-webhook-mutations.mjs` mutates the sender in four ways — sign the
wrong bytes, count a refusal as a delivery, drop the receipt guard, gate the
retraction behind the blocked queue — and requires the e2e harness to catch
each one with its named check. **4/4 caught.**

`tools/e2e-dlq-mutations.mjs` does the same for the dead-letter harness —
**4/4 caught**: disable the queue (the refused event never lands and the
queue dies), count a quarantined event as a delivery, reclassify a 5xx as a
verdict, and send different bytes on replay.

The third mutation earned its keep: the first version of the refusal counter
selected on a hard-coded `400..500` status range while the classifier
decides "refusal" independently, so a mutation that reclassified 5xx as a
refusal produced a dead letter the counter never counted — the harness went
green over a real defect. The counter now follows the classification (a
recorded non-2xx status IS a refusal, by construction of where attempts are
recorded), and the mutation turns the harness red as it must.

### Limits

No TLS client authentication and no per-target payload filtering yet; the
secret is a shared HMAC key, not a certificate. A target that stays refused
used to hold its own queue forever; since the dead-letter queue it holds it
for `dead_letter_after` refused attempts, then the event is quarantined and
the queue moves on — `webhook status` shows `dead_letters`, `webhook dlq
list` shows what is in them, and `webhook dlq replay` hands them over. Other
targets always kept moving. Nothing here is an outside user yet: the
receiver above is my own test harness, not a third party. The first consumer
that is not reeflog lives in its own repository —
[auroraxo/whirlpool-watcher](https://github.com/auroraxo/whirlpool-watcher),
which reads this mailbox over `tail` and the signed webhook, decodes Whirlpool
`Traded` events with a layout it verified against live mainnet itself, and
keeps a fill ledger without spending an RPC call (see the qualification list).

A **second** consumer in a different repository now reads the envelope's
instruction bytes: [auroraxo/sememe](https://github.com/auroraxo/sememe)
harvests from this mailbox instead of paying `getSignaturesForAddress` /
`getBlock` / `getTransaction`, with the zero-RPC claim *enforced* (its
`fetch` is replaced by a recording rejector, and the run fails if any call
was attempted). Measured 2026-10-31 on a fresh 45-second window (456 events,
452 envelopes held): 156 payloads from 137 transactions with **0 network
attempts**; against independently fetched chain transactions, 10/10 sampled
payload-bearing transactions **byte-identical** and 10/10 sampled
no-target transactions corroborated, 0 different, 0 unavailable. Its own
report of the first run was wrong (it labelled v1-transaction rejections as
rate limiting) and says so — the retraction and the reader contract (what
was read, what was written, which counters reconciled) are in
[issue #1](https://github.com/auroraxo/reeflog/issues/1).

## Three kinds of gap

They need different tools, and conflating them is how a "we never lose events" claim turns out to be false:

- **Tail gap** — events after the newest signature the journal holds (crash, disconnect, restart). Fixed by walking back from the chain tip with `before` until the boundary **slot**. Bounded and cheap. reeflog closes this automatically on `start`.
- **Rolling-window hole** — events missing from the middle of the recent history the daemon saw. Repaired by `start` every 30s over a rolling window (default 1800 slots) so rows the WebSocket dropped do not age out unrescued.
- **Archive hole** — events missing from history deeper than one tip-walk budget reaches (measured 2026-10-30: 40 pages of 1000 signatures = 40,000 signatures covers only ~17,490 slots / ~2.4h on Raydium AMM v4). A tip-started walk burns its budget crossing history the journal never claimed, reports `truncated: true` and exits 3. `reeflog verify --anchor-slot` and `reeflog repair --anchor-slot` anchor the walk at a finalized block's last signature at the window's ceiling, making 24h-deep (or deeper) holes reachable and verifiable with zero tip-crossing overhead.

A walk that is cut short is reported as truncated, the gap stays **open**, and the command exits non-zero. A partially filled gap is never presented as filled, and a scan that read nothing cannot report `missing_count: 0` as if it had checked.

## Repeated use

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
reeflog tail    --db reeflog.db --cursor ./c.txt   # deliver events (content included)
reeflog webhook push --db reeflog.db --config webhooks.json   # deliver to outside targets
reeflog status  --db reeflog.db
```

`tools/e2e-repair.mjs` runs detect → repair → prove against live mainnet on a
journal with planted holes and fails if any step misreports; it passed on
2026-10-28 with 333 planted holes: verify saw 333, repair filled 333, the
second verify returned `missing_count: 0, complete: true`, and a deliberately
truncated scan exited 3.

`tools/e2e-archive.mjs` proves archive-anchored gap fill on live mainnet
at 24h depth (~216,000 slots back, well past a tip-walk's 17.5k-slot budget):
tip-started verify exits 3 refusing to claim completeness, anchored verify
sees all 39 planted holes, anchored repair fills all 39, second verify
returns `missing_count: 0, complete: true`, and `--anchor-slot` without value
anchors at the journal's ceiling. Report: `research/archive-e2e-report.json`.

The raw report of the confirmation kill test is committed at
`research/killtest-full-after-fix-2026-10-28.json` (compact form: the
per-signature loss samples are truncated, every number and every check verdict
is the harness's own).

## Free and paid

- Free, MIT: daemon, journal, CLI, SDKs, webhook fan-out, dead-letter queue, archive backfill. No event or cursor limits.
- Paid later: managed always-on feeds, replicated journals with delivery objectives, and an enterprise mailbox with access control and a delivery audit trail. Events are never sold individually.

Webhook fan-out ships in this tree: `reeflog webhook push` / `status`, signed
bodies, per-target durable cursors, in-order at-least-once with receipts —
plus the dead-letter queue (`reeflog webhook dlq list` / `replay`) for a
target that refuses one event instead of holding its queue forever. Archive
backfill (`verify --anchor-slot` / `repair --anchor-slot`) ships as well,
anchored at finalized blocks for holes deeper than the tip-walk window.

## Evidence and limits

Checked on 2026-10-23: Solana client errors already name the failure modes, including closed WebSockets, missing subscription IDs, and corrupt iterators. Yellowstone is a strong live transport but explicitly ephemeral. The closest general-chain project, Chainhook, is archived and does not cover Solana. One developer rebuilt reliable replay into Postgres as a four-star personal pipeline.

reeflog does not recover account bytes from before an event, and the account
envelope is balance MOVEMENTS an event caused — not the state of the accounts
involved. `tail --with-accounts` skips rows whose envelope has not been
fetched yet, which is honest but still a skip. That is a different problem.
Public WebSocket endpoints are also rate-limited, so provider independence
has to be real rather than assumed.

Still open: Yellowstone as a second transport, and a real consumer other
than my own RFQ fill polling and Whirlpool watcher. Reorg handling marks a
retracted event, delivers the retraction, runs the check inside the daemon,
and joins retractions against the fill watcher's ledger — and it has now been
proven against a REAL fork of a real chain, not only against a planted
signature. What remains unproven is the check's reach on mainnet: it still
looks only at sealed rows near the tip, and no mainnet fork has been observed
end to end.

## Next qualification

1. ~~Put my own RFQ fill polling and Whirlpool watcher on `reeflog tail`.~~ **User zero exists**: [auroraxo/whirlpool-watcher](https://github.com/auroraxo/whirlpool-watcher) (2026-10-31) is a real out-of-tree consumer — separate repository, no reeflog code linked, the `Traded` decode verified from live mainnet independently. Measured against a direct-RPC poller on the same window: 7/7 checks, including `window_superset` (0 in-window trades missing), `decoded_amounts_match` (two independently written decoders agree) and `chain_witness` (every sampled fill's amounts appear as real token-balance deltas); the watcher made **0 RPC calls** for 138 fills where the poller spent **85 calls** for 19 trade-bearing transactions. Its mutation probe goes red as required. What remains out of scope: this consumer is still the mailbox owner's own (user zero, not user one), and my own RFQ fill polling has not been moved over yet — a third party adopting it is what would close the second half of this item.
   **A second out-of-tree consumer landed the same day** (2026-10-31): [auroraxo/sememe](https://github.com/auroraxo/sememe) harvests the envelope's `instructions[]` instead of paying RPC (0 enforced attempts; 10/10 sampled transactions byte-identical against independently fetched chain data, 10/10 no-target corroborated, mutation red at the named check), its reader contract and one retracted claim of its own in [issue #1](https://github.com/auroraxo/reeflog/issues/1). Two consumers, two different byte surfaces (logs/fills and instruction bytes), both in my own repositories — still no third party.
2. Yellowstone source as a second transport.
3. Observe a real **mainnet** fork end to end: the detector is now proven
   against a real fork of a real chain (`tools/e2e-fork.mjs`, 47/47 checks,
   2026-10-29) and against a planted signature on live mainnet — but no
   mainnet fork has been caught while watching. The daemon's loop makes that
   plausible to catch; only an observed one closes it.
4. An account envelope says what an event *did* to balances, and the
   `instructions` field now carries the bytes a decoder needs to say *why*
   (outer instructions, CPIs, verbatim data, outcome). A consumer that
   wants to *know* an account (its state before the event, its authority,
   its data) still needs the chain: the envelope answers "what changed",
   not "what is". The next envelope would carry the account list a
   transaction touched — addresses, owners, writable flags — so a consumer
   can decide which accounts it cares about without a second RPC round
   trip.
