# reeflog

A durable local mailbox for Solana events. A daemon journals logs and what
they did to account balances for selected programs into a cursor-addressed
append-only log, so a consumer resumes without gaps after a crash,
disconnect, or reorg.

**Status: v0.7.7 shipped (2026-11-09), v0 working.** Live against mainnet-beta: see *Measured results* below. Install the binary without a Rust toolchain: `npm install github:auroraxo/reeflog#v0.7.7` then `import { bootstrap } from 'reeflog-client'; await bootstrap()` (downloads the prebuilt release archive and verifies its sha256). Or from source: `cargo install --git https://github.com/auroraxo/reeflog --tag v0.7.7`. The full event envelope (logs, balances, instructions, account list with owners, the bytes each account held, and the authority each token account had at the moment of the event), the Node client (see *The Node client*), webhook fan-out, a dead-letter queue, and archive backfill ship in this tree; Yellowstone as a second transport does not.

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

**First-touch contract (measured 2026-11-02, `tests/quickstart.rs` +
`tests/rpc_timeout.rs`):** `init` is the only command that creates a journal,
and it refuses a `--program` that is not a base58 32-byte address — a typo
here used to be accepted silently and produced a mailbox that watches
nothing while reporting perfect health (`0 events`, `0 gaps`, exit 0).
Every other command refuses a `--db` path that has no journal, with the
`reeflog init …` line as the fix, instead of conjuring an empty one.
Every RPC call is bounded (10 s per call, two attempts for a stalled peer; a
429 still gets the full six-attempt backoff). Degraded is not the same as
hung: when the endpoint accepts connections and never answers, `start` can
exceed its `--seconds` while the shutdown passes pay their bounded timeouts,
but it always returns and names the failure on stderr (measured 2026-11-02:
pre-fix, 4 of 5 repetitions of `start --seconds 3` never returned at 45 s;
post-fix, worst observed run returned in 83 s with `rpc call timed out
twice` reported).

**A flag the CLI does not know is refused, not ignored** (measured 2026-11-06,
`tests/arg_parsing.rs` + `tests/tool_invocations.rs`,
`research/flag-surface-2026-11-06.json`). Until now a typo in a flag was
silently DROPPED and the command ran anyway — the most expensive kind of quiet
failure this product can make, because the tool reported doing something it
did not do:

| Before | What actually happened |
|---|---|
| `reeflog verify --db reeflog.db --pagesx 400` | scanned with the **default** page budget (40), not 400, and reported it as if it had |
| `reeflog status --dbx nope.db` | read the **default** `reeflog.db` and printed a healthy status for it |
| `reeflog status --db reeflog.db --bogus-flag` | printed a status, output identical to a correct run |
| `reeflog init --db probe.db --program <pk> --nope` | created the journal anyway |
| `reeflog init --db probe.db` (no `--program`) | created the journal FIRST, refused SECOND — a mailbox left behind |
| `reeflog status --db=other.db` | ignored the inline form and read the default journal |
| `reeflog status --help` | ran `status` and printed a health report instead of the usage |

Now every command declares the flags it accepts, and anything else is
refused **before** any journal is opened or created:

- an unknown flag is refused, and when the typo is close to a real flag the
  likely intent is named (`unknown flag --pagesx (did you mean --pages?)`);
  a far-away typo gets no guess, because a wrong suggestion is worse than none;
- a flag that lost its value is refused — including when the "value" is
  another flag (`--db --program` must not become a path called `--program`);
- a flag given twice is refused rather than silently resolved;
- a numeric flag whose value is not a non-negative whole number is refused,
  naming the flag and the value (`flag --limit expects a non-negative whole
  number, got `abc``) — before any journal is opened. Until 2026-11-08 an
  unparseable value silently became the DEFAULT: `tail --limit abc` delivered
  every row, and `tail --limit -1` delivered every row *twice as unbounded*,
  because SQLite reads `LIMIT -1` as "no limit". `0` stays legal (several
  flags document it as "disable this pass"), and the bare `--anchor-slot`
  keeps its documented meaning. Measured against the published v0.7.1 binary:
  4/10 cases behaved as specified, 2 "refusals" delivered 650 rows anyway —
  `research/numeric-flag-values-2026-11-08.json`;
- a numeric flag whose value is a number the CONSUMER cannot hold is also
  refused, naming the flag and the value (`flag --limit expects a
  non-negative whole number no greater than 9223372036854775807, got
  `18446744073709551615``) — and a cadence in seconds must be one this
  clock can schedule. "A non-negative whole number" is not one number
  space: until 2026-11-08 the checker accepted `u64` while consumers parse
  `i64` or add to a clock, so `tail --limit 18446744073709551615` delivered
  the DEFAULT limit (1000 rows, exit 0), `verify --since-slot
  9223372036854775808` dropped the window and scanned a different one, and
  `start --seconds 18446744073709551615` panicked while `start
  --repair-every …` hung. Measured against the published v0.7.2 binary:
  4/11 cases behaved as specified (2 silent defaults, 3 panics, 1 hang) →
  11/11 — `research/numeric-flag-domain-2026-11-08.json`;
