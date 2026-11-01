// Tests only (vitest): the settings fake server plus `GET /settings` (the index) and the network
// check, behind one stubbed `fetch`.
import { fakeServer } from '../../settings/form/testing/fake-server';
import type { components } from '$lib/api/schema';
import { applied, finished, strengthening } from '../../filters/flows/testing/fixtures';
import { WIZARD_INDEX, wizardValues } from './fixtures';

export const PACK_IDS = ['malware', 'lures', 'fakes', 'spam', 'scams'];
type Assessment = ReturnType<typeof strengthening>;

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
	// The filters API the pack step drives: the overview, a preview job and apply.
	const packs = PACK_IDS.map((id) => ({ id, on: false, rules: [] as unknown[] }));
	const filterCalls: { method: string; path: string; body: unknown }[] = [];
	let failFilters = false;
	let failIndex = false;
	let assessment: Assessment = strengthening();
	let applyReply: ((body: { change: { id: string } }) => Response) | undefined;
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
		if (method === 'GET' && path === '/settings')
			return failIndex
				? json(500, { error: { code: 'internal', message: 'x' } })
				: json(200, WIZARD_INDEX);
		if (path.startsWith('/filters')) {
			const body = init.body === undefined ? undefined : JSON.parse(String(init.body));
			filterCalls.push({ method, path, body });
			if (method === 'GET' && path === '/filters')
				return failFilters
					? json(500, { error: { code: 'internal', message: 'x' } })
					: json(200, { packs: structuredClone(packs) });
			if (method === 'POST' && path === '/filters/preview/jobs') return json(200, { job: 1 });
			if (method === 'GET' && path.startsWith('/filters/preview/jobs/'))
				return json(200, finished(assessment));
			if (method === 'POST' && path === '/filters/apply') {
				if (applyReply) return applyReply(body);
				const pack = packs.find((p) => p.id === body.change.id);
				if (pack) pack.on = true;
				return json(200, applied(assessment));
			}
		}
		return server.fetch(url, init);
	};
	return {
		...server,
		fetch,
		netCalls,
		packs,
		filterCalls,
		failIndex(on: boolean) {
			failIndex = on;
		},
		failFilters(on: boolean) {
			failFilters = on;
		},
		setAssessment(next: Assessment) {
			assessment = next;
		},
		onApply(f: typeof applyReply) {
			applyReply = f;
		},
		setNetwork(next: typeof check) {
			check = next;
		},
		json
	};
}
