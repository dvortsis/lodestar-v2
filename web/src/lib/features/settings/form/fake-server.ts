// Tests only: an in-memory `/settings` API behind a stubbed `fetch`, close enough to B2b's to
// drive the form store (revisions, `if_revision`, env locks, `null` resets, the batch).
import { vi } from 'vitest';
import type { SectionValues, SettingValue } from './types';

type Reply = { status: number; body: unknown; headers?: Record<string, string> };
type Call = { method: string; path: string; body: unknown };

export function fakeServer(initial: SectionValues[]) {
	const sections = new Map<string, SectionValues>(
		initial.map((s) => [s.section, structuredClone(s)])
	);
	const calls: Call[] = [];
	/** Canned replies for the next requests to a route (`PATCH /settings`), in order. */
	const queued = new Map<string, (Reply | ((body: unknown) => Reply))[]>();
	/** Hold the next request to a route until `release()`. */
	const holds = new Map<string, { release: () => void; applied: boolean }[]>();
	let previewReply: (body: Record<string, Record<string, unknown>>) => Reply = (body) => ({
		status: 200,
		body: {
			sections: Object.fromEntries(
				Object.entries(body).map(([s, patch]) => [
					s,
					{
						revision: sections.get(s)?.revision ?? 0,
						fields: Object.fromEntries(
							Object.entries(patch)
								.filter(([k]) => k !== 'if_revision' && k !== 'confirm_adult')
								.map(([k, v]) => [
									k,
									{ estimates: [], restart_required: false, value: v, warnings: [] }
								])
						)
					}
				])
			)
		}
	});

	const err = (status: number, code: string, extra: Record<string, unknown> = {}, top = {}) => ({
		status,
		body: { error: { code, message: `msg ${code}`, ...extra }, ...top }
	});

	function patch(body: Record<string, Record<string, unknown>>, dry = false): Reply {
		const stale: Record<string, SectionValues> = {};
		for (const [s, p] of Object.entries(body)) {
			const cur = sections.get(s);
			if (!cur) return err(404, 'unknown_section', { section: s });
			if (typeof p.if_revision === 'number' && p.if_revision < cur.revision) stale[s] = cur;
			for (const [k] of Object.entries(p)) {
				if (k === 'if_revision' || k === 'confirm_adult') continue;
				const f = cur.fields[k];
				if (!f) return err(400, 'unknown_field', { section: s, field: k });
				if (f.source === 'env') return err(409, 'set_by_environment', { section: s, field: k });
				if (f.read_only) return err(409, 'read_only', { section: s, field: k });
			}
		}
		if (Object.keys(stale).length) {
			return err(409, 'stale_revision', {}, { current: structuredClone(stale) });
		}
		if (dry) return previewReply(body);
		const out: Record<string, SectionValues> = {};
		for (const [s, p] of Object.entries(body)) {
			const cur = sections.get(s)!;
			let changed = false;
			for (const [k, v] of Object.entries(p)) {
				if (k === 'if_revision' || k === 'confirm_adult') continue;
				const f = cur.fields[k]!;
				const next: SettingValue =
					v === null
						? { ...f, source: 'default', value: f.value }
						: { ...f, source: 'db', value: v };
				if (JSON.stringify(next) !== JSON.stringify(f)) changed = true;
				cur.fields[k] = next;
			}
			if (changed) cur.revision++;
			out[s] = structuredClone(cur);
		}
		return { status: 200, body: { sections: out } };
	}

	function route(method: string, path: string, body: unknown): Reply {
		const q = queued.get(`${method} ${path}`);
		const canned = q?.shift();
		if (canned) return typeof canned === 'function' ? canned(body) : canned;
		if (method === 'GET' && path.startsWith('/settings/')) {
			const s = sections.get(decodeURIComponent(path.slice('/settings/'.length)));
			return s ? { status: 200, body: structuredClone(s) } : err(404, 'unknown_section');
		}
		if (method === 'PATCH' && path === '/settings') return patch(body as never);
		if (method === 'POST' && path === '/settings/preview') return patch(body as never, true);
		return err(404, 'not_found');
	}

	const fetchMock = vi.fn(async (url: string, init: RequestInit = {}) => {
		const method = (init.method ?? 'GET').toUpperCase();
		const path = String(url).replace(/^\/api\/v1/, '');
		const body = init.body === undefined ? undefined : JSON.parse(String(init.body));
		calls.push({ method, path, body });
		const hold = holds.get(`${method} ${path}`)?.shift();
		const wait = () => new Promise<void>((r) => (hold!.release = r));
		if (hold && !hold.applied) await wait();
		if (init.signal?.aborted) throw new DOMException('Aborted', 'AbortError');
		const reply = route(method, path, body);
		// `applied`: the server has saved, only the response is late (its event can come first).
		if (hold?.applied) await wait();
		if (reply.status === 0) throw new TypeError('Failed to fetch');
		const headers = new Headers(reply.headers);
		return {
			ok: reply.status >= 200 && reply.status < 300,
			status: reply.status,
			statusText: '',
			headers,
			json: async () => structuredClone(reply.body)
		} as unknown as Response;
	});

	return {
		fetch: fetchMock,
		calls,
		sections,
		/** Changes a section as another tab would, bumping its revision. */
		change(section: string, fields: Record<string, Partial<SettingValue>>) {
			const s = sections.get(section)!;
			for (const [k, v] of Object.entries(fields)) s.fields[k] = { ...s.fields[k]!, ...v };
			s.revision++;
			return s.revision;
		},
		next(route: string, ...replies: (Reply | ((body: unknown) => Reply))[]) {
			queued.set(route, [...(queued.get(route) ?? []), ...replies]);
		},
		hold(route: string, opts: { applied?: boolean } = {}) {
			const h = { release: () => {}, applied: opts.applied ?? false };
			holds.set(route, [...(holds.get(route) ?? []), h]);
			return () => h.release();
		},
		onPreview(f: typeof previewReply) {
			previewReply = f;
		},
		err,
		requests: (method: string, path: string) =>
			calls.filter((c) => c.method === method && c.path === path)
	};
}

/** Lets pending promise chains (fake fetches, their `.json()`) run. */
export async function flush(rounds = 30): Promise<void> {
	for (let i = 0; i < rounds; i++) await Promise.resolve();
}
