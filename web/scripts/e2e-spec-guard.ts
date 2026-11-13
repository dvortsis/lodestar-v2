// Guard for web/playwright.config.ts: a spec that matches no Playwright project never runs, and
// nothing says so. Pure so scripts/e2e-spec-guard.test.ts can check it against near misses.

export interface SpecFile {
	/** Path shown in the message. */
	path: string;
	/** Base name, which the projects' `testMatch` patterns are written against. */
	name: string;
	/** The file's text. */
	source: string;
}

/** The suffix that puts a real-server spec in the `fixture` project. */
export const FIXTURE_SPEC = /\.fixture\.test\.ts$/;

// Any way of reading the variable: `process.env.E2E_FIXTURE_URL`, `process.env['E2E_FIXTURE_URL']`,
// a destructure, or a helper that names it.
const READS_FIXTURE_URL = /E2E_FIXTURE_URL/;

/** What is wrong with `files`, one line each; empty when every spec has a project. */
export function specProblems(files: SpecFile[], matchers: RegExp[]): string[] {
	const problems: string[] = [];
	for (const f of files) {
		if (!f.name.endsWith('.test.ts')) continue;
		if (!matchers.some((m) => m.test(f.name)))
			problems.push(`${f.path}: matches no Playwright project, so it never runs`);
		else if (!FIXTURE_SPEC.test(f.name) && READS_FIXTURE_URL.test(f.source))
			problems.push(
				`${f.path}: reads E2E_FIXTURE_URL, so name it *.fixture.test.ts (project "fixture")`
			);
	}
	return problems;
}

/** The regular expressions in a Playwright `testMatch` (a pattern or a list of them). */
export function patternsOf(testMatch: unknown): RegExp[] {
	return [testMatch].flat().filter((p): p is RegExp => p instanceof RegExp);
}
