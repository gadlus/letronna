// The review workflow that ships with Letronna: /review, or /review <a git ref
// to review against>. A reviewer reads each changed file's diff, or the whole
// of a file git does not track yet, three skeptics try to refute each thing it
// found, and what two of them could not refute goes into one report. The
// estimate is for a change of a few files with a finding or two each; a
// bigger change starts more helpers.
export const meta = {
  name: "review",
  description: "Review the changes in this project against HEAD or a ref: a reviewer per changed file, three votes to refute each finding, then one report",
  whenToUse: "Before committing or merging, when a change touches more than a file or two",
  phases: [{ title: "Find" }, { title: "Review" }, { title: "Verify" }, { title: "Report" }],
  estimate: { agents: 30 },
};

const LISTED = {
  type: "object",
  required: ["ran", "tip", "files", "added"],
  properties: {
    ran: { type: "boolean" },
    tip: { type: "boolean" },
    files: { type: "array", items: { type: "string" } },
    added: { type: "array", items: { type: "string" } },
  },
};
const FINDINGS = {
  type: "object",
  required: ["items"],
  properties: {
    items: {
      type: "array",
      items: {
        type: "object",
        required: ["line", "problem"],
        properties: { line: { type: "integer" }, problem: { type: "string" } },
      },
    },
  },
};
// A new file's reviewer can also say it left the file unread.
const ADDED = { ...FINDINGS, properties: { ...FINDINGS.properties, unread: { type: "boolean" } } };
const VOTE = {
  type: "object",
  required: ["real", "reason"],
  properties: { real: { type: "boolean" }, reason: { type: "string" } },
};
// The ref and the paths go into commands a helper runs, so only a ref's or a
// path's characters pass, and never a leading "-" git would take for an option.
const REF = /^\w[\w./~^-]*$/;
const PATH = /^[\w.][\w./-]*$/;
// Twenty new files is a big change already; past that, the likelier cause is
// a folder git should ignore, such as a build's output, which would otherwise
// start a reviewer for each of its files.
const MOST_NEW = 20;

// What the diffs are taken against: the words after /review, or the last
// commit, which covers staged and unstaged changes alike. They start where
// HEAD's history left that ref, its merge base, so on a branch the ref's newer
// commits do not show up reversed.
const asked = typeof args === "string" ? args.trim() : "";
if (asked && !REF.test(asked)) {
  return `Nothing was reviewed: ${JSON.stringify(asked)} is not a git ref to review against, such as main or HEAD~3.`;
}
const against = asked || "HEAD";

phase("Find");
const listed = await agent(
  `Run \`git diff --merge-base ${against} --name-only\` in this project and give the paths it lists that still exist, as they are, as files with tip false; if it fails, run \`git diff ${against} --name-only\` instead and give those with tip true. Then run \`git ls-files --others --exclude-standard\` and give the paths it lists as added. If a command is refused, or neither diff runs, give ran false and no paths; otherwise ran true. Change nothing and run nothing else.`,
  { type: "review", schema: LISTED, label: "changed files" },
);
// A helper that could not run git diff lists nothing, which must not read as
// a clean change.
if (!listed?.ran) {
  return `Nothing was reviewed: the files changed against ${against} could not be listed, as git diff did not run.`;
}
if (!listed.files.length && !listed.added.length) {
  return `No files changed against ${against}, so there was nothing to review.`;
}
const files = listed.files.filter(file => PATH.test(file));
const added = listed.added.filter(file => PATH.test(file));
const kept = added.slice(0, MOST_NEW);
const left = added.slice(MOST_NEW);
const diff = listed.tip ? `git diff ${against}` : `git diff --merge-base ${against}`;
log(`${files.length} changed and ${added.length} new files against ${against}`);

const RULES = "Report only real problems the change brings -- bugs, broken edge cases, security holes, missing error handling -- each with the line in the file as it is now. Style and naming are not problems. Do not run the tests or change anything. An empty list is a fine answer.";
const reviewing = [...files, ...kept];
const checked = await pipeline(reviewing,
  (file, _, index) => index < files.length
    ? agent(
      `Review the change to ${file}: run \`${diff} -- ${file}\` and read the code around it. ${RULES}`,
      { type: "review", schema: FINDINGS, label: file, phase: "Review" },
    )
    : agent(
      `Review the change to ${file}: it is new and git does not track it yet, so read all of it and review it as an addition. If it is not text or runs past 1,500 lines, review none of it and give unread true. ${RULES}`,
      { type: "review", schema: ADDED, label: file, phase: "Review" },
    ),
  (found, file) => found.unread ? { unread: true } : parallel(found.items.map(item => async () => {
    const votes = await parallel([1, 2, 3].map(n => () => agent(
      `Claim: ${file}, line ${item.line}, as the file is now: ${item.problem}\nTry to refute it: read the code, its callers and its tests. Say real only if the problem holds up.`,
      { type: "explore", schema: VOTE, label: `${file}:${item.line} #${n}`, phase: "Verify" },
    )));
    const upheld = votes.filter(vote => vote?.real).length;
    const answered = votes.filter(vote => vote).length;
    // Unless the votes that failed could not have upheld it, it is unchecked
    // rather than refuted.
    return { file, ...item, real: upheld >= 2, unchecked: upheld < 2 && upheld + 3 - answered >= 2 };
  })),
);

phase("Report");
const findings = checked.filter(Array.isArray).flat().filter(finding => finding);
const real = findings.filter(finding => finding.real);
// A reviewer that failed ends its file's pipeline with null.
const failed = reviewing.filter((file, index) => checked[index] === null);
const unread = reviewing.filter((file, index) => checked[index]?.unread);
const unchecked = findings.filter(finding => finding.unchecked).length;
const skipped = listed.files.length + listed.added.length - files.length - added.length;
// An output folder can leave hundreds out, more than a report should name.
const named = left.slice(0, MOST_NEW).join(", ") + (left.length > MOST_NEW ? ` and ${left.length - MOST_NEW} more` : "");
const gaps = [
  listed.tip && `Diffed against the tip of ${against}, as no merge base was found.`,
  failed.length && `No review came back for ${failed.length} of ${reviewing.length} files: ${failed.join(", ")}.`,
  unread.length && `${unread.length} new files were not read, as they are not text or run past 1,500 lines: ${unread.join(", ")}.`,
  left.length && `${left.length} new files were left out, as a review reads at most ${MOST_NEW}: ${named}.`,
  unchecked && `${unchecked} findings were neither upheld nor refuted, as their votes failed.`,
  skipped && `${skipped} changed files were left out, as their names are not plain paths.`,
].filter(Boolean);
if (!real.length) {
  const reviewed = reviewing.length - failed.length - unread.length;
  return [`Reviewed ${reviewed} of ${reviewing.length} changed files against ${against}: nothing found held up.`, ...gaps].join("\n");
}
const report = await agent(
  `Write a short code review of these problems, which survived three refutation attempts each, worst first, each with its file and line and what to change:\n${JSON.stringify(real)}`,
  { type: "explore", label: "report" },
);
return [report ?? `The report could not be written. What held up: ${JSON.stringify(real)}`, ...gaps].join("\n\n");
