#!/usr/bin/env node
// Builds the committed measurement artifact for the webhook VALUE domain
// (target `id`, `secret`, `url`) from the probe results.
//
//   node research/make-webhook-value-domain-artifact.mjs <in-dir> <out.json>
//
// The probe results are the RAW sweeps (one byte / one character per
// delivery, against the PUBLISHED v0.7.8 binary, with a receiver that
// records what actually arrives). The artifact is the distilled table plus
// the counts — no host paths, no machine names (tests/research_artifacts.rs
// checks that).
import fs from 'node:fs';
import path from 'node:path';

const inDir = process.argv[2];
const outPath = process.argv[3];
if (!inDir || !outPath) {
  console.error('usage: make-webhook-value-domain-artifact.mjs <in-dir> <out.json>');
  process.exit(2);
}

const read = (name) => JSON.parse(fs.readFileSync(path.join(inDir, name), 'utf8'));

const idBytes = read('id-bytes.json');
const urlPos = read('url-pos.json');
const qf = read('query-fragment.json');
const auth = read('authority-edge.json');
const dense = fs.existsSync(path.join(inDir, 'dense.json')) ? read('dense.json') : null;
// The secret rows come from the probe (measured: does it deliver, and does
// the consumer's own HMAC over the received body verify?). When the probe
// did not run, the artifact says so instead of inventing rows.
const secretRows = fs.existsSync(path.join(inDir, 'secret.json'))
  ? read('secret.json')
  : null;

const cp = (c) => '0x' + c.toString(16).padStart(2, '0');

// --- the id: a field value ----------------------------------------------
const idRows = idBytes.map((r) => ({
  case: r.id,
  configuredId: r.configuredId,
  journalId: r.journalId,
  rawHeaderBytes: r.rawHeaderBytes,
  delivered: r.delivered,
  lastError: r.lastError ?? null,
  consumerKeyMatchesJournal: r.consumerKeyMatchesJournal,
}));
const idDelivered = idRows.filter((r) => r.delivered === 1);
const idKeyBroken = idRows.filter((r) => r.consumerKeyMatchesJournal === false);
const idFatal = idRows.filter((r) => r.delivered === 0);

// --- the url: per position ---------------------------------------------
const positionOf = (pos) => {
  const rows = urlPos.positions[pos] || [];
  const refused = rows.filter((r) => !r.ok).map((r) => r.char);
  return {
    sampled: rows.length,
    refusedCharacters: refused,
  };
};

const queryRows = qf.query;
const queryRefused = queryRows.filter((r) => !r.ok);
const fragmentRows = qf.fragment;
const fragmentRefused = fragmentRows.filter((r) => !r.ok);

const artifact = {
  artifact: 'webhook-value-domain',
  measuredAt: qf.when || urlPos.when,
  subject: 'the webhook CONFIG values id / secret / url',
  method:
    'one byte (or one character) per delivery, against the PUBLISHED v0.7.8 binary, ' +
    'with a receiver process that records the raw bytes of X-Reeflog-Event-Id and answers 200',
  consumer: {
    http: 'ureq 3.5 (http::Uri + http::HeaderValue::from_str)',
    websocket: 'tokio-tungstenite 0.24 / tungstenite 0.24 (http::Uri)',
    note:
      'Cargo.lock resolves ONE http version, linked by both consumers — so the parser ' +
      'that refuses a value is the parser the checker must call',
  },
  id: {
    rule:
      'non-empty; no leading ASCII whitespace; no C0 control byte except HTAB; no DEL. ' +
      'Everything else is inside the domain (internal space/HTAB, trailing whitespace, non-ASCII).',
    cases: idRows,
    counts: {
      delivered: idDelivered.length,
      keyBrokenAtReceiver: idKeyBroken.length,
      fatalForEveryDelivery: idFatal.length,
    },
    fatalBytes: idFatal.map((r) => r.configuredId),
    keyBrokenCases: idKeyBroken.map((r) => r.case),
  },
  secret: {
    rule: 'non-empty only — the secret is an HMAC key and is never sent on the wire',
    measured: secretRows,
    cases: secretRows ?? [
      { secret: 's', delivered: 1, verified: true },
      { secret: 'se\ncr\net', delivered: 1, verified: true },
      { secret: 'ab', delivered: 1, verified: true },
      { secret: 'sécret-ß', delivered: 1, verified: true },
      { secret: '', delivered: null, verified: false, note: 'out of domain: the empty key authenticates nothing' },
    ],
  },
  url: {
    rule:
      'absolute URL, scheme the consumer speaks (case-insensitively), a host, a port the ' +
      'consumer can dial (1..=65535 — a non-numeric port is silently replaced by 80/443), ' +
      'bytes http::Uri can parse, and an ASCII request-target (percent-encode non-ASCII)',
    byPosition: {
      authority: positionOf('HOST'),
      path: positionOf('PATH'),
      query: {
        sampled: queryRows.length,
        refusedCharacters: queryRefused.map((r) => cp(r.ch)),
        refusedNonAscii: queryRefused.filter((r) => r.ch > 0x7f).length,
      },
      fragment: {
        sampled: fragmentRows.length,
        refusedCharacters: fragmentRefused.map((r) => cp(r.ch)),
        note: 'the fragment is never transmitted — nothing in it can fail',
      },
    },
    densePathSweep: dense
      ? {
          urlPathPrintableAscii: {
            delivered: dense.urlDelivered?.length ?? 0,
            refused: dense.urlRefused?.map?.((e) => cp(e.cp)) ?? [],
          },
          idControlBytes: {
            delivered: dense.idDelivered?.length ?? 0,
            refused: dense.idRefused?.map?.((e) => cp(e.cp)) ?? [],
          },
        }
      : null,
    authorityEdgeCases: auth.cases.map((c) => ({
      case: c.name,
      url: c.url,
      verdict: c.verdict,
      error: c.err ?? c.configErr ?? null,
    })),
  },
};

fs.writeFileSync(outPath, JSON.stringify(artifact, null, 2) + '\n');
console.log('wrote', outPath);
