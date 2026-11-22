// `npm run size:ci`: the size measurement CI runs (web.yml), step for step, so the two can't
// drift (X-size-ci-script). Arguments go to size-limit (`npm run size:ci -- --json`).
//   1. the shipped build, as `npm run build`;
//   2. the measurement rebuild: a fixed version name (the commit SHA otherwise changes the app id
//      and every hash) and names from each chunk's or stylesheet's sources, not its bytes
//      (LODESTAR_STABLE_NAMES, vite.config.ts), so an unrelated change doesn't move other sizes;
//   3. size-limit on that rebuild.
// Stops at the first step that fails, with its exit code.
import { spawnSync } from 'node:child_process';

const steps = [
	{ args: ['vite', 'build'], env: {} },
	{
		args: ['vite', 'build'],
		env: { LODESTAR_BUILD_VERSION: '0000000', LODESTAR_STABLE_NAMES: '1' }
	},
	{ args: ['size-limit', ...process.argv.slice(2)], env: {} }
];

for (const { args, env } of steps) {
	const run = spawnSync('npx', ['--no-install', ...args], {
		stdio: 'inherit',
		env: { ...process.env, ...env },
		// npx is a .cmd on Windows, which needs a shell to start.
		shell: process.platform === 'win32'
	});
	if (run.error) throw run.error;
	if (run.status !== 0) process.exit(run.status ?? 1);
}
