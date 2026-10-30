import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
	existsSync,
	lstatSync,
	mkdirSync,
	mkdtempSync,
	readlinkSync,
	realpathSync,
	rmSync,
	symlinkSync
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { isBuild, syncDevRoutes, wantDevRoutes } from './dev-routes.mjs';

let root: string;
const link = () => join(root, 'src/routes/dev');

beforeEach(() => {
	root = mkdtempSync(join(tmpdir(), 'dev-routes-'));
	mkdirSync(join(root, 'src/routes'), { recursive: true });
	mkdirSync(join(root, 'src/dev-routes'), { recursive: true });
});
afterEach(() => rmSync(root, { recursive: true, force: true }));

describe('wantDevRoutes / isBuild', () => {
	it('wants routes for vite dev or a truthy VITE_DEV_ROUTES, not for 0', () => {
		expect(wantDevRoutes(['n', 'vite', 'dev'], {})).toBe(true);
		expect(wantDevRoutes(['n', 'vite', 'build'], { VITE_DEV_ROUTES: '1' })).toBe(true);
		expect(wantDevRoutes(['n', 'vite', 'build'], { VITE_DEV_ROUTES: '0' })).toBe(false);
		expect(wantDevRoutes(['n', 'vite', 'build'], {})).toBe(false);
	});
	it('detects a build', () => {
		expect(isBuild(['n', 'vite', 'build'])).toBe(true);
		expect(isBuild(['n', 'vitest', 'run'])).toBe(false);
	});
});

describe('syncDevRoutes', () => {
	it('creates the link when wanted, idempotently', () => {
		syncDevRoutes(root, true, false);
		syncDevRoutes(root, true, false);
		expect(lstatSync(link()).isSymbolicLink()).toBe(true);
		expect(realpathSync(link())).toBe(realpathSync(join(root, 'src/dev-routes')));
		expect(existsSync(link())).toBe(true);
	});

	it('removes the link only on a build without dev routes', () => {
		syncDevRoutes(root, true, false);
		syncDevRoutes(root, false, false); // vitest, check, eslint
		expect(lstatSync(link()).isSymbolicLink()).toBe(true);
		syncDevRoutes(root, false, true);
		expect(existsSync(link())).toBe(false);
	});

	it('recreates a dangling or mis-targeted link', () => {
		symlinkSync('../gone', link(), 'junction');
		syncDevRoutes(root, true, false);
		expect(realpathSync(link())).toBe(realpathSync(join(root, 'src/dev-routes')));
		rmSync(link());
		mkdirSync(join(root, 'src/other'));
		symlinkSync('../other', link(), 'junction');
		syncDevRoutes(root, true, false);
		expect(realpathSync(link())).toBe(realpathSync(join(root, 'src/dev-routes')));
	});

	it('keeps a link that resolves correctly even if its text differs', () => {
		symlinkSync(join(root, 'src/dev-routes'), link(), 'junction'); // absolute, as Windows reads back
		syncDevRoutes(root, true, false);
		expect(readlinkSync(link())).toBe(join(root, 'src/dev-routes'));
	});

	it('refuses a real src/routes/dev folder with a clear message', () => {
		mkdirSync(link());
		expect(() => syncDevRoutes(root, true, false)).toThrow(/src\/dev-routes/);
		expect(() => syncDevRoutes(root, false, true)).toThrow(/real folder/);
	});
});
