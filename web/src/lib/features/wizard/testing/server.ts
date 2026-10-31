// Tests only (vitest): the settings fake server plus `GET /settings` (the index) and the network
// check, behind one stubbed `fetch`.
import { fakeServer } from '../../settings/form/testing/fake-server';
import type { components } from '$lib/api/schema';
import { WIZARD_INDEX, wizardValues } from './fixtures';

export type NetworkCheck = components['schemas']['NetworkCheck'];

export const network = (over: Partial<NetworkCheck> = {}): NetworkCheck => ({
	checked_at: 1,
	external: { agreeing: 14, ip: '203.0.113.9', port: 6881, voters: 16 },
	inbound_queries: 412,
	mapping: 'unavailable',
	measured_s: 590,
	min_window_s: 120,
	reachable: true,
	reason: 'unsolicited_queries',
	unsolicited_queries: 37,
	unsolicited_sources: 21,
	window_s: 600,
	window_started_at: 1,
	...over
});

const json = (status: number, body: unknown, headers: Record<string, string> = {}) =>
	({
		ok: status >= 200 && status < 300,
		status,
		statusText: '',
		headers: new Headers(headers),
		json: async () => structuredClone(body)
	}) as unknown as Response;

export function wizardServer(values = wizardValues()) {
	const server = fakeServer(values);
	let check: NetworkCheck | ((method: string) => Response | NetworkCheck) = network();
	const netCalls: string[] = [];
	const fetch = async (url: string, init: RequestInit = {}) => {
		const method = (init.method ?? 'GET').toUpperCase();
		const path = String(url).replace(/^\/api\/v1/, '');
		if (path === '/network/check') {
			netCalls.push(method);
			const reply = typeof check === 'function' ? check(method) : check;
			return reply instanceof Response || 'ok' in (reply as object)
				? (reply as Response)
				: json(200, reply);
		}
		if (method === 'GET' && path === '/settings') return json(200, WIZARD_INDEX);
		return server.fetch(url, init);
	};
	return {
		...server,
		fetch,
		netCalls,
		setNetwork(next: typeof check) {
			check = next;
		},
		json
	};
}
