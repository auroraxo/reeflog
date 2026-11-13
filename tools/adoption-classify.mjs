// Classification of outsider GitHub issues into USAGE evidence and OUTREACH.
//
// PULS AH (2026-11-13) caught the adoption probe flipping to
// user-one-evidence on issue #2 of auroraxo/reeflog: an unsolicited
// invitation from a foreign agent (webbeep / SloppableAI) that never used
// reeflog and only promoted its own service. The old filter excluded
// accounts whose login ends in "[bot]"; such senders simply omit the
// suffix, so the gate was flipped by marketing traffic.
//
// An invitation is traffic, not consumption. This module is the only place
// that decides, it is conservative (it excludes ONLY on explicit textual
// evidence of outreach), and it is unit-testable on its own.
'use strict';

const OUTREACH_TITLE = /\b(invite|outreach)\b/i;
const OUTREACH_BODY = [
  /this is opt-in\b/i,
  /close this issue\b.{0,120}\b(won'?t|will not)\b.{0,60}\b(contact|reach out|write)\b/i,
  /\bi['’]m writing to you, the agent\b/i,
  /\bthis is [A-Za-z0-9_.-]+, an (AI|autonomous) agent\b/i,
];

/**
 * @param {{title?: string, body?: string}} issue
 * @returns {{kind: 'usage'|'outreach', reasons: string[]}}
 */
function classifyExternalIssue(issue) {
  const reasons = [];
  const title = issue && issue.title ? issue.title : '';
  if (OUTREACH_TITLE.test(title)) reasons.push('title:invite/outreach');
  const body = issue && issue.body ? issue.body : '';
  for (const re of OUTREACH_BODY) {
    if (re.test(body)) reasons.push(`body:${re.source}`);
  }
  return reasons.length > 0 ? { kind: 'outreach', reasons } : { kind: 'usage', reasons: [] };
}

/**
 * Split outsider issues into usage evidence and excluded outreach.
 * @param {Array<{number:number, user:{login:string}, pull_request?:object, title:string, body?:string}>} outsiderIssues
 */
function splitIssuesByUsage(outsiderIssues) {
  const usage = [];
  const outreach = [];
  for (const i of outsiderIssues) {
    const c = classifyExternalIssue(i);
    const row = {
      n: i.number,
      author: i.user.login,
      isPR: Boolean(i.pull_request),
      title: i.title,
      ...(c.kind === 'outreach' ? { reasons: c.reasons } : {}),
    };
    (c.kind === 'outreach' ? outreach : usage).push(row);
  }
  return { usage, outreach };
}

export { classifyExternalIssue, splitIssuesByUsage, OUTREACH_TITLE, OUTREACH_BODY };
