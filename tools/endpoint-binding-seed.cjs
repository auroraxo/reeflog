const { DatabaseSync } = require('node:sqlite');
const d = new DatabaseSync(process.argv[2]);
const n = parseInt(process.argv[3] || '3', 10);
for (let i = 1; i <= n; i++) {
  d.exec(
    `INSERT INTO events (signature, slot, err, logs, source, first_seen, seal_status) VALUES ('SIG${i}${'x'.repeat(60)}', ${44000000 + i}, NULL, '[]', 'ws', 1700000000000, 'sealed')`
  );
}
console.log('inserted', n, 'events');
