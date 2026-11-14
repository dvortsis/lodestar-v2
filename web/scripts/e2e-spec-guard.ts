// Guard for web/playwright.config.ts: a spec that matches no Playwright project never runs, and
// nothing says so. Pure so scripts/e2e-spec-guard.test.ts can check it against near misses.

/** What the guard needs of a Playwright project. */
export interface GuardProject {
	name?: string;
	testMatch?: unknown;
	testIgnore?: unknown;
}

export interface SpecFile {
	/** Path relative to the test directory, with `/` separators: what Playwright matches on. */
	path: string;
	/** The file's text. */
	source: string;
}

/** The suffix that puts a real-server spec in the `fixture` projects. */
export const FIXTURE_SPEC = /\.fixture\.test\.ts$/;

/** Playwright's default `testMatch`: every file it would take for a spec. */
export const SPEC_FILE = /\.(spec|test)\.[cm]?[jt]sx?$/;

/** The projects that run the real server; a spec that needs it belongs to these and nothing else. */
const FIXTURE_PROJECTS = new Set(['fixture', 'fixture-writes']);

// Any way of reading the variable: `process.env.E2E_FIXTURE_URL`, `process.env['E2E_FIXTURE_URL']`,
// a destructure, or a helper that names it.
const READS_FIXTURE_URL = /E2E_FIXTURE_URL/;

const patterns = (value: unknown): RegExp[] =>
	[value].flat().filter((p): p is RegExp => p instanceof RegExp);

/** Does `project` run `path`: `testMatch` matches it and `testIgnore` does not. */
export function covers(project: GuardProject, path: string): boolean {
	return (
		patterns(project.testMatch).some((m) => m.test(path)) &&
		!patterns(project.testIgnore).some((m) => m.test(path))
	);
}

/** What is wrong with `files`, one line each; empty when every spec has a home. */
export function specProblems(files: SpecFile[], projects: GuardProject[]): string[] {
	const problems: string[] = [];
	for (const f of files) {
		if (!SPEC_FILE.test(f.path)) continue;
		const homes = projects.filter((p) => covers(p, f.path));
		if (!homes.length) problems.push(`${f.path}: matches no Playwright project, so it never runs`);
		else if (
			READS_FIXTURE_URL.test(f.source) &&
			homes.some((p) => !FIXTURE_PROJECTS.has(p.name ?? ''))
		)
			problems.push(
				`${f.path}: reads E2E_FIXTURE_URL, so name it *.fixture.test.ts (projects "fixture" and "fixture-writes" only; it also runs in ${homes
					.filter((p) => !FIXTURE_PROJECTS.has(p.name ?? ''))
					.map((p) => `"${p.name ?? '(unnamed)'}"`)
					.join(', ')})`
			);
	}
	return problems;
}
