// Measure actual external adoption of reeflog — the card's open claim is
// "no unaffiliated third party", and that claim has never been measured, only
// asserted. This probe asks the question with evidence instead of memory.
//
// Checks:
//  1. Repo metadata is reachable, public, MIT-licensed (the placement
//     checklist depends on all three).
//  2. Star and fork census with the owner excluded — a star from auroraxo
//     is not adoption.
//  3. Issue/PR census with the owner excluded — an outsider opening an
//     issue is the cheapest form of real use evidence.
//  4. Traffic measured over GitHub's fixed 14-day window (views/clones,
//     uniques). Owner-only endpoint; CI checkouts inflate clones, so the
//     star/fork/issue census is the primary signal, traffic is context.
//  5. Referrer census — non-GitHub referrers are the only evidence that
//     anyone arrived from outside github.com itself.
//  6. Placement channel state — the distribution PR is fetched LIVE from
//     the upstream list (MERGED means the channel delivered; it is NOT
//     adoption by itself and the verdict says so).
//  7. Classification, derived only from 1-6:
//       user-one evidence : any non-owner star/fork/issue/PR
//       distributed, not yet consumed: placement merged, but zero
//           non-owner signals
//       not distributed, not consumed: placement still open/none
//
// Exit codes: 0 = measurement complete (any verdict), 3 = a data source
// failed or looked cached — no verdict is issued on partial evidence.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const REPO = process.env.ADOPTION_REPO || 'auroraxo/reeflog';
const OWNER = process.env.ADOPTION_OWNER || REPO.split('/')[0];
const UPSTREAM = 'StockpileLabs/awesome-solana-oss';
const PLACEMENT_PR = Number(process.env.PLACEMENT_PR || 93);
const OUT = process.argv.includes('--out')
  ? process.argv[process.argv.indexOf('--out') + 1]
  : null;

const startedAt = new Date().toISOString();
const failures = [];

function gh(args, optional = false) {
  try {
    return JSON.parse(
      execFileSync('gh', ['api', ...args], { encoding: 'utf8', timeout: 60000 })
    );
  } catch (err) {
    const msg = `gh api ${args.join(' ')} failed: ${String(err.message).split('\n')[0]}`;
    if (optional) {
      failures.push(msg);
      return null;
    }
    console.error('FATAL ' + msg);
    process.exit(3);
  }
}

// ---- 1. repo metadata -----------------------------------------------------
const repo = gh([`repos/${REPO}`]);
const checks = {};
checks.repoMetadata = {
  public: !repo.private,
  license: repo.license && repo.license.spdx_id,
  description: repo.description,
  ok: !repo.private && repo.license && repo.license.spdx_id === 'MIT',
};

// ---- 2. stars and forks, owner excluded -----------------------------------
const stargazers = gh([`repos/${REPO}/stargazers?per_page=100`], true) || [];
const forks = gh([`repos/${REPO}/forks?per_page=100`], true) || [];
const nonOwnerStars = stargazers.filter((s) => s.login && s.login !== OWNER);
const nonOwnerForks = forks.filter((f) => f.owner && f.owner.login !== OWNER);
const starLogins = nonOwnerStars.map((s) => s.login);
const forkLogins = nonOwnerForks.map((f) => f.owner.login);
checks.starForkCensus = {
  starsTotal: repo.stargazers_count,
  forksTotal: repo.forks_count,
  nonOwnerStars: starLogins,
  nonOwnerForks: forkLogins,
  starsApiUsable: stargazers.length > 0 || repo.stargazers_count === 0,
  ok: true,
};

// ---- 3. issues and PRs, owner excluded ------------------------------------
// GitHub's issues list includes PRs; both are outsider evidence.
const issues = gh([`repos/${REPO}/issues?state=all&per_page=100`]) || [];
const externalIssues = issues.filter(
  (i) => i.user && i.user.login !== OWNER && !i.user.login.endsWith('[bot]')
);
checks.issueCensus = {
  total: issues.length,
  external: externalIssues.map((i) => ({
    n: i.number,
    author: i.user.login,
    isPR: Boolean(i.pull_request),
    title: i.title,
  })),
  ok: true,
};

// ---- 4. traffic (owner-only, fixed 14-day window) --------------------------
const views = gh([`repos/${REPO}/traffic/views`], true);
const clones = gh([`repos/${REPO}/traffic/clones`], true);
checks.traffic = {
  window: 'last 14 days, GitHub-fixed buckets',
  views: views ? { count: views.count, uniques: views.uniques } : null,
  clones: clones ? { count: clones.count, uniques: clones.uniques } : null,
  caveat:
    'clone count includes every CI checkout of this repo; not an adoption signal',
  ok: Boolean(views && clones),
};

