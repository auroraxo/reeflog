// The binary a Node consumer needs, without a Rust toolchain.
//
// Why this exists (2026-11-07): `npm install github:auroraxo/reeflog#v0.6.0`
// hands a stranger the client, and the client's first call fails with
// `reeflog binary not found` — because the ONLY documented install path is
// `cargo install --git …`, which costs a Rust toolchain and a ~3 minute
// build (measured 2026-11-07, see research/bootstrap-2026-11-07.json).
// The release already carries prebuilt archives (linux-amd64, macos-arm64)
// that nothing points a stranger at.
//
// `bootstrap()` closes that gap and is verified here OFFLINE, against a
// local server that plays the role of GitHub:
//
//   B1 extract      — a served archive yields an executable bin + a sidecar
//                     that names the asset and the verified sha256;
//   B2 tamper       — bytes that do not match the release digest are
//                     REFUSED and no binary is left behind (fail closed:
//                     a wrong binary must never become the trusted one);
//   B3 no digest    — a release asset without a sha256 digest refuses too;
//   B4 platform     — an unsupported platform names the cargo fallback
//                     instead of downloading the wrong file;
//   B5 cache        — a second call resolves from disk with zero network;
//   B6 ENOENT hint  — the client's missing-binary error names bootstrap().
//
// The trust anchor is deliberately the same one `cargo install --git` uses:
// github.com/auroraxo/reeflog, read over TLS (the release API's stored
// digest for the asset). It does NOT defend against whoever can push tags
// and releases to that repo — no more than cargo does. That limit is
// stated in the README and in the measurement artifact.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';
import { bootstrap, ReeflogError, reeflog, RELEASE_ASSETS } from '../index.js';

const VERSION = 'v9.9.9';

/** Build a real .tar.gz containing an executable named `reeflog`, the way
 *  the release workflow does (`tar -czf … -C dir reeflog`). */
function makeArchive(script) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-archive-'));
  const bin = path.join(dir, 'reeflog');
  fs.writeFileSync(bin, script, { mode: 0o755 });
  const tarball = path.join(dir, 'reeflog.tar.gz');
  execFileSync('tar', ['-czf', tarball, '-C', dir, 'reeflog']);
  const bytes = fs.readFileSync(tarball);
  fs.rmSync(dir, { recursive: true, force: true });
  return bytes;
}

const sha256 = (buf) => createHash('sha256').update(buf).digest('hex');

/** A local stand-in for the release API + the asset CDN. */
function serve({ archive, digest = null, count = { n: 0 }, seen = [] }) {
  const server = http.createServer((req, res) => {
    if (req.url.includes('/releases/tags/')) {
      seen.push(req.url);
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({
        tag_name: VERSION,
        assets: [{
          name: 'reeflog-linux-amd64.tar.gz',
          digest,
          browser_download_url: `http://127.0.0.1:${server.address().port}/dl/reeflog-linux-amd64.tar.gz`,
        }],
      }));
      return;
    }
    if (req.url.startsWith('/dl/')) {
      count.n += 1;
      res.setHeader('content-type', 'application/gzip');
      res.end(archive);
      return;
    }
    res.statusCode = 404;
    res.end('not found');
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      resolve({
        apiBase: `http://127.0.0.1:${port}`,
        downloadBase: `http://127.0.0.1:${port}/dl`,
        count,
        seen,
        close: () => new Promise((r) => server.close(r)),
      });
    });
  });
}

