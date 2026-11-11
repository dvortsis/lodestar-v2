// @vitest-environment node
// The checked-in files in logging/event-names/ and the loader map event-names.ts are what the
// generator writes for the server's current catalogue, so a new or removed server event changes
// them (and its messages) in the same PR.
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { basename } from 'node:path';
import { describe, expect, it } from 'vitest';
// @ts-expect-error: a plain .mjs script with no type declarations
import { CATALOG, DIR, OUTPUT, componentIds, eventsByComponent, render } from './event-names.mjs';

type Render = (g: string) => Promise<Record<string, string>>;
const golden = readFileSync(CATALOG, 'utf8');

describe('event-names generator', () => {
	it('matches the checked-in files', async () => {
		const files = await (render as Render)(golden);
		for (const [file, text] of Object.entries(files)) {
			expect(existsSync(file), `${file} is missing`).toBe(true);
			expect(readFileSync(file, 'utf8'), file).toBe(text);
		}
	});

	it('leaves no stale chunk behind, and writes one chunk per component', async () => {
		const files = await (render as Render)(golden);
		const components = (componentIds as (g: string) => string[])(golden);
		expect(components.length).toBeGreaterThan(5);
		expect(Object.keys(files).sort()).toEqual(
			[OUTPUT, ...components.map((c) => `${DIR}/${c}.ts`)].sort()
		);
		expect(readdirSync(DIR as string).sort()).toEqual(components.map((c) => `${c}.ts`).sort());
	});

	it('puts an event in its own component chunk only', async () => {
		const files = await (render as Render)(golden);
		const owner = Object.entries(files).filter(
			([f, t]) => f !== OUTPUT && t.includes("'app.start'")
		);
		expect(owner.map(([f]) => basename(f))).toEqual(['system.ts']);
	});

	it('refuses a component id that is not a plain file and key name', () => {
		const golden = (c: string) =>
			`200\n${JSON.stringify({ events: [{ id: 'a.b', component: c }] })}\n`;
		const groups = eventsByComponent as (g: string) => Map<string, string[]>;
		expect([...groups(golden('abc_d')).keys()]).toEqual(['abc_d']);
		for (const bad of ['v2', 'a-b', 'a.b', '../x', '__proto__', 'A', ''])
			expect(() => groups(golden(bad)), bad).toThrow(/must be/);
	});
});
