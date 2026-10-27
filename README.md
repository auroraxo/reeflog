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
| Unit tests | 17 / 17 |

## RPC semantics, measured

`getSignaturesForAddress` does not behave the way the parameter names suggest. Measured with an anchor at the 4th of 10 tip signatures:

| Parameter | Observed behaviour |
|---|---|
| `before=<sig>` | returns signatures **older** than the anchor — the backwards-paging primitive |
| `until=<sig>` | returns signatures **newer** than the anchor |
| `since=<sig>` | behaves like `until` — **newer** |
| `before` + `until` together | rejected with `-32020 Transaction not found` for an inverted window |

Consequence: a backfill written with `since=<newest signature we hold>` returns an empty window and reports no loss while filling nothing. reeflog pages backwards with `before` and stops at the boundary signature instead. This was found by measuring the endpoint, not by reading the docs, and it is recorded in `src/backfill.rs`.

## Two kinds of gap

They need different tools, and conflating them is how a "we never lose events" claim turns out to be false:

- **Tail gap** — events after the newest signature the journal holds (crash, disconnect, restart). Fixed by walking back from the chain tip with `before` until the boundary. Bounded and cheap. reeflog closes this automatically on `start`.
- **Deep hole** — events missing from the middle of the journal's history. Filling it costs one RPC page per 500 signatures, and public endpoints rate-limit (observed: HTTP 429 after ~10 pages). v0 **detects and reports** deep holes rather than pretending to repair them. `reeflog verify` lists exactly which signatures are missing.

A walk that is cut short is reported as truncated and the gap stays **open**. A partially filled gap is never presented as filled.

## Free and paid

- Free, MIT: daemon, journal, CLI, SDKs, webhook fan-out, dead-letter queue, archive backfill. No event or cursor limits.
- Paid later: managed always-on feeds, replicated journals with delivery objectives, and an enterprise mailbox with access control and a delivery audit trail. Events are never sold individually.

## Evidence and limits

Checked on 2026-10-23: Solana client errors already name the failure modes, including closed WebSockets, missing subscription IDs, and corrupt iterators. Yellowstone is a strong live transport but explicitly ephemeral. The closest general-chain project, Chainhook, is archived and does not cover Solana. One developer rebuilt reliable replay into Postgres as a four-star personal pipeline.

reeflog does not recover account bytes from before an event. That is a different problem. Public WebSocket endpoints are also rate-limited, so provider independence has to be real rather than assumed.

Still open: reorg handling beyond marking an entry as reverted, Yellowstone as a second transport, webhook fan-out, and a real consumer other than my own RFQ fill polling and Whirlpool watcher.

## Next qualification

1. Replace my RFQ fill polling and Whirlpool watcher glue as user zero.
2. Reorg handling: detect a finalized event that reverts and mark it reverted.
3. Archive gap-fill for holes deeper than the RPC window.
4. Yellowstone source and webhook fan-out.
