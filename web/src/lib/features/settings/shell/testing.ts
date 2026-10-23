// Tests only: screens to resolve, and a Settings API behind a stubbed `fetch`.
import { vi } from 'vitest';
import { fakeServer } from '../form/testing/fake-server';
import { goldenIndex, goldenValues } from './golden';
import type { SettingsScreen } from './registry';

import { indexView } from '../form/schema';

export { GOLDEN, checkBody, goldenIndex, goldenValues, systemInfo } from './golden';
export const goldenView = () => indexView(goldenIndex());

export const screen = (id: string, over: Partial<SettingsScreen> = {}): SettingsScreen => ({
	id,
	group: 'system',
	title: () => id.toUpperCase(),
	...over
});

/** Extra replies by `METHOD /path`; anything else goes to the in-memory settings server. */
export type Replies = Record<
	string,
	() => { status: number; body: unknown; headers?: Record<string, string> }
>;

/** Stubs `fetch`: the golden `GET /settings`, its sections' values, and `replies`. */
export function settingsApi(replies: Replies = {}, initial = goldenValues()) {
	const server = fakeServer(initial);
	const calls: { method: string; path: string; body?: unknown }[] = [];
	const fetch = vi.fn(async (url: string, init: RequestInit = {}) => {
		const method = (init.method ?? 'GET').toUpperCase();
		const path = String(url).replace(/^\/api\/v1/, '');
		calls.push({ method, path, body: init.body ? JSON.parse(String(init.body)) : undefined });
		const reply = replies[`${method} ${path}`];
		const out = reply
			? reply()
			: method === 'GET' && path === '/settings'
				? { status: 200, body: goldenIndex() }
				: undefined;
		if (!out) return server.fetch(url, init);
		return {
			ok: out.status >= 200 && out.status < 300,
			status: out.status,
			statusText: '',
			headers: new Headers(out.headers),
			json: async () => structuredClone(out.body)
		} as unknown as Response;
	});
	vi.stubGlobal('fetch', fetch);
	return { server, fetch, calls };
}
