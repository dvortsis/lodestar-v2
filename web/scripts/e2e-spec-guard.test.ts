import { describe, expect, it } from 'vitest';
import {
	covers,
	FIXTURE_SPEC,
	specProblems,
	type GuardProject,
	type SpecFile
} from './e2e-spec-guard';

const writes = /(filters-flows|settings-form)\.fixture\.test\.ts$/;
const projects: GuardProject[] = [
	{ name: 'core', testMatch: /(routes|tokens)\.test\.ts$/ },
	{ name: 'keyboard-only', testMatch: /\.keyboard\.test\.ts$/ },
	{ name: 'fixture-writes', testMatch: writes },
	{ name: 'fixture', testMatch: FIXTURE_SPEC, testIgnore: writes }
];
const file = (path: string, source = ''): SpecFile => ({ path, source });

describe('specProblems', () => {
	it('accepts a spec that matches a project', () => {
		expect(specProblems([file('a.routes.test.ts'), file('b.keyboard.test.ts')], projects)).toEqual(
			[]
		);
		expect(
			specProblems([file('x.fixture.test.ts'), file('settings-form.fixture.test.ts')], projects)
		).toEqual([]);
	});

	it('flags near-miss names that match no project', () => {
		for (const path of [
			'a.fixtures.test.ts',
			'a.keyboards.test.ts',
			'a.route.test.ts',
			'a.fixture.test.tsx'
		])
			expect(specProblems([file(path)], projects), path).toHaveLength(1);
	});

	it('flags specs with the other names Playwright would pick up', () => {
		for (const path of [
			'a.fixture.spec.ts',
			'a.test.tsx',
			'a.test.js',
			'a.spec.mjs',
			'sub/a.spec.ts'
		])
			expect(specProblems([file(path)], projects), path).toHaveLength(1);
	});

	it('ignores files that are not specs', () => {
		expect(specProblems([file('support/helper.ts'), file('a.docs.ts')], projects)).toEqual([]);
	});

	it('flags a spec that a project ignores and no other project matches', () => {
		// Someone narrows the writes project's testMatch but leaves its testIgnore on `fixture`.
		const narrowed = projects.map((p) =>
			p.name === 'fixture-writes' ? { ...p, testMatch: /filters-flows\.fixture\.test\.ts$/ } : p
		);
		expect(specProblems([file('settings-form.fixture.test.ts')], narrowed)).toHaveLength(1);
		expect(specProblems([file('filters-flows.fixture.test.ts')], narrowed)).toEqual([]);
	});

	it('matches the path relative to the test directory, not the basename', () => {
		const dir: GuardProject = { name: 'docs', testMatch: /docs\/.*\.test\.ts$/ };
		expect(specProblems([file('docs/a.test.ts')], [dir])).toEqual([]);
		expect(specProblems([file('other/a.test.ts')], [dir])).toHaveLength(1);
	});

	it('flags a real-server spec that a non-fixture project would swallow', () => {
		const src = "const url = process.env.E2E_FIXTURE_URL;\ntest.skip(!url, '');";
		expect(specProblems([file('a.fixture.routes.test.ts', src)], projects)).toHaveLength(1);
		expect(specProblems([file('a.fixture.test.ts', src)], projects)).toEqual([]);
	});

	it('sees the other ways of reading the variable', () => {
		for (const src of [
			"process.env['E2E_FIXTURE_URL']",
			'const { E2E_FIXTURE_URL } = process.env;'
		])
			expect(specProblems([file('a.routes.test.ts', src)], projects)).toHaveLength(1);
	});

	it('stops matching when a project narrows its pattern', () => {
		expect(
			specProblems([file('a.keyboard.test.ts')], [{ name: 'k', testMatch: /never/ }])
		).toHaveLength(1);
	});
});

describe('covers', () => {
	it('needs testMatch to match and testIgnore not to', () => {
		const p: GuardProject = { name: 'p', testMatch: [/a/, 'b'], testIgnore: /ab/ };
		expect(covers(p, 'a')).toBe(true);
		expect(covers(p, 'ab')).toBe(false);
		expect(covers(p, 'c')).toBe(false);
		expect(covers({ name: 'q' }, 'a')).toBe(false);
	});
});
