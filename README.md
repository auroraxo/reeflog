# reeflog

A durable local mailbox for Solana events. A daemon journals logs, account changes, and transactions for selected programs into a cursor-addressed append-only log, so a consumer resumes without gaps after a crash, disconnect, or reorg.

**Status: RFC stage (October 2026). No implementation yet.**

## The idea

Solana event consumption is normally a live firehose. `logsSubscribe` has no delivery guarantee, no durable cursor, and no catch-up after disconnection. Every project rebuilds reconnect, deduplication, gap-fill, and reorg handling, or it silently loses events.

reeflog treats events as a mailbox instead of a phone call:

- **ingest:** subscribe to configured programs, addresses, and log patterns over WebSocket or Yellowstone gRPC.
- **seal:** record events as confirmed, promote them when finalized, and mark reorged entries as reverted.
- **gap-fill:** backfill from signature history or public archives before resuming. A gap must be reported; silent loss is a protocol error.
- **journal:** use append-only SQLite with one monotone sequence and a portable file.
- **deliver:** provide a cursor-based SDK iterator, local HTTP or SSE, and webhook fan-out with retry and a dead-letter queue.

## Shape

```bash
reeflog init --program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc
reeflog start
reeflog tail --program whirLbMiicVdio4qvUfM5KAg6Ct8VwpYzGff3uctyCc --cursor ~/.agents/whirl.cursor
reeflog bind --pattern 'log-matches="swap"' --url http://localhost:3000/rfq-fill
reeflog backfill --days 10
reeflog status
```

The planned core is a Rust daemon and a TypeScript client. Decoding stays pluggable; reeflog does not itself decide what bytes mean.

## Free and paid

- Free, MIT: daemon, journal, CLI, SDKs, webhook fan-out, dead-letter queue, and archive backfill. No event or cursor limits.
- Paid later: managed always-on feeds, replicated journals with delivery objectives, and an enterprise mailbox with access control and a delivery audit trail. Events are never sold individually.

## Evidence and limits

Checked on 2026-10-23: Solana client errors already name the failure modes, including closed WebSockets, missing subscription IDs, and corrupt iterators. Yellowstone is a strong live transport but explicitly ephemeral. The closest general-chain project, Chainhook, is archived and does not cover Solana. One developer rebuilt reliable replay into Postgres as a four-star personal pipeline.

reeflog does not recover account bytes from before an event. That is a different problem. Public WebSocket endpoints are also rate-limited, so provider independence has to be real rather than assumed.

## Next qualification

1. Ingest one program over WebSocket into a sealed and staging journal, with CLI cursor resume.
2. Replace my RFQ fill polling and Whirlpool watcher glue as user zero.
3. Add archive gap-fill, then a Yellowstone source and webhook fan-out.
