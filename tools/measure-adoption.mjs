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
//  9. distribution surfaces — everything the distribution lane does OFF
//     this repo, which the census above is blind to: the measured evidence
//     posted on foreign threads (helius-labs/laserstream-sdk#115), the
//     placement PRs (#93, #280) with their review/comment state, and the
//     public measurement harness repo. A foreign reply that arrives after
//     our first comment is the earliest possible reaction signal — it is
//     classified as FIELD-USE OF THE MEASUREMENT, never as reeflog
//     consumption, unless the reply itself mentions reeflog or links an
//     auroraxo repo (then it is a foreign reeflog reaction, still not
//     user-one: that class stays repo-scoped to stars/forks/usage-issues).
//  7. npm downloads (the distribution card requires weekly npm numbers):
//     recorded WITH their attribution — npm counts every install and this
//     project's own CI installs the client on every run (client.test,
//     npm-self-dependency, registry-channel, live registry harness), so
//     downloads are owner-context, NEVER adoption. The structural guard:
//     downloads before the first publish must be 0, the owner's CI run
//     count for the same 7-day window is recorded next to the downloads,
//     and the verdict classes never read a download.
//  8. Classification, derived only from 1-6:
//       user-one evidence : any non-owner star/fork, or a non-owner
//           issue/PR that is USAGE evidence — unsolicited outreach and
//           invitation issues are classified separately and never count
//       distributed, not yet consumed: placement merged, but zero
//           non-owner signals
//       not distributed, not consumed: placement still open/none
//
// Exit codes: 0 = measurement complete (any verdict), 3 = a data source
// failed or looked cached — no verdict is issued on partial evidence.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { classifyExternalIssue, splitIssuesByUsage } from './adoption-classify.mjs';

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

