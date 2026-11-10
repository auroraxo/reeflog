#!/usr/bin/env node
// A receiver that records the RAW bytes of the X-Reeflog-Event-Id header as
// they arrive on the wire, and answers 200 to every POST.
//
//   node tools/raw-receiver.mjs <out-file>
//
// Writes `<out-file>.port` with the bound port (so a caller can wait for
// readiness), then `<out-file>` with one entry per request.
//
// It runs as a SEPARATE PROCESS on purpose: the probe drives reeflog with
// `spawnSync`, which BLOCKS the event loop of its own process — a receiver
// in the same process could never answer, and every delivery would time out
// with `timeout: global` instead of the transport error we are measuring.
import http from 'node:http';
import fs from 'node:fs';

const outFile = process.argv[2];
if (!outFile) {
  console.error('usage: raw-receiver.mjs <out-file>');
  process.exit(2);
}

const seen = [];
const server = http.createServer((req, res) => {
  const headers = {};
  let raw = null;
  for (let i = 0; i < req.rawHeaders.length; i += 2) {
    const k = req.rawHeaders[i].toLowerCase();
    headers[k] = req.rawHeaders[i + 1];
    if (k === 'x-reeflog-event-id') raw = req.rawHeaders[i + 1];
  }
  const chunks = [];
  req.on('data', (d) => chunks.push(d));
  req.on('end', () => {
    seen.push({
      method: req.method,
      url: req.url,
      rawEventIdBytes: raw,
      signatureHeader: headers['x-reeflog-signature'] ?? null,
      body: Buffer.concat(chunks).toString('latin1'),
    });
    fs.writeFileSync(outFile, JSON.stringify(seen));
    res.writeHead(200, { 'content-type': 'application/json', 'content-length': '2' });
    res.end('{}');
  });
});

server.listen(0, '127.0.0.1', () => {
  fs.writeFileSync(outFile + '.port', String(server.address().port));
});
