// The chrome's copy of the registry's CONTROL list (X-skip-results-flake) can't drift: both are
// read as source, every selector expanded, and the sets compared.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = (path: string) => readFileSync(resolve(process.cwd(), path), 'utf8');

/** The selectors a `const CONTROL = [ ... ].join(', ')` list builds, with each `.map` expanded. */
function selectors(text: string): string[] {
	const start = text.indexOf('const CONTROL = [');
	const end = text.indexOf("].join(', ')", start);
	expect(start, 'CONTROL list').toBeGreaterThan(-1);
	expect(end, 'end of the CONTROL list').toBeGreaterThan(start);
	let body = text.slice(start + 'const CONTROL = ['.length, end);
	const out: string[] = [];
	// `...[ 'a', 'b' ].map((x) => `prefix${x}suffix`)`
	const mapped = /\.\.\.\[([^\]]*)\]\s*\.map\(\s*\(?(\w+)\)?\s*=>\s*`([^`]*)`\s*\)/g;
	body = body.replace(mapped, (_all, items: string, name: string, template: string) => {
		for (const item of items.match(/'([^']*)'/g) ?? [])
			out.push(template.replace(`\${${name}}`, item.slice(1, -1)));
		return '';
	});
	for (const item of body.match(/'([^']*)'/g) ?? []) out.push(item.slice(1, -1));
	return out.sort();
}

describe('the CONTROL list', () => {
	it('is the same in the registry and in the chrome', () => {
		const registry = selectors(source('src/lib/keys/registry.svelte.ts'));
		const chrome = selectors(source('src/lib/shell/Chrome.svelte'));
		expect(registry.length).toBeGreaterThan(20);
		expect(registry).toContain('input[type="checkbox"]');
		expect(registry).toContain('[role="option"]');
		expect(chrome).toEqual(registry);
	});
});
