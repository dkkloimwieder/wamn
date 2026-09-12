#!/usr/bin/env bash
# Read retained logs only. JSON goes to stdout; exit 0 means parsing, not passing tests.
set -euo pipefail
[[ $# == 1 && "$1" =~ ^(integration-[0-9]{3}|baseline)$ ]] || {
    printf 'usage: bash compare-workspace.sh {integration-NNN|baseline}\n' >&2
    exit 2
}
evidence_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
node - "$evidence_root" "$1" <<'NODE'
const fs = require('fs'), path = require('path'), crypto = require('crypto');
const [directory, run] = process.argv.slice(2);
const baseline = JSON.parse(fs.readFileSync(path.join(directory, 'workspace-baseline.json'), 'utf8'));
const root = run === 'baseline' ? path.resolve(directory, '../ctc8-15-1-identity/integration-001') : path.join(directory, run);
if (!fs.existsSync(root) || fs.lstatSync(root).isSymbolicLink()) throw Error('missing or symlinked evidence directory');
const read = name => {
  const file = path.join(root, name);
  if (!fs.existsSync(file)) return null;
  if (!fs.lstatSync(file).isFile()) throw Error('evidence input is not a regular file');
  return fs.readFileSync(file, 'utf8');
};
const code = name => { const s = read(name)?.trim(); return /^\d+$/.test(s ?? '') ? Number(s) : null; };
const workspaceExit = code('workspace.exit'), captureExit = code('exit');
const log = read('workspace.log'), command = read('workspace.command');
const output = {run, evidence_directory: root, source: (read('source.base') ?? read('source-head'))?.trim() ?? null,
  after_source: read('after-source.base')?.trim() ?? null, command: command?.trimEnd() ?? null,
  workspace_exit: workspaceExit, capture_exit: captureExit, capture_complete: captureExit !== null,
  baseline_source: baseline.source.commit, limits: [
    'This reducer does not run tests or decide acceptance. Its exit 0 means the evidence was parsed.',
    'It uses the final libtest summary per Running or Doc-tests block and reports earlier nested summaries separately.',
    'Baseline failures not reported here are not automatically fixed: their tests can be absent, filtered, renamed, or self-skipped.',
    'A new failed name is not automatically a production regression; its raw assertion still needs classification.',
    'Passing stdout is hidden without --nocapture. No absence of visible skip output proves live execution.',
    'Package identity for failed targets comes from Cargo rerun selectors, never from guessed binary names.']};
const emit = () => process.stdout.write(JSON.stringify(output, null, 2) + '\n');
if (workspaceExit === null || log === null || command === null) {
  Object.assign(output, {workspace_status: 'incomplete', counts: null, comparison: null});
  emit(); process.exit(0);
}
const lines = log.split('\n'), sections = [], issues = [], excluded = [];
let section;
for (let i = 0; i < lines.length; i++) {
  const running = lines[i].match(/^\s+Running (.+) \(([^()]+)\)$/);
  const doc = lines[i].match(/^\s+Doc-tests (\S+)$/);
  if (!running && !doc) continue;
  if (section) section.end = i;
  section = {kind: running ? 'test' : 'doctest', start: i + 1, end: lines.length,
    description: running ? running[1] : doc[1], binary: running ? running[2] : null};
  sections.push(section);
}
const counts = {test_targets: 0, failed_test_targets: 0, passed_tests: 0, failed_tests: 0, ignored_tests: 0,
  doctest_targets: 0, failed_doctest_targets: 0, passed_doctests: 0, failed_doctests: 0, ignored_doctests: 0};
const targets = [];
for (const s of sections) {
  const segment = lines.slice(s.start - 1, s.end);
  const summaries = segment.flatMap((line, i) => {
    const m = line.match(/^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;/);
    return m ? [{line: s.start + i, result: m[1], passed: +m[2], failed: +m[3], ignored: +m[4], filtered_out: +m[6]}] : [];
  });
  const summary = summaries.at(-1), suffix = s.kind === 'test' ? 'tests' : 'doctests';
  counts[s.kind === 'test' ? 'test_targets' : 'doctest_targets']++;
  if (!summary) { issues.push({line: s.start, issue: 'target has no final summary'}); continue; }
  for (const nested of summaries.slice(0, -1)) excluded.push({parent_running_line: s.start, parent_summary_line: summary.line, summary: nested});
  for (const field of ['passed','failed','ignored']) counts[`${field}_${suffix}`] += summary[field];
  if (!summary.failed) continue;
  counts[s.kind === 'test' ? 'failed_test_targets' : 'failed_doctest_targets']++;
  const rerunIndex = segment.findIndex(l => /^error: (?:test|doctest) failed, to rerun pass `-p /.test(l));
  const rerun = rerunIndex < 0 ? null : segment[rerunIndex].match(/`(-p (\S+) (.+))`$/);
  if (!rerun) { issues.push({line: s.start, issue: 'failed target lacks an explicit Cargo package selector'}); continue; }
  const names = new Map();
  segment.forEach((line, i) => { const m = line.match(/^test (.+) \.\.\. FAILED$/); if (m) names.set(m[1], s.start + i); });
  const beforeSummary = segment.slice(0, summary.line - s.start);
  const failureList = beforeSummary.lastIndexOf('failures:');
  if (failureList >= 0) beforeSummary.slice(failureList + 1).forEach((line, i) => {
    if (/^    \S/.test(line) && !names.has(line.trim())) names.set(line.trim(), s.start + failureList + 1 + i);
  });
  if (names.size !== summary.failed) issues.push({line: summary.line, issue: 'named failures disagree with summary'});
  targets.push({package: rerun[2], target: rerun[3], selector: rerun[1], rerun_line: s.start + rerunIndex, kind: s.kind,
    running: {line: s.start, description: s.description, binary_path: s.binary}, summary,
    failed_tests: [...names].map(([name, line]) => ({name, line}))});
}
const footerIndex = lines.findIndex(l => /^error: \d+ targets failed:$/.test(l));
const footer = footerIndex < 0 ? [] : lines.slice(footerIndex + 1).flatMap(l => { const m = l.match(/^    `(-p \S+ .+)`$/); return m ? [m[1]] : []; });
if ((targets.length || footer.length) && (footerIndex < 0 || JSON.stringify(footer.slice().sort()) !== JSON.stringify(targets.map(t => t.selector).sort())))
  issues.push({line: footerIndex < 0 ? null : footerIndex + 1, issue: 'failed target footer is absent or differs from parsed targets'});
if (footerIndex >= 0 && +lines[footerIndex].match(/\d+/)[0] !== footer.length)
  issues.push({line: footerIndex + 1, issue: 'failed target footer count differs'});
const compileErrors = lines.flatMap((line, i) => /^error(?:\[E\d+\]:|: could not compile|: linking with)/.test(line) ? [i + 1] : []);
if (!sections.length && workspaceExit === 0) issues.push({line: null, issue: 'successful exit has no test targets'});
if (workspaceExit === 0 && (targets.length || compileErrors.length)) issues.push({line: null, issue: 'successful exit conflicts with failures'});
const key = item => JSON.stringify([item.package, item.target, item.name]);
const flatten = ts => ts.flatMap(t => t.failed_tests.map(test => ({package: t.package, target: t.target, name: test.name})));
const previous = flatten(baseline.failed_targets), current = flatten(targets);
const previousKeys = new Set(previous.map(key)), currentKeys = new Set(current.map(key));
Object.assign(output, {workspace_status: compileErrors.length ? 'compile_failure' : issues.length ? 'unclassified' :
  targets.length ? 'test_failure' : workspaceExit === 0 ? 'passed' : 'command_failure', counts,
  log_sha256: crypto.createHash('sha256').update(log).digest('hex'), compiler_error_lines: compileErrors,
  excluded_nested_summaries: excluded, parser_issues: issues, failed_targets: targets,
  comparison: {complete: !issues.length && !compileErrors.length && sections.length > 0 && (workspaceExit === 0 || targets.length > 0),
    new_failures: current.filter(f => !previousKeys.has(key(f))),
    repeated_failures: current.filter(f => previousKeys.has(key(f))),
    baseline_failures_not_reported: previous.filter(f => !currentKeys.has(key(f)))}});
if (run === 'baseline' && (issues.length || current.length !== baseline.counts.failed_tests || output.comparison.new_failures.length || output.comparison.baseline_failures_not_reported.length))
  throw Error('retained baseline self-check failed');
emit();
NODE