// ---- 3. issues and PRs, owner excluded, outreach separated ----------------
// GitHub's issues list includes PRs; both are outsider evidence — but NOT
// every outsider issue is a consumer. PULS AH caught this tool flipping to
// user-one-evidence on issue #2 (webbeep, 2026-11-03): an unsolicited
// outreach/invitation issue from a foreign agent promoting its own service.
// The author never used reeflog; the [bot]-suffix filter did not catch it
// because such senders omit the suffix. An invitation is marketing traffic,
// not consumption, so it is classified apart and NEVER counts as a signal.
//
// classifyExternalIssue (imported from ./adoption-classify.mjs, unit-pinned
// in test/adoption-outreach-classification.test.mjs) is deliberately
// conservative: it only excludes an issue when the text itself identifies
// it as an outreach/invitation. Genuine bug reports, feature requests and
// PRs keep counting.
const issues = gh([`repos/${REPO}/issues?state=all&per_page=100`]) || [];
const outsiderIssues = issues.filter(
  (i) => i.user && i.user.login !== OWNER && !i.user.login.endsWith('[bot]')
);
const { usage: usageIssues, outreach: outreachIssues } = splitIssuesByUsage(outsiderIssues);
const externalIssues = usageIssues; // only usage issues are consumer evidence
checks.issueCensus = {
  total: issues.length,
  external: usageIssues,
  outreachExcluded: outreachIssues,
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

// ---- 7. npm downloads, attributed (owner harness inflates them) ------------
function curlJson(url, optional = false) {
  try {
    return JSON.parse(
      execFileSync('curl', ['-sf', '--max-time', '30', url], { encoding: 'utf8' })
    );
  } catch (err) {
    const msg = `npm downloads api ${url} failed: ${String(err.message).split('\n')[0]}`;
    if (optional) {
      failures.push(msg);
      return null;
    }
    console.error('FATAL ' + msg);
    process.exit(3);
  }
}

const NPM_PKG = process.env.ADOPTION_NPM_PKG || 'reeflog-client';
const firstPublishAt = JSON.parse(
  execFileSync('npm', ['view', NPM_PKG, 'time.created', '--json'], {
    encoding: 'utf8',
    timeout: 60000,
  })
);
const today = new Date().toISOString().slice(0, 10);
const rangeFrom = new Date(Date.parse(firstPublishAt) - 14 * 86400e3).toISOString().slice(0, 10);
const npmWeek = curlJson(`https://api.npmjs.org/downloads/point/last-week/${NPM_PKG}`, true);
const npmRange = curlJson(
  `https://api.npmjs.org/downloads/range/${rangeFrom}:${today}/${NPM_PKG}`,
  true
);
let downloadsBeforeFirstPublish = null;
let byDay = null;
if (npmRange) {
  byDay = npmRange.downloads.map((d) => ({ day: d.day, downloads: d.downloads }));
  downloadsBeforeFirstPublish = byDay
    .filter((d) => d.day < firstPublishAt.slice(0, 10))
    .reduce((s, d) => s + d.downloads, 0);
  if (downloadsBeforeFirstPublish !== 0) {
    failures.push(
      `npm downloads before first publish (${firstPublishAt}) = ${downloadsBeforeFirstPublish} — the package name was in use before this project; attribution is broken, no verdict`
    );
  }
}

// Owner CI runs over the same trailing 7 days: the lower bound of the
// harness activity that legitimately produces npm installs.
const OWNER_REPOS = (
  process.env.ADOPTION_OWNER_REPOS ||
  'auroraxo/reeflog,auroraxo/sememe,auroraxo/signerd,auroraxo/whirlpool-watcher'
).split(',');
const runWindowStart = new Date(Date.now() - 7 * 86400e3).toISOString();
const ownerCiRuns = OWNER_REPOS.map((r) => r.trim()).map((repo) => {
  let page = 1;
  let total = 0;
  let capped = false;
  for (;;) {
    let dates;
    try {
      // --jq projects to timestamps only: the full run objects exceed
      // execFileSync's default 1 MB buffer (ENOBUFS on page 1).
      dates = JSON.parse(
        execFileSync(
          'gh',
          [
            'api',
            `repos/${repo}/actions/runs?per_page=100&page=${page}`,
            '--jq',
            '[.workflow_runs[].created_at]',
          ],
          { encoding: 'utf8', timeout: 60000, maxBuffer: 16 * 1024 * 1024 }
        )
      );
    } catch (err) {
      failures.push(
        `owner ci runs for ${repo} (page ${page}) failed: ${String(err.message).split('\n')[0]}`
      );
      return { repo, error: true };
    }
    const inWindow = dates.filter((d) => d >= runWindowStart);
    total += inWindow.length;
    if (dates.length < 100 || page >= 5) {
      capped = dates.length >= 100 && page >= 5;
      break;
    }
    page += 1;
  }
  return { repo, runs: total, since: runWindowStart, ...(capped ? { capped } : {}) };
});

checks.npmDownloads = {
  package: NPM_PKG,
  registry: 'api.npmjs.org/downloads',
  firstPublishAt,
  lastWeek: npmWeek
    ? { downloads: npmWeek.downloads, window: `${npmWeek.start}..${npmWeek.end}` }
    : null,
  byDay,
  downloadsBeforeFirstPublish,
  countsAsAdoption: false,
  meaning:
    'npm counts every install; this project’s own CI installs the package (client.test, npm-self-dependency, registry-channel, live registry harness) — downloads measure OWNER ACTIVITY unless a foreign consumer is proven elsewhere; adoption signals stay stars/forks/usage-issues/referrers',
  ownerCiRunsSameWeek: ownerCiRuns,
  ok: Boolean(npmWeek && npmRange && downloadsBeforeFirstPublish === 0),
};

// ---- 8. placement channel, fetched live ------------------------------------
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

// ---- 9. distribution surfaces: the outbound action, reacted to ------------
// Configured surfaces (env-overridable); each is "owner/repo#number".
const DIST_SURFACES = (
  process.env.ADOPTION_DIST_SURFACES || 'helius-labs/laserstream-sdk#115'
)
  .split(',')
  .map((s) => s.trim())
  .filter(Boolean);
const HARNESS_REPO = process.env.ADOPTION_HARNESS_REPO || 'auroraxo/laserstream-cursor-measure';

const distributionSurfaces = [];
for (const spec of DIST_SURFACES) {
  const m = spec.match(/^(.+?)#(\d+)$/);
  if (!m) {
    failures.push(`distribution surface ${spec} is not owner/repo#number`);
    continue;
  }
  const [, where, nStr] = m;
  const n = Number(nStr);
  const comments = gh([`repos/${where}/issues/${n}/comments?per_page=100`], true);
  if (!comments) {
    distributionSurfaces.push({ surface: spec, state: 'unverifiable' });
    continue;
  }
  const presented = comments
    .filter((c) => c.user && c.user.login === OWNER && c.created_at)
    .map((c) => c.created_at)
    .sort()[0] || null;
  // Emoji reactions on the owner's comments: another reaction channel a
  // reply-only view would miss.
  const ownerCommentIds = comments
    .filter((c) => c.user && c.user.login === OWNER)
    .map((c) => c.id);
  let reactionsOnOwnerComments = 0;
  if (presented !== null) {
    for (const id of ownerCommentIds) {
      const rs = gh([`repos/${where}/issues/comments/${id}/reactions`], true);
      if (rs === null) {
        reactionsOnOwnerComments = null;
        break;
      }
      reactionsOnOwnerComments += rs.length;
    }
  }
  const foreignReplies = comments.filter(
    (c) =>
      presented &&
      c.user &&
      c.user.login !== OWNER &&
      !c.user.login.endsWith('[bot]') &&
      c.created_at > presented
  );
  const replies = foreignReplies.map((c) => ({
    author: c.user.login,
    createdAt: c.created_at,
    mentionsReeflog: /\breeflog\b/i.test(c.body || ''),
    mentionsAuroraxoRepo: /github\.com\/auroraxo\//i.test(c.body || ''),
    // 92..121 is the skip range this project measured on SDK main 379a6ba;
    // a foreign reply carrying both endpoints engaged with the numbers.
    quotesMeasuredSkip: /\b92\b[\s\S]*\b121\b/.test(c.body || ''),
  }));
  const reeflogMentions = replies.filter((r) => r.mentionsReeflog || r.mentionsAuroraxoRepo);
  let classification;
  const reasons = [];
  if (presented === null) {
    classification = 'no-presentation';
    reasons.push(`no-presentation: ${OWNER} has not commented on ${spec}`);
  } else if (replies.length === 0) {
    classification = 'no-visible-reaction';
    reasons.push(`no-visible-reaction: no foreign non-bot reply after ${presented} on ${spec}`);
  } else if (reeflogMentions.length > 0) {
    classification = 'foreign-reeflog-reaction';
    reasons.push(
      `foreign-reeflog-reaction: ${reeflogMentions.map((r) => r.author).join(', ')} mentioned reeflog or an auroraxo repo link on ${spec}`
    );
  } else {
    classification = 'measurement-field-use-not-reeflog-consumption';
    reasons.push(`thread-is-foreign: ${where} is not owned by ${OWNER}`);
    reasons.push(
      `no-reeflog-mention: ${replies.length} foreign reply/replies after presentation mention neither reeflog nor a github.com/auroraxo/ link`
    );
    if (replies.some((r) => r.quotesMeasuredSkip)) {
      reasons.push(
        'content-engagement: the reply quotes the measured skip range 92..121, i.e. it engages with the evidence, not with reeflog'
      );
    }
  }
  distributionSurfaces.push({
    surface: spec,
    kind: 'issue-thread',
    ownerPresentedAt: presented,
    ownerComments: comments.filter((c) => c.user && c.user.login === OWNER).length,
    reactionsOnOwnerComments,
    commentsTotal: comments.length,
    nonOwnerRepliesAfter: replies,
    classification,
    reasons,
  });
}

// The public harness: stars/forks there are harness adoption, recorded
// separately so they can never leak into the reeflog user-one class.
const harness = gh([`repos/${HARNESS_REPO}`], true);
const harnessStars = harness ? gh([`repos/${HARNESS_REPO}/stargazers?per_page=100`], true) || [] : [];
const harnessForks = harness ? gh([`repos/${HARNESS_REPO}/forks?per_page=100`], true) || [] : [];
const harnessRepo = harness
  ? {
      repo: HARNESS_REPO,
      public: !harness.private,
      stars: harness.stargazers_count,
      forks: harness.forks_count,
      nonOwnerStars: harnessStars
        .filter((s) => s.login && s.login !== OWNER)
        .map((s) => s.login),
      nonOwnerForks: harnessForks
        .filter((f) => f.owner && f.owner.login !== OWNER)
        .map((f) => f.owner.login),
      countsAsReeflogAdoption: false,
    }
  : { repo: HARNESS_REPO, state: 'unverifiable' };

// Placement PRs: both listing channels, with review/comment state — 0
// reviews is a measured fact, not a remembered one.
const PLACEMENT_SPECS = (
  process.env.ADOPTION_PLACEMENTS ||
  'StockpileLabs/awesome-solana-oss#93,solana-foundation/awesome-solana-ai#280'
)
  .split(',')
  .map((s) => s.trim())
  .filter(Boolean);
const placements = [];
for (const spec of PLACEMENT_SPECS) {
  const m = spec.match(/^(.+?)#(\d+)$/);
  if (!m) {
    failures.push(`placement ${spec} is not owner/repo#number`);
    continue;
  }
  const [, up, nStr] = m;
  const pr = gh([`repos/${up}/pulls/${Number(nStr)}`], true);
  if (!pr) {
    placements.push({ upstream: up, pr: Number(nStr), state: 'unverifiable' });
    continue;
  }
  const reviews = gh([`repos/${up}/pulls/${pr.number}/reviews`], true);
  const comments = gh([`repos/${up}/issues/${pr.number}/comments?per_page=100`], true);
  placements.push({
    upstream: up,
    pr: pr.number,
    title: pr.title,
    state: pr.state,
    merged: Boolean(pr.merged),
    mergedBy: pr.merged_by ? pr.merged_by.login : null,
    mergedAt: pr.merged_at,
    url: pr.html_url,
    reviews: reviews ? reviews.length : null,
    reviewsByOthers: reviews
      ? reviews.filter((r) => r.user && r.user.login !== OWNER).length
      : null,
    commentsByOthers: comments
      ? comments.filter((c) => c.user && c.user.login !== OWNER && !c.user.login.endsWith('[bot]')).length
      : null,
    fetchedAt: startedAt,
  });
}

const distVerdict = (() => {
  const reacted = distributionSurfaces.filter(
    (s) => s.classification === 'foreign-reeflog-reaction' || s.classification === 'measurement-field-use-not-reeflog-consumption'
  );
  const reeflogReactions = distributionSurfaces.filter((s) => s.classification === 'foreign-reeflog-reaction');
  if (reeflogReactions.length > 0) {
    return {
      class: 'foreign-reeflog-reaction',
      signals: reeflogReactions.map(
        (s) => `foreign reply on ${s.surface} mentions reeflog: ${s.nonOwnerRepliesAfter.filter((r) => r.mentionsReeflog || r.mentionsAuroraxoRepo).map((r) => r.author).join(', ')}`
      ),
      meaning:
        'a foreign account brought reeflog up on a distribution surface — follow up as a signal, but the user-one class stays repo-scoped (stars/forks/usage-issues on the reeflog repo)',
      userOneEvidence: false,
    };
  }
  if (reacted.length > 0) {
    const signalParts = reacted.map((s) => {
      const first = s.nonOwnerRepliesAfter[0];
      return `${s.nonOwnerRepliesAfter.length} foreign reply/replies after presentation on ${s.surface} (${first.author} at ${first.createdAt}) — measurement field-use, NOT reeflog adoption`;
    });
    return {
      class: 'measurement-field-use-zero-reeflog-consumption',
      signals: signalParts,
      meaning:
        'the measured evidence was read and answered off-repo; reeflog itself was not mentioned, no foreign star/fork/usage-issue exists — PUBLISH != CONSUMPTION still holds',
      userOneEvidence: false,
    };
  }
  return {
    class: 'no-visible-reaction',
    signals: ['no foreign reply after presentation on any distribution surface'],
    meaning: 'the outbound action is live but nothing has come back — still zero signals',
    userOneEvidence: false,
  };
})();

const distributionSurfacesBlock = {
  measuredAt: startedAt,
  surfaces: distributionSurfaces,
  harnessRepo,
  placements,
  reeflogCensusAtMeasurement: {
    nonOwnerStars: starLogins.length,
    nonOwnerForks: forkLogins.length,
    usageIssues: usageIssues.length,
  },
  verdict: distVerdict,
  ok: distributionSurfaces.every((s) => s.state !== 'unverifiable') && placements.every((p) => p.state !== 'unverifiable') && harnessRepo.state !== 'unverifiable',
};
if (!distributionSurfacesBlock.ok) {
  failures.push('distribution surfaces not fully verifiable — no verdict on partial evidence');
}

// ---- 10. classification ----------------------------------------------------
const userOneSignals = [
  ...starLogins.map((l) => `star by ${l}`),
  ...forkLogins.map((l) => `fork by ${l}`),
  ...usageIssues.map((i) => `${i.isPR ? 'PR' : 'issue'} #${i.n} by ${i.author}`),
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
const npmOk = Boolean(checks.npmDownloads.ok);
const report = {
  measuredAt: startedAt,
  repo: REPO,
  checks,
  placement,
  distributionSurfaces: distributionSurfacesBlock,
  verdict,
  dataSourcesFailed: failedSources,
  complete: failedSources === 0 && checks.repoMetadata.ok && checks.traffic.ok && npmOk,
};

const lines = [];
lines.push(`adoption measurement ${REPO} @ ${startedAt}`);
lines.push(`  repo: public=${checks.repoMetadata.public} license=${checks.repoMetadata.license}`);
lines.push(
  `  stars=${checks.starForkCensus.starsTotal} (non-owner: ${checks.starForkCensus.nonOwnerStars.length}) forks=${checks.starForkCensus.forksTotal} (non-owner: ${checks.starForkCensus.nonOwnerForks.length})`
);
lines.push(
  `  external issues/PRs: ${checks.issueCensus.external.length}` +
    (checks.issueCensus.outreachExcluded.length
      ? ` (+${checks.issueCensus.outreachExcluded.length} outreach excluded: ` +
        checks.issueCensus.outreachExcluded.map((i) => `#${i.n} ${i.author}`).join(', ') + ')'
      : '')
);
lines.push(
  `  traffic 14d: views=${checks.traffic.views ? checks.traffic.views.count : '?'} uniques=${checks.traffic.views ? checks.traffic.views.uniques : '?'} clones=${checks.traffic.clones ? checks.traffic.clones.count : '?'} (CI-inflated)`
);
lines.push(`  external referrers: ${checks.referrers.external.length ? checks.referrers.external.join(', ') : 'none'}`);
lines.push(
  `  npm ${NPM_PKG}: last-week=${checks.npmDownloads.lastWeek ? checks.npmDownloads.lastWeek.downloads : '?'} (window ${checks.npmDownloads.lastWeek ? checks.npmDownloads.lastWeek.window : '?'}, first publish ${firstPublishAt}) — owner CI runs same week: ${ownerCiRuns.map((r) => `${r.repo}=${r.runs ?? 'err'}`).join(' ')}; downloads are NOT adoption`
);
lines.push(
  `  placement: ${UPSTREAM}#${placement.pr} state=${placement.state}${placement.merged ? ` MERGED by ${placement.mergedBy}` : ''}`
);
for (const s of distributionSurfaces) {
  lines.push(
    `  surface ${s.surface}: owner presented ${s.ownerPresentedAt ?? 'n/a'}, ${s.ownerComments} owner comment(s), ${s.nonOwnerRepliesAfter.length} foreign reply/replies after presentation -> ${s.classification}`
  );
}
for (const p of placements) {
  lines.push(
    `  placement ${p.upstream}#${p.pr}: state=${p.state}${p.merged ? ' MERGED' : ''}, reviews=${p.reviews ?? '?'} (others ${p.reviewsByOthers ?? '?'}), foreign comments=${p.commentsByOthers ?? '?'}`
  );
}
lines.push(
  `  harness ${harnessRepo.repo}: public=${harnessRepo.public} stars=${harnessRepo.stars ?? '?'} (non-owner ${harnessRepo.nonOwnerStars ? harnessRepo.nonOwnerStars.length : '?'}) forks=${harnessRepo.forks ?? '?'} (non-owner ${harnessRepo.nonOwnerForks ? harnessRepo.nonOwnerForks.length : '?'}) — harness adoption, never reeflog adoption`
);
if (distributionSurfacesBlock.ok) {
  lines.push(`  DISTRIBUTION VERDICT: ${distributionSurfacesBlock.verdict.class}`);
  for (const s of distributionSurfacesBlock.verdict.signals) lines.push(`    - ${s}`);
}
if (failedSources || !checks.repoMetadata.ok || !checks.traffic.ok || !npmOk) {
  lines.push(
    `  NO VERDICT — incomplete evidence (${failedSources} data source failure(s)` +
      `${!checks.repoMetadata.ok ? '; repo metadata not as claimed' : ''}` +
      `${!checks.traffic.ok ? '; traffic unavailable' : ''}` +
      `${!npmOk ? '; npm downloads unavailable or attribution broken' : ''}):`
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
