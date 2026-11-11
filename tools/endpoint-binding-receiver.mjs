// A receiver that answers 200 and records what it actually received.
import http from 'node:http';
import fs from 'node:fs';

const out = process.argv[2];
const portFile = out + '.port';
const seen = [];

const srv = http.createServer((req, res) => {
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    seen.push({
      eventId: req.headers['x-reeflog-event-id'] ?? null,
      seq: req.headers['x-reeflog-seq'] ?? null,
      sig: req.headers['x-reeflog-signature'] ?? null,
      len: body.length,
    });
    fs.writeFileSync(out, JSON.stringify(seen, null, 2));
    res.statusCode = 200;
    res.end('ok');
  });
});

srv.listen(0, () => {
  fs.writeFileSync(portFile, String(srv.address().port));
});
