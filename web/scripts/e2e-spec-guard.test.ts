import { describe, expect, it } from 'vitest';
import { FIXTURE_SPEC, patternsOf, specProblems, type SpecFile } from './e2e-spec-guard';

const matchers = [/(routes|tokens)\.test\.ts$/, /\.keyboard\.test\.ts$/, FIXTURE_SPEC];
const file = (name: string, source = ''): SpecFile => ({ path: `e2e/${name}`, name, source });

describe('specProblems', () => {
	it('accepts a spec that matches a project', () => {
		expect(specProblems([file('a.routes.test.ts'), file('b.keyboard.test.ts')], matchers)).toEqual(
			[]
		);
	});

	it('flags near-miss names that match no project', () => {
		for (const name of [
			'a.fixtures.test.ts',
			'a.keyboards.test.ts',
			'a.route.test.ts',
			'a.fixture.test.tsx.test.ts'
		])
			expect(specProblems([file(name)], matchers), name).toHaveLength(1);
	});

	it('ignores files that are not specs', () => {
		expect(specProblems([file('support.ts'), file('a.docs.ts')], matchers)).toEqual([]);
	});

	it('flags a real-server spec that a non-fixture project would swallow', () => {
		const src = "const url = process.env.E2E_FIXTURE_URL;\ntest.skip(!url, '');";
		expect(specProblems([file('a.fixture.routes.test.ts', src)], matchers)).toHaveLength(1);
		expect(specProblems([file('a.fixture.test.ts', src)], matchers)).toEqual([]);
	});

	it('sees the other ways of reading the variable', () => {
		for (const src of [
			"process.env['E2E_FIXTURE_URL']",
			'const { E2E_FIXTURE_URL } = process.env;'
		])
			expect(specProblems([file('a.routes.test.ts', src)], matchers)).toHaveLength(1);
	});

	it('stops matching when a project narrows its pattern', () => {
		expect(specProblems([file('a.keyboard.test.ts')], [/never/])).toHaveLength(1);
	});
});

describe('patternsOf', () => {
	it('takes a pattern, a list, and ignores strings', () => {
		expect(patternsOf(/a/)).toEqual([/a/]);
		expect(patternsOf([/a/, 'b', /c/])).toEqual([/a/, /c/]);
		expect(patternsOf(undefined)).toEqual([]);
	});
});
