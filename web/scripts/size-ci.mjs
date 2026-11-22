// `npm run size:ci`: the size measurement CI runs (web.yml), step for step, so the two can't
// drift (X-size-ci-script). Arguments go to size-limit (`npm run size:ci -- --json`).
//   1. `npm run build`, the shipped build, with its prebuild (themes and their contrast gates,
//      brand assets, messages), so the measurement is of freshly generated files;
//   2. the measurement rebuild: a fixed version name (the commit SHA otherwise changes the app id
//      and every hash) and names from each chunk's or stylesheet's sources, not its bytes
//      (LODESTAR_STABLE_NAMES, vite.config.ts), so an unrelated change doesn't move other sizes;
//   3. size-limit on that rebuild.
// The build steps' output goes to stderr, so size-limit's stdout (`--json`) stays clean. Every
// step runs on this Node with its script file, not through a shell, so it behaves the same on
// Windows. Stops at the first step that fails, with its exit code.
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

const require = createRequire(import.meta.url);
const bin = (pkg, rel) => join(dirname(require.resolve(`${pkg}/package.json`)), rel);

const npm = process.env.npm_execpath;
if (!npm) throw new Error('size:ci: run it as `npm run size:ci`, which builds with npm');

const toStderr = ['ignore', process.stderr, process.stderr];
const steps = [
	{ args: [npm, 'run', 'build'], env: {}, stdio: toStderr },
	{
		args: [bin('vite', 'bin/vite.js'), 'build'],
		env: { LODESTAR_BUILD_VERSION: '0000000', LODESTAR_STABLE_NAMES: '1' },
		stdio: toStderr
	},
	{ args: [bin('size-limit', 'bin.js'), ...process.argv.slice(2)], env: {}, stdio: 'inherit' }
];

for (const { args, env, stdio } of steps) {
	const run = spawnSync(process.execPath, args, { stdio, env: { ...process.env, ...env } });
	if (run.error) throw run.error;
	if (run.status !== 0) process.exit(run.status ?? 1);
}