// ---- 5. referrers ----------------------------------------------------------
const referrers = gh([`repos/${REPO}/traffic/popular/referrers`], true) || [];
const externalReferrers = referrers.filter(
  (r) => r.referrer && !/(^|\.)github\.com$/i.test(r.referrer)
);
checks.referrers = {
  all: referrers.map((r) => ({ referrer: r.referrer, count: r.count, uniques: r.uniques })),
  external: externalReferrers.map((r) => r.referrer),
  ok: true,
};

// ---- 6. placement channel, fetched live ------------------------------------
let placement;
if (PLACEMENT_PR > 0) {
  const pr = gh([`repos/${UPSTREAM}/pulls/${PLACEMENT_PR}`], true);
  if (pr) {
    placement = {
      upstream: UPSTREAM,
      pr: pr.number,
      title: pr.title,
      state: pr.state,
      merged: Boolean(pr.merged),
      mergedBy: pr.merged_by ? pr.merged_by.login : null,
      mergedAt: pr.merged_at,
      url: pr.html_url,
      fetchedAt: startedAt,
    };
  } else {
    placement = { upstream: UPSTREAM, pr: PLACEMENT_PR, state: 'unverifiable' };
  }
} else {
  placement = { upstream: UPSTREAM, pr: null, state: 'none-open' };
}

// ---- 7. classification -----------------------------------------------------
const userOneSignals = [
  ...starLogins.map((l) => `star by ${l}`),
  ...forkLogins.map((l) => `fork by ${l}`),
  ...externalIssues.map((i) => `${i.pull_request ? 'PR' : 'issue'} #${i.number} by ${i.user.login}`),
];
let verdict;
if (userOneSignals.length > 0) {
  verdict = {
    class: 'user-one-evidence',
    signals: userOneSignals,
    meaning: 'at least one non-owner account touched the repo',
  };
} else if (placement.merged) {
  verdict = {
    class: 'distributed-not-yet-consumed',
    signals: [`placement ${UPSTREAM}#${PLACEMENT_PR} merged by ${placement.mergedBy}`],
    meaning:
      'the channel delivered the repo to list readers; consumption is still unproven — do not read a merge as adoption',
  };
} else {
  verdict = {
    class: 'not-distributed-not-consumed',
    signals: [
      `placement ${UPSTREAM}#${PLACEMENT_PR} state=${placement.state}`,
      'zero non-owner stars, forks, issues, PRs',
    ],
    meaning: 'the candidate channel is open but nothing has been consumed',
  };
}

// ---- report ----------------------------------------------------------------
const failedSources = failures.length;
const report = {
  measuredAt: startedAt,
  repo: REPO,
  checks,
  placement,
  verdict,
  dataSourcesFailed: failedSources,
  complete: failedSources === 0 && checks.repoMetadata.ok && checks.traffic.ok,
};

const lines = [];
lines.push(`adoption measurement ${REPO} @ ${startedAt}`);
lines.push(`  repo: public=${checks.repoMetadata.public} license=${checks.repoMetadata.license}`);
lines.push(
  `  stars=${checks.starForkCensus.starsTotal} (non-owner: ${checks.starForkCensus.nonOwnerStars.length}) forks=${checks.starForkCensus.forksTotal} (non-owner: ${checks.starForkCensus.nonOwnerForks.length})`
);
lines.push(`  external issues/PRs: ${checks.issueCensus.external.length}`);
lines.push(
  `  traffic 14d: views=${checks.traffic.views ? checks.traffic.views.count : '?'} uniques=${checks.traffic.views ? checks.traffic.views.uniques : '?'} clones=${checks.traffic.clones ? checks.traffic.clones.count : '?'} (CI-inflated)`
);
lines.push(`  external referrers: ${checks.referrers.external.length ? checks.referrers.external.join(', ') : 'none'}`);
lines.push(
  `  placement: ${UPSTREAM}#${placement.pr} state=${placement.state}${placement.merged ? ` MERGED by ${placement.mergedBy}` : ''}`
);
if (failedSources || !checks.repoMetadata.ok || !checks.traffic.ok) {
  lines.push(
    `  NO VERDICT — incomplete evidence (${failedSources} data source failure(s)` +
      `${!checks.repoMetadata.ok ? '; repo metadata not as claimed' : ''}` +
      `${!checks.traffic.ok ? '; traffic unavailable' : ''}):`
  );
  for (const f of failures) lines.push(`    ! ${f}`);
  lines.push('    (partial censuses above are context only, not an adoption claim)');
} else {
  lines.push(`  VERDICT: ${verdict.class}`);
  for (const s of verdict.signals) lines.push(`    - ${s}`);
}
console.log(lines.join('\n'));

if (OUT) {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2) + '\n');
  console.log(`report written: ${OUT}`);
}
if (!report.complete) process.exit(3);
