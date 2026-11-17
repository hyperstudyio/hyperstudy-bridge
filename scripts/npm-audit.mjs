// CI gate for npm advisories: fails on any high or critical advisory that is not listed in IGNORED.
// `npm audit` has no ignore list of its own; this mirrors src-tauri/.cargo/audit.toml for the Rust side.
import { execFileSync } from 'node:child_process';

const IGNORED = {
  // picomatch ReDoS on single-branch repeated extglobs. Every published version is affected (<=4.0.7, the latest),
  // so there is nothing to upgrade to. It is here only through build tooling (vite, vitest, svelte-check and
  // typescript-eslint via tinyglobby/fdir), which globs this repository's own files; it is not in the built app.
  // Checked 2026-11-17. Remove this entry once a fixed picomatch is published.
  'GHSA-fc5q-39xc-vjpf': 'picomatch: no fixed version published',
};

let report;
try {
  report = execFileSync('npm', ['audit', '--json'], { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
} catch (error) {
  // npm audit exits non-zero when it finds anything; the report is still on stdout.
  report = error.stdout;
}
const { vulnerabilities } = JSON.parse(report);

const blocking = new Map();
for (const { via } of Object.values(vulnerabilities)) {
  for (const advisory of via) {
    if (typeof advisory === 'string') continue; // a dependency's advisory, listed under that dependency
    if (advisory.severity !== 'high' && advisory.severity !== 'critical') continue;
    const id = advisory.url.split('/').pop();
    if (!(id in IGNORED)) blocking.set(id, `${advisory.name}: ${advisory.title} (${advisory.url})`);
  }
}

for (const [id, reason] of Object.entries(IGNORED)) console.log(`Ignored ${id} - ${reason}`);
if (blocking.size) {
  console.error(`\n${blocking.size} high or critical npm advisories:`);
  for (const line of blocking.values()) console.error(`  ${line}`);
  process.exit(1);
}
console.log('No other high or critical npm advisories.');