- a URL flag whose value is not a URL the CONSUMER can speak is also
  refused, naming the flag and the value (`flag --ws expects a ws or wss URL
  — got `https://…``) — before any journal is opened. Until 2026-11-09
  `--ws` and `--rpc` were the only value flags with no domain at all: on the
  published v0.7.5 binary `start --ws ftp://x/y` exited 0 with a journal it
  had just created (0/11 cases behaved as specified), `--ws https://…` ran
  its full duration and recorded a gap for a scheme tungstenite can never
  speak, and `verify --rpc not-a-url` failed only after opening the journal,
  in ureq's words (`bad uri: …`) rather than reeflog's. The domains are the
  consumers', not an opinion: `ureq` resolves `http`/`https` only,
  `tokio-tungstenite` accepts `ws`/`wss` only — and both compare schemes
  case-insensitively, so `WS://…`, which the consumer accepts, is accepted
  here too. Measured against the published v0.7.5 binary: 0/11 refused →
  11/11, 6/6 guards green on both trees —
  `research/url-flag-domain-2026-11-09.json`;
- an argument that is not a flag is refused;
- `--flag=value` is honoured instead of falling back to the default;
- `--anchor-slot` keeps its documented **optional** value (bare = anchor at
  the journal's own window), and `--help` prints the usage instead of running
  the command.

`reeflog status` now names the journal it reports on (`"db": …`), so a report
can be checked against what was actually asked for. The false-positive guard
matters as much: `tests/tool_invocations.rs` pins every invocation shape the
`tools/e2e-*.mjs` harnesses pass, so a strict parser cannot quietly break the
project's own measured runs.

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

It also carries **`token_state`** (added 2026-11-02): the event-time
authority of every token account the transaction touched whose amount did
NOT change — `index`, `address`, `mint`, `authority`, `token_program`,
`source: "tx-time"`. Until then the envelope stated the authority only for
accounts whose balance moved, and dropped the rest. That is the majority
of what a transaction touches: measured on live mainnet (Raydium AMM v4,
10 transactions, `tools/probe-tx-time-authority.mjs`) **103 of 125 token
accounts (82.4 %)** carry an unchanged amount, and all 103 carry an
event-time authority the previous shape discarded. Changed accounts are
not duplicated — they keep their authority in `token_changes[].owner`, and
the two lists never claim the same account (pinned by
`a_changed_token_account_belong_to_token_changes_not_token_state`).

It also carries **the account list itself** (`accounts`, added 2026-10-31):
every account the transaction touched, in the same canonical order, with
`signer` and `writable` permissions and a `source` of `static`,
`lookup_writable` or `lookup_readonly`. Until then the envelope said *what
changed* and *how many* accounts there were, but not *which ones* or *whether
the transaction could write to them* — a consumer that needed those had to
pay for a second round trip to the chain.

Where the permissions come from is stated, not hidden, in
`accounts_flags_source`:

- `stated` — the response already carries them (the object form of
  `accountKeys` states signer/writable per static key);
- `header` — derived from the message header's three counts (writable signed,
  readonly signed, writable unsigned, readonly unsigned) plus the two loaded
  groups. Measured before landing against `jsonParsed`'s authoritative flags
  over 346 keys across 8 live mainnet transactions including two with address
  lookup tables: 346/346 agreement, 0 mismatches, static-prefix order
  identical. A real lookup-table transaction is pinned as
  `test/fixtures/account-list-lookuptable.json` and re-checked offline on
  every run (`the_derived_account_list_equals_the_authoritative_flags_of_a_real_lookup_table_transaction`);
- `unavailable` — the header is missing or its counts cannot describe the
  static keys, so `accounts` is `null` and the permissions are **refused**
  rather than guessed. A guessed permission is worse than an absent one: a
  consumer that trusts `writable` would act on it.

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

The envelope does NOT contain pre-event account state; see *Evidence and
limits*. What an account HELD at fetch time is the section below.

### What an account held (the bytes)

Ownership answers WHO controls an account; the bytes answer WHAT it held —
and both are outside the `getTransaction` response, so the bytes are their
own explicitly-budgeted pass: `hydrate --account-data` (after
`--accounts`, like `--owners`). It batch-fetches `getMultipleAccounts`
(100 addresses per call) for every address the held envelopes enumerate and
stores `account_data` inside each envelope. Every address gets exactly one
of three answers:

- **`held`** — the account's data bytes (base64), capped at 16384 B per
  address. The cap is a measured defense: on live mainnet (Raydium AMM v4
  window, 2026-11-02, `tools/probe-account-data.mjs`) account sizes sit at
  p50 165 B / p90 1024 B / p95 4483 B — but the tail is real (a
  1,048,588-byte Serum/OpenBook slab, a 262,156-byte one) — and the
  endpoint's `space` field reports the true size under the slice, so the
  row records `bytes` (the real size) and `truncated`/`truncation_verified`
  instead of pretending the cut is the whole. The probe semantics were
  pinned live first: `space` reports the full size even when sliced
  (1,048,588 reported as 16384 delivered), and an at-cap probe
  (offset=cap, length=1) returns the account when more data exists and
  `null` when it is gone-or-exact — an ambiguity the envelope reports as
  `truncation_verified: false` rather than guessed.
- **`missing`** — the chain does not have the account at fetch time (the
  same rule as the owner map: absence is a fact, recorded, never guessed).
- **`skipped_executable`** — the account is program code (same window: the
  second-largest accounts were 105 KB / 74 KB of SPL program binaries).
  Journaling program bytes as event data would be wrong in a different way
  than truncating state is, so they are delivered as a flag plus their
  true size, byte-free.

Measured on live mainnet (Raydium AMM v4, 2026-11-02, 20 s window, 119
rows): **14 calls for 1,235 distinct addresses**; sizes agree with an
independent re-fetch on **252 / 252**; the 81 executable accounts were all
flagged byte-free with correct sizes; 43 recorded-missing addresses
confirmed absent on an independent read; mint/owner decoded from the
stored bytes matched the transaction's own token metadata **35 / 35**.

Storing bytes honestly means admitting what a re-read can and cannot
prove: these are LIVE accounts, and between reeflog's fetch and any later
read a hot SPL token account's `amount` legitimately moves (measured in
the second e2e window: 39 of 252 re-reads differed this way). The e2e harness
therefore classifies differences instead of demanding byte-frozen
equality: identical, live-moving (a third read also differs),
volatile-region of a classic 165-byte token account (mint/owner immutable,
amount/delegate/close-authority are not), and — for layouts that grant no
immutable claim (mints, program state) — **unattributed**, which is
reported as a count and never waved through as a pass. The product claims
are anchored where time does not matter: completeness (every address
answered), true size (`space`), tx-consensus (decoded immutable fields vs
the transaction's own metadata), and absence-not-guessed.

The bytes are read AT FETCH TIME, not at event time — measured on the
probe window, 16–18 % of enumerated addresses no longer existed by the
time the pass ran. The envelope's slot and the `source` tag say when the
facts were true; historical state (the bytes AS OF the event) remains
open. Its boundary was measured on 2026-11-02 (`tools/probe-pre-event-bytes.mjs`,
report `research/pre-event-bytes-probe-2026-11-02.json`, 60 touched
accounts over 10 live transactions):

- **A past-slot question does not reach the past.** `getAccountInfo` with
  `minContextSlot` set to the event's slot answered with the current state
  and no error (requested slot 464542126, response context 464542734) —
  the parameter is a floor, not a time machine, and a caller who reads it
  as one silently receives today's bytes.
- **The transaction history carries no account bytes either.** 6 nearest
  predecessor transactions were fetched and walked: 0 account-state
  payloads anywhere in the responses; the 56 `data` fields found are
  instruction bytes, not account state. The "recurse over predecessor
  transactions" route this item carried is refuted, measured — the
  history cannot serve what it does not contain.
- **What the history does certify is the other direction.** For 7 of the
  60 touched accounts nothing touched them after the event, so the bytes a
  fetch returns now ARE the post-event bytes; the event's own post-state
  witnesses that claim — lamports 4/4, token mint/owner/amount decoded
  from the fetched bytes 2/2 — and on the 3 witnessed accounts the event
  had moved, the pre-side witness agreed 0/3, which is what keeps the
  check from being vacuous (wrong-side guard). 3 more certified accounts
  were already gone by fetch time and stay counted but unwitnessed.
  Certification is checked AFTER the fetch for a reason: an earlier
  classify-then-fetch run hit the race it implies (1 witness disagreement,
  root-caused to an account touched between the two calls), so the probe
  re-checks the boundary post-fetch and counts expiring certifications
  instead of hiding them.

Pre-event bytes therefore need a data source this RPC class does not
have (geyser with history, snapshot replay, an indexer) — not a second
fetch and not the chain's history, both measured here rather than argued.
The certification half is measured and NOT built into the envelope: no
field claims it.

### Which program owns what

Ownership answers a different question than the ones above — *what program
controls this account* — and unlike everything else in the envelope, `owner`
is **not** in the `getTransaction` response (measured 2026-11-01 with
`tools/probe-owners.mjs`: the response names a token account's `programId`
inside `pre/postTokenBalances`, but nothing states the owner of an arbitrary
account). So reeflog treats it as its own budget rather than pretending it is
free:

- **Free tx-time half:** every changed token account and lamport-change
  entry of a token account carries the SPL token program the transaction
  itself named (`token_program` / `owner_program` on the delta entry), read
  from the same response hydration already pays for.
- **Paid owner map:** `hydrate --owners` collects every address the held
  envelopes enumerate **across all owed rows first**, then batch-fetches
  `getMultipleAccounts` (100 addresses per call, owner-only dataSlice) and
  stores `account_owners` inside each envelope: an `owners` map for addresses
  the chain still has, and an explicit `missing` list for addresses it does
  not.

Measured on live mainnet (Raydium AMM v4, 2026-11-01):

| Measurement | Result |
|---|---|
| Probe: distinct addresses → batched calls | 406 addresses, 5 calls (291× vs 1 per address) |
| Probe: addresses gone at fetch time | 63 / 406 = **15.5 %**, recorded as missing |
| Probe: authority agreement (stated vs decoded from bytes vs map) | **12 / 12** token accounts, all three paths |
| E2E: stored owners vs an independent re-fetch | **412 / 412** agree |
| E2E: missing addresses re-fetch as null **and carry no owner** | **57 / 57** |
| E2E: batched cost | **12 calls for 1,111 distinct addresses** |
| E2E: tx-time token program vs chain | **20 / 20** statable entries |
| E2E: `owner_program` vs re-fetched account owner | **13 / 13** |
| Idempotence | second run: 0 rows, 0 calls |
| Unit tests | **97 / 97** |

Three honesty rules the implementation is built against, each pinned by a
test and by the live harness:

- **A gone address is a fact, never a guess.** ~15 % of addresses no longer
  exist when owners are fetched (accounts closed after the transaction).
  They land in `missing` with **no** owner entry — writing `null`, omitting
  the key, or filling in a plausible program would all be indistinguishable
  from "fetched" to a consumer.
- **No half-written rows.** A row is written only when *every* address it
  enumerates has an answer; a failed batch leaves the row owed instead of
  storing a map with a silent hole in it.
- **Owners are read at fetch time, not at event time.** An account can change
  owner after the transaction; the map's `source` says which pass wrote it,
  and nothing here claims historical state (see *Evidence and limits*).
  The exception that is not an exception: for token accounts the
  transaction itself touched, the envelope NOW carries the event-time
  authority (`token_state[].authority` / `token_changes[].owner`, read from
  the transaction's own `preTokenBalances` — the chain's statement at event
  time, measured to agree with a later fetch-time read 124/124 in the same
  probe window, and irreplaceable for the 1 of 125 accounts that no longer
  exist at all). Historical state for accounts a transaction did NOT touch
  remains open, unchanged.

The two pre-existing correctness traps above — lookup-table indexing and a
closed token account — apply to the owner map for the same reason: it is keyed
by the canonical address list, and the negative control in the probe proves an
absent account produces no entry rather than a fabricated one.

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
  says why one is missing. `tail --with-accounts` delivers only rows whose
  envelope is complete, and **holds** at the first row the debt queues still
  owe: the cursor stays behind it, the hold and the filling command
  (`hydrate --accounts`) are printed, and the command exits 3 — the same
  honesty contract as `hydrate` itself. Only a row with a recorded outcome
  (`unavailable`/`invalid`/`no_metadata`), which can never be filled, is
  skipped, reported, and consumed.
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

**Config values are checked before the pass starts**, because a config file
is one layer above the flag parser and fails the same way the parser used
to: a value the consumer cannot honour must be refused, not accepted into a
behaviour the file does not describe.

| Key | Domain | What is refused |
|---|---|---|
| `timeout_ms` | whole milliseconds in `1..=3600000` (1 hour) | `0` (the poster would silently re-read it as 1 ms), anything above the 1-hour ceiling (a timeout no process will live to see — an attempt that cannot finish within an hour is stuck, not slow), a fraction, a negative number, a string, an integer beyond `u64` |
| `dead_letter_after` | non-negative integer in `i64` | a fraction, a negative number, an integer outside `i64` (the count is compared against an `i64` refusal counter) |

Every refusal names **the key and the offending value** and happens before
any connection is attempted — a refusal with a report would look like a
delivery pass. `timeout_ms: 5000` (default) and `dead_letter_after: 5`
(default) are unaffected; `dead_letter_after: 0` keeps its documented
meaning of "disable the valve".

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

### The dead-letter report must match the queue

The dead-letter queue is the one place where "empty" means "a consumer is not
holding events it was promised". So the report about that queue has to be
right, and it was not. Measured 2026-11-09 on the **published v0.7.6
binary** (the sha256 of whose release archive equals the digest the release
API publishes), against a journal holding planted pending letters and a
config with two targets:

| command | v0.7.6 answered | the truth was |
| --- | --- | --- |
| `dlq list --target bogus` | exit 0, `{"targets": [], "pending_total": 0}` | 3 pending letters |
| `dlq replay --target bogus` | exit 0, `{"replayed": 0, "failed": 0}` | 3 pending letters |
| `dlq list --limit 2` | `pending: 2`, `pending_total: 2` | 7 pending letters |
| `dlq list` (target removed from config) | `pending_total: 0` | 1 pending letter |

Four separate lies. A typo in a target id was indistinguishable from a clean
queue. `pending` counted **the rows the listing showed**, not the letters that
are pending — so `webhook status` said `dead_letters: 7` for the same queue
that `dlq list` called 2. And a letter whose target had left the config was
invisible: it can never be replayed (no url, no secret) and no command ever
said so, which breaks the product's promise that nothing is ever lost.

The rule now:

* `--target <id>` must name a target the config knows — an unknown id is a
  **refusal** that names the flag, the value and the known ids, in both
  `list` and `replay`;
* `pending` and `pending_total` count the letters **pending in the journal**,
  not the rows a `--limit` happened to show; a truncated listing says
  `truncated: true`;
* a pending letter whose target is not in the config is **reported as an
  orphan** (`in_config: false`), counted in the total, and keeps `dlq replay`
  from exiting 0 — a letter nobody can replay is a fact the operator needs,
  not a number to round to zero.

Measured on both trees by `tools/measure-dlq-target-domain.mjs`
(`research/dlq-target-domain-2026-11-09.json`): the published v0.7.6 asset
violates **5 of 8** cases; this tree violates **0 of 8**, with the three
guards — a known target is still listed with its letters, a listing without a
filter still reports every config target, an empty queue still reads healthy
— unchanged on both trees.

The ladder is `tests/dlq_target_domain.rs` (10 tests) and
`tools/mutation-ladder-dlq-target-domain.sh` (**5/5 mutations caught**): the
unknown target is accepted again, `pending` counts the rows shown again, an
orphaned letter is dropped from the report again, a replay with an orphaned
letter reads clean again, and the refusal no longer names the offending
value.

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

## The cursor file is read strictly

The cursor file is state the consumer owns, and it never passes through the
flag parser — so the numeric-flag gate (v0.7.2, v0.7.3) never saw it.
Measured 2026-11-08 against the published v0.7.3 binary with 20 planted rows,
one cursor file per case (`research/cursor-state-2026-11-08.json`):

| What the existing cursor file held | v0.7.3 did | Now |
|---|---|---|
| `abc` | exit 0, **all 20 rows replayed**, file overwritten | refused, 0 rows, file untouched |
| `9223372036854775808` (outside i64) | exit 0, all 20 rows replayed | refused, 0 rows |
| `-1` | exit 0, all 20 rows replayed | refused, 0 rows |
| empty (what a truncate-then-crash leaves) | exit 0, all 20 rows replayed | refused, 0 rows |
| `15.0` | exit 0, all 20 rows replayed | refused, 0 rows |
| **missing** (a first run) | exit 0, 20 rows | unchanged — that is the documented start |
| `0` (a deliberate rewind) | exit 0, 20 rows | unchanged |
| `15` (resume) | exit 0, 5 rows | unchanged |

`read_cursor` was `.ok().and_then(parse).unwrap_or(0)`: every way an
existing cursor file can fail to carry a cursor collapsed into "start from
the beginning", delivered in full, with an exit code that says the run was
fine. The consumer's central promise — *resumes exactly where it stopped* —
is not a promise a silent restart from 0 can keep. The rule now: a missing
file is a first run; an existing file that does not hold a non-negative
whole number inside `i64` is a refusal that names the file and its content,
before the journal is opened, and the file is left untouched. `reorg tail`
reads cursors the same way. Ladder: `tests/cursor_state.rs` (7 defect cases
RED on v0.7.3, 6 guards green on both trees), artifact pinned by
`tests/cursor_state_measurement.rs`.

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
reeflog hydrate --db reeflog.db --owners           # + owning program per address (batched)
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

- Free, MIT: daemon, journal, CLI, the Node client (below), webhook fan-out, dead-letter queue, archive backfill. No event or cursor limits.
- Paid later: managed always-on feeds, replicated journals with delivery objectives, and an enterprise mailbox with access control and a delivery audit trail. Events are never sold individually.

## The Node client: the exit-code contract, as a library

The CLI is a contract before it is a convenience: exit 0 delivers and
advances the cursor, exit 3 (on `tail`) means the delivery **held** at a row
whose account envelope is still owed — rows before it are delivered, nothing
at or after it is consumed — and exit 1 is a refusal. A consumer that maps
those three outcomes wrongly either replays rows forever or drops the owed
row without ever seeing it. Until now every Node consumer had to derive that
by hand.

`index.js` (npm package `reeflog-client`, MIT, zero dependencies, Node ≥ 18)
ships that mapping as one file. It never opens the journal — every call runs
the `reeflog` binary, so there is exactly one implementation of the reader
contract, and a library's idea of the schema cannot drift from the daemon's.

```bash
npm install github:auroraxo/reeflog#v0.7.7   # the client ships from this repo
```

The client needs the `reeflog` binary. `bootstrap()` gets it **without a
Rust toolchain** — it downloads the prebuilt archive the release already
ships for your platform (linux-amd64, macos-arm64), verifies its bytes
against the sha256 the GitHub release API stores for that asset, and only
then installs it to `~/.reeflog`:

```js
import reeflog, { bootstrap } from 'reeflog-client';

const { bin, digest, binarySha256 } = await bootstrap();
//   digest       = sha256 of the ARCHIVE (what the release API stores)
//   binarySha256 = sha256 of `bin` itself — check this against the file
const box = reeflog({ db: 'reeflog.db', bin });
```

- **Refuses instead of trusting.** A download whose sha256 does not match
  the release digest is refused and nothing is written — no binary, no
  sidecar. A release asset with no digest is refused too.
- **A cache hit is verified, not assumed.** The sidecar (`install.json`)
  records the extracted binary's own `binarySha256`; a second call re-hashes
  the file on disk and only reports `cached: true` when it matches. Before
  v0.7.1 the cache path compared only version + asset name, so a replaced or
  truncated binary came back as `cached: true` under a digest that named the
  ARCHIVE, not the bytes on disk (measured: claimed `f6b36c53…`, actual
  `6b9fd8b7…`). A sidecar without `binarySha256` — every install made before
  v0.7.1 — is re-fetched once to upgrade it, then caches again. The check
  defends against a stale or corrupted cache being reported as verified; an
  attacker who can write your install directory can simply re-run
  `bootstrap()` afterwards, so it is not a defence against a hostile local
  user. Live measurement: `research/bootstrap-cache-2026-11-07.json`.
- **A platform without a published archive is refused by name** (the
  refusal lists what exists and gives the `cargo install` fallback), rather
  than fetching a file that is not there.
- **The default tag comes from the package version**, never a hard-coded
  string, so the client can never fetch an older release than the package
  it ships in.
- **Trust is the same anchor as `cargo install --git`**: this repository
  over TLS. It protects against a corrupted or substituted download, not
  against whoever can push tags and releases here. If you want to be
  independent of that, compare `digest` against the asset's published
  sha256 yourself.
- Measured end to end against the published release —
  `research/bootstrap-2026-11-07.json` (install + cached re-run + real
  rows delivered through the installed binary) and
  `research/bootstrap-cache-2026-11-07.json` (fresh install, verified cache
  hit, tampered binary, pre-v0.7.1 sidecar upgrade, other-platform asset).

```js
import reeflog from 'reeflog-client';

const box = reeflog({ db: 'reeflog.db' });       // bin on PATH, or { bin: '/path/to/reeflog' }
const s   = await box.status();                  // parsed health: events, envelope debt, gaps

const { events, held, cursorValue } = await box.tail({
  limit: 500,
  consumer: 'my-service',                        // separate default cursor per consumer
  withAccounts: true,                            // hold rather than deliver an unreadable row
});
if (held) {
  // exit 3: `held.seq` and `held.signature` name the owed row; the rows
  // above it are already delivered and the cursor already advanced past
  // them. Pay the debt (`reeflog hydrate --accounts`) and call again — the
  // held row and everything behind it arrive next, nothing was lost.
}

const { retractions } = await box.reorgs({ consumer: 'my-service' });  // own cursor channel
await box.verify({ pages: 400 });   // incomplete scan THROWS (exit 3): no clean-looking result
await box.hydrate({ accounts: true });
```

The rules the client enforces instead of leaving to the caller:

- **exit 0 vs exit 3 are different answers.** `tail` returns
  `{ events, held, cursorValue }`; `held: null` is the only shape that means
  "the batch is fully delivered". `verify`/`repair`/`hydrate` throw on exit 3,
  because a pass that proves nothing must never arrive as a resolved value.
- **exit 1 keeps the binary's words.** The thrown `ReeflogError` carries the
  CLI's own refusal (`no journal at … — run \`reeflog init …\``), plus
  `exitCode`, `stderr` and the `args`, so a caller can act on it rather than
  on a rephrased guess.
- **Cursors are the binary's.** The client only chooses a default path per
  db + channel + consumer label, so two consumers of one journal never share
  a cursor by accident; pass `cursor` to name it yourself.
- **A missing binary says how to install one** — naming `bootstrap()` (no
  Rust toolchain) and the `cargo install` fallback — instead of crashing
  with `ENOENT`.

The contract is tested against the real binary and a real journal in
`test/client.test.mjs` (`npm test`, 8 tests): delivery order, no replay
across a resumed cursor, the hold at an owed row (delivered before, not
consumed at/after, delivered once the debt is paid), separate cursor
channels, refusals with their fix line intact, and the incomplete-verify
throw. CI runs them on every push.

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
involved. `tail --with-accounts` does not skip past a row whose envelope has
not been fetched yet: it holds the cursor there and exits 3 until
`hydrate --accounts` pays the debt, so an owed row can never be lost to the
consumer. Rows the chain itself made unfillable are the only ones skipped,
and each skip is reported. That is a different problem.
Public WebSocket endpoints are also rate-limited, so provider independence
has to be real rather than assumed.

Ownership is the partial exception: `hydrate --owners` fills an `account_owners`
map (owning program per address), and changed token accounts name their token
program for free. Two limits remain and are stated rather than smoothed over:
owners are read **at fetch time**, not at event time (an account may have
changed owner since), and account **bytes / pre-event state** are still not in
the envelope at all — that stays open, see *Next qualification* item 4.

The account BYTES are now in the envelope too (`hydrate --account-data`),
which closes one half of that sentence and not the other: the bytes say what
an account held **at fetch time**, not what it held when the event happened.
16–18 % of the addresses a transaction enumerates no longer exist by the
time the pass runs (measured on two live windows), and a hot token
account's balance moves between two reads. Historical state — the bytes AS
OF the event — remains open, and no envelope field claims otherwise.
Measured 2026-11-02 (`tools/probe-pre-event-bytes.mjs`,
`research/pre-event-bytes-probe-2026-11-02.json`): both free routes to
pre-event bytes are dead — a past-slot `getAccountInfo` parameter answers
anchored at the current context slot (requested 464542126, got 464542734),
and predecessor transactions carry instruction bytes but no account-state
payload (0 across 6 scans) — so the boundary is "no data source on this RPC
class", not "a second fetch" and not "the chain's history". What history
does prove, measured on the same window: 7/60 touched accounts had no
touch after the event, for which fetch-time bytes ARE the post-event bytes
(witness: lamports 4/4, token mint/owner/amount 2/2; wrong-side guard on
moved accounts 0/3; 3 certified accounts already gone at fetch time are
counted, never witnessed). That certification is checked after the fetch,
not before: a certification can expire mid-run (an earlier
classify-then-fetch run showed exactly one such disagreement,
root-caused) — and it is measured, not shipped: no envelope field claims
it, and it covers the post-event side only.

Still open: Yellowstone as a second transport, and a real consumer other
than my own two repositories (the Whirlpool watcher and sememe). Reorg handling marks a
retracted event, delivers the retraction, runs the check inside the daemon,
and joins retractions against the fill watcher's ledger — and it has now been
proven against a REAL fork of a real chain, not only against a planted
signature. What remains unproven is the check's reach on mainnet: it still
looks only at sealed rows near the tip, and no mainnet fork has been observed
end to end.

## Next qualification

1. ~~Put my own RFQ fill polling and Whirlpool watcher on `reeflog tail`.~~ **User zero exists**: [auroraxo/whirlpool-watcher](https://github.com/auroraxo/whirlpool-watcher) (2026-10-31) is a real out-of-tree consumer — separate repository, no reeflog code linked, the `Traded` decode verified from live mainnet independently. Measured against a direct-RPC poller on the same window: 7/7 checks, including `window_superset` (0 in-window trades missing), `decoded_amounts_match` (two independently written decoders agree) and `chain_witness` (every sampled fill's amounts appear as real token-balance deltas); the watcher made **0 RPC calls** for 138 fills where the poller spent **85 calls** for 19 trade-bearing transactions. Its mutation probe goes red as required. What remains out of scope: this consumer is still the mailbox owner's own (user zero, not user one), and a third party adopting it is what would close this item.
   **Correction, measured 2026-10-31**: the second half of this item as originally written ("my own RFQ fill polling has not been moved over yet") named work that does not exist. A scan of all 28 repositories of mine on this machine plus the home directory for an RFQ fill poller found none — no such script, service or file exists. The premise behind that sentence was the falsified "21 own key-load sites" claim from 24.10, the same category of error: a sentence that repeated a plausible-sounding inventory without checking it against the disk. The honest remainder of this item is the one that was always the real gate: **a third party using reeflog**, not the migration of a poller nobody ever wrote.
   **A second out-of-tree consumer landed the same day** (2026-10-31): [auroraxo/sememe](https://github.com/auroraxo/sememe) harvests the envelope's `instructions[]` instead of paying RPC (0 enforced attempts; 10/10 sampled transactions byte-identical against independently fetched chain data, 10/10 no-target corroborated, mutation red at the named check), its reader contract and one retracted claim of its own in [issue #1](https://github.com/auroraxo/reeflog/issues/1). Two consumers, two different byte surfaces (logs/fills and instruction bytes), both in my own repositories — still no third party.
   **The install blocker is gone (2026-11-01, v0.1.0)**: before this release a third party could not adopt reeflog at all — there was no tag, no release, no install path, and the binary could not even say which version it was. `v0.1.0` is now a real release (`cargo install --git https://github.com/auroraxo/reeflog --tag v0.1.0`, per-platform archives attached to the [release](https://github.com/auroraxo/reeflog/releases/tag/v0.1.0)), the binary reports its own version, and the release workflow refuses to publish a tag whose version the binary disagrees with. Proven from a stranger's empty directory: installed from the tag, then journaled 535 live mainnet events with 0 gaps. That removed the *install* blocker; the *adoption* blocker (an unaffiliated third party) is untouched and remains the real remainder of this item.
   **Distribution opened, adoption instrumented (2026-11-02)**: the first earned placement is submitted as [StockpileLabs/awesome-solana-oss#93](https://github.com/StockpileLabs/awesome-solana-oss/pull/93) (Data section, per that list's CONTRIBUTING; the list merges about half of what outsiders send — 24 merged vs 22 closed at submission time, so a merge is not assumed). And "no third party" is no longer a sentence in a README but a measured verdict: `node tools/measure-adoption.mjs` classifies external use from non-owner stars/forks/issues/PRs, non-GitHub referrers and the live state of the placement PR. Baseline committed in `research/adoption-2026-11-02.json`: 0 non-owner stars, 0 forks, 0 external issues/PRs, 0 external referrers, 14-day views 0/0 — verdict `not-distributed-not-consumed` (the 356 clones in the same window are our own CI checkouts, explicitly not an adoption signal). Controls: the probe flips to `user-one-evidence` on a repo with real outsiders (100 non-owner forks, outsider issues), and prints `NO VERDICT` with exit 3 when a data source is partial — a merge alone will read as `distributed-not-yet-consumed`, never as adoption.
2. Yellowstone source as a second transport.
3. Observe a real **mainnet** fork end to end: the detector is now proven
   against a real fork of a real chain (`tools/e2e-fork.mjs`, 47/47 checks,
   2026-10-29) and against a planted signature on live mainnet — but no
   mainnet fork has been caught while watching. The daemon's loop makes that
   plausible to catch; only an observed one closes it.
4. ~~An account envelope says what an event *did* to balances… The next envelope would carry the account list a transaction touched — addresses, owners, writable flags — so a consumer can decide which accounts it cares about without a second RPC round trip.~~ **Landed 2026-10-31**: `accounts[]` carries every account a transaction touched with `signer`, `writable` and `source`, derived from the message header and the two loaded groups (or stated by the response where it says so), with `accounts_flags_source` recording which. Falsified live against `jsonParsed`'s authoritative flags: 346/346 keys over 8 transactions including lookup tables, 0 mismatches; the loaded path is pinned offline by a real transaction fixture.
   **Ownership landed 2026-11-01**: `hydrate --owners` resolves the owning program of every address the envelope enumerates (batched `getMultipleAccounts`, 12 calls for 1,111 addresses in the live e2e), and changed token accounts name their token program for free inside the transaction response. Falsified live: 412/412 stored owners agree with an independent re-fetch, 57/57 addresses the chain no longer has are recorded in an explicit `missing` list with **no** owner entry (never guessed), and the probe pins the authority three ways (stated vs decoded from the account's own bytes vs the map): 12/12.
   **Account bytes landed 2026-11-02**: `hydrate --account-data` stores what each enumerated account HELD — the bytes, not just the owner — batched (11 calls for 964 distinct addresses in the live e2e), capped at 16 KiB per address with the real size and a verified truncation flag, program code flagged instead of stored, and absence recorded as `missing`. Premise measured before building (`tools/probe-account-data.mjs`, 428 distinct addresses over 30 live transactions): p50 165 B, p95 4483 B, 5 addresses over the cap, and a 1,048,588-byte slab — which is why the cap exists and why program binaries are not journalled as event data. Falsified live: 258/258 stored sizes equal the chain's byte length; 232 of 258 re-reads byte-identical, 17 live-moving, 4 in the mutable region of a classic token account, 5 unattributed (no immutability claimed — reported, not waved through); the mint/owner decoded from the stored bytes matched the transaction's own token metadata 12/12; 82/82 program accounts flagged byte-free with correct sizes; 47/47 recorded-missing addresses confirmed absent on an independent read; a second run stored nothing and cost 0 calls.
   **What remains open is the half that is genuinely different**: account bytes AS OF the event (historical state), not bytes at fetch time. That is a different problem, and it was last written here as "needs the chain's history, not a second fetch" — **measured 2026-11-02, that sentence was wrong in its remedy**: the history has no account bytes to serve (6 predecessor transactions, 0 account-state payloads), and a past-slot parameter on a public RPC answers with current state, not the requested slot. Pre-event bytes need a data source this RPC class does not offer; the two dead routes and the measured boundary are in *Evidence and limits* (`tools/probe-pre-event-bytes.mjs`). The same probe found the one thing history does prove — for 7/60 touched accounts with no later touch, fetch-time bytes ARE the post-event bytes (witness 4/4 lamports, 2/2 token fields, wrong-side guard 0/3; 3 certified accounts already gone stay counted but unwitnessed) — measured, deliberately not shipped as an envelope field, and it certifies the post-event side only.
   **Released as v0.2.0 (2026-11-02)**: this owners + account-bytes envelope is the release. The historical-state half above remains the open item it was the day before.