test('B1: a served release archive yields an executable + a verified sidecar', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive, digest: `sha256:${sha256(archive)}` });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-boot-'));
  try {
    const out = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(out.bin, path.join(dest, 'reeflog'));
    assert.ok(fs.existsSync(out.bin), 'the binary must exist');
    assert.equal(out.cached, false);
    assert.equal(out.digest, sha256(archive));

    // executable, and it actually runs (this is the file a consumer spawns)
    const ran = execFileSync(out.bin, [], { encoding: 'utf8' });
    assert.match(ran, /reeflog 9\.9\.9/);

    // sidecar: what was installed, from which asset, verified how
    const meta = JSON.parse(fs.readFileSync(path.join(dest, 'install.json'), 'utf8'));
    assert.equal(meta.version, VERSION);
    assert.equal(meta.asset, 'reeflog-linux-amd64.tar.gz');
    assert.equal(meta.sha256, sha256(archive));
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('B2: bytes that do not match the release digest are refused, nothing installed', async () => {
  const archive = makeArchive('#!/bin/sh\necho evil\n');
  const srv = await serve({ archive, digest: `sha256:${'0'.repeat(64)}` });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-boot-'));
  try {
    await assert.rejects(
      () => bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        assert.ok(e instanceof ReeflogError);
        assert.match(e.message, /sha256 mismatch/i);
        return true;
      },
      'a mismatching digest must refuse',
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')), 'no binary may be left behind');
    assert.ok(!fs.existsSync(path.join(dest, 'install.json')), 'no sidecar may be left behind');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('B3: a release asset with no sha256 digest refuses instead of trusting the bytes', async () => {
  const archive = makeArchive('#!/bin/sh\necho reeflog\n');
  const srv = await serve({ archive, digest: null });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-boot-'));
  try {
    await assert.rejects(
      () => bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        // Specific wording, so this test cannot pass on the mismatch
        // refusal (MUTATION M2 was masked by it until this was pinned).
        assert.match(e.message, /no sha256 digest/i);
        assert.doesNotMatch(e.message, /mismatch/i);
        return true;
      },
    );
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')));
    assert.ok(!fs.existsSync(path.join(dest, 'install.json')));
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('B4: an unsupported platform names the cargo fallback, downloads nothing', async () => {
  const archive = makeArchive('#!/bin/sh\necho reeflog\n');
  const srv = await serve({ archive, digest: `sha256:${sha256(archive)}` });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-boot-'));
  try {
    await assert.rejects(
      () => bootstrap({ version: 'v0.6.0', platform: 'win32', arch: 'x64', apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest }),
      (e) => {
        assert.match(e.message, /no prebuilt/i);
        assert.match(e.message, /cargo install/, 'the refusal must name the fallback');
        return true;
      },
    );
    assert.equal(srv.count.n, 0, 'no bytes may be fetched for an unsupported platform');
    assert.ok(!fs.existsSync(path.join(dest, 'reeflog')));
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('B5: a second call resolves from disk with zero network', async () => {
  const archive = makeArchive('#!/bin/sh\necho "reeflog 9.9.9"\n');
  const srv = await serve({ archive, digest: `sha256:${sha256(archive)}` });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-boot-'));
  try {
    await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    const before = srv.count.n;
    const second = await bootstrap({ version: VERSION, apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(second.cached, true);
    assert.equal(srv.count.n, before, 'a cached install must not download again');
    assert.equal(second.digest, sha256(archive), 'the sidecar carries the verified digest');
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('B6: the client\'s missing-binary error names bootstrap()', async () => {
  const box = reeflog({ db: '/nonexistent/j.db', bin: '/definitely/not/reeflog' });
  await assert.rejects(
    () => box.status(),
    (e) => {
      assert.ok(e instanceof ReeflogError);
      assert.match(e.message, /reeflog binary not found/);
      assert.match(e.message, /bootstrap/, 'the error must point at the Rust-free path');
      return true;
    },
  );
});

test('B7: the mapped assets are EXACTLY the archives the release workflow builds', () => {
  const workflow = fs.readFileSync(
    path.resolve(import.meta.dirname, '..', '.github', 'workflows', 'release.yml'),
    'utf8',
  );
  const built = [...workflow.matchAll(/^\s*artifact:\s*(\S+)\s*$/gm)].map((m) => m[1]).sort();
  const mapped = Object.values(RELEASE_ASSETS).sort();
  assert.deepEqual(mapped, built,
    'RELEASE_ASSETS and the release matrix must name the same archives — mapping an unbuilt asset sends a stranger to a 404');
  assert.equal(new Set(mapped).size, mapped.length, 'no two platforms may share one asset name');
});

test('B8: the default release tag comes from the package version, not a hard-coded string', async () => {
  const pkg = JSON.parse(
    fs.readFileSync(path.resolve(import.meta.dirname, '..', 'package.json'), 'utf8'),
  );
  const source = fs.readFileSync(path.resolve(import.meta.dirname, '..', 'index.js'), 'utf8');

  // No hard-coded release tag may sit as the default: the pin-lag defect
  // (client fetches an OLD release while the package says a newer one) has
  // already been measured twice in this family of repos.
  assert.ok(source.includes('version = packageVersion()'),
    'the default version must be derived from package.json');
  assert.doesNotMatch(source, /version\s*=\s*['"]v\d+\.\d+\.\d+['"]/,
    'a hard-coded release tag in the client is a pin that can lag');

  // Observed, not inferred: the request must carry the PACKAGE's tag.
  const archive = makeArchive('#!/bin/sh\necho reeflog\n');
  const srv = await serve({ archive, digest: `sha256:${sha256(archive)}` });
  const dest = fs.mkdtempSync(path.join(os.tmpdir(), 'reeflog-boot-'));
  try {
    await bootstrap({ apiBase: srv.apiBase, downloadBase: srv.downloadBase, dest });
    assert.equal(srv.seen.length, 1);
    assert.ok(srv.seen[0].endsWith(`/tags/v${pkg.version}`),
      `the client must ask for the package's release tag v${pkg.version}, saw: ${srv.seen[0]}`);
  } finally {
    await srv.close();
    fs.rmSync(dest, { recursive: true, force: true });
  }
});

test('B9: every future release page must carry the Rust-free install path', () => {
  const workflow = fs.readFileSync(
    path.resolve(import.meta.dirname, '..', '.github', 'workflows', 'release.yml'),
    'utf8',
  );
  assert.match(workflow, /bootstrap\(\)/,
    'the release body template must show bootstrap() — a release page that only names cargo install puts a JS consumer back at the wall');
  assert.match(workflow, /cargo install/,
    'the source fallback must stay listed too');
  assert.match(workflow, /Limits:/,
    'the release body must state its limits, like every other surface here');
  // The asset digests come from GitHub at upload time; the workflow must not
  // pretend to publish checksums it does not generate.
  assert.doesNotMatch(workflow, /SHA256SUMS/,
    'no checksum file is generated by this workflow — bootstrap reads the release API digest');
});
