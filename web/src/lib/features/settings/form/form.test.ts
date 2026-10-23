import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { onLoginRequired } from '$lib/api/client';
import { DRAFT_PREFIX } from './draft';
import { fakeServer, flush } from './testing/fake-server';
import { createSettingsForm, preloadSettingsForm, type SettingsForm } from './form.svelte';
import { clearIndexCache } from './schema';
import { spec, sv, values } from './testing/values';
import type { FieldSpec, GateContext } from './types';

const SECRET = 'hunter2-very-secret';

const SPECS: FieldSpec[] = [
	spec('crawler.intensity', { kind: 'enum', enum: ['low', 'balanced', 'aggressive'], order: 0 }),
	spec('crawler.timeout_secs', {
		kind: 'integer',
		minimum: 1,
		maximum: 120,
		unit: 'seconds',
		order: 1,
		restartRequired: true
	}),
	spec('crawler.max_sockets', {
		kind: 'integer',
		minimum: 0,
		maximum: 4096,
		auto: 0,
		preview: true,
		order: 2
	}),
	spec('transport.utp', { kind: 'boolean' }),
	spec('network.http_bind', { format: 'socket-addr', danger: 'disruptive', preview: true }),
	spec('network.dht_port', { kind: 'integer', minimum: 1, maximum: 65535 }),
	spec('logging.data', { kind: 'enum', enum: ['private', 'full'], danger: 'privacy' }),
	spec('content.show_adult', { kind: 'boolean' }),
	spec('secrets.token', { writeOnly: true })
];

const initial = () => [
	values('crawler', 3, {
		intensity: sv('balanced'),
		timeout_secs: sv(12, 'db'),
		max_sockets: sv(0)
	}),
	values('transport', 7, { utp: sv(true) }),
	values('network', 2, {
		http_bind: sv('0.0.0.0:8090', 'file'),
		dht_port: sv(6881, 'env', { env: 'LODESTAR_DHT_PORT', read_only: true })
	}),
	values('logging', 1, { data: sv('private') }),
	values('content', 1, { show_adult: sv(false) }),
	values('secrets', 1, { token: sv(null, 'db') })
];

let server: ReturnType<typeof fakeServer>;
let form: SettingsForm;
const forms: SettingsForm[] = [];

function open(over: Partial<Parameters<typeof createSettingsForm>[0]> = {}) {
	const f = createSettingsForm({ id: 'test', fields: SPECS, ...over });
	forms.push(f);
	return f;
}

// The parts loaded on demand are loaded up front here; `lazy.test.ts` covers loading them.
beforeAll(() => preloadSettingsForm());

beforeEach(async () => {
	vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout', 'Date'], now: 1_000_000 });
	sessionStorage.clear();
	clearIndexCache();
	server = fakeServer(initial());
	vi.stubGlobal('fetch', server.fetch);
	form = open();
	await flush();
});

afterEach(() => {
	for (const f of forms.splice(0)) f.destroy();
	vi.unstubAllGlobals();
	vi.useRealTimers();
	vi.restoreAllMocks();
	onLoginRequired(undefined);
});

const patches = () => server.requests('PATCH', '/settings');

describe('loading', () => {
	it('fetches each section once and shows its values', () => {
		expect(form.status).toBe('ready');
		expect(server.calls.filter((c) => c.method === 'GET').map((c) => c.path)).toEqual([
			'/settings/crawler',
			'/settings/transport',
			'/settings/network',
			'/settings/logging',
			'/settings/content',
			'/settings/secrets'
		]);
		expect(form.field('crawler.timeout_secs')).toMatchObject({
			value: 12,
			source: 'db',
			changed: true,
			dirty: false,
			locked: false
		});
		expect(form.field('network.dht_port')).toMatchObject({
			locked: true,
			env: 'LODESTAR_DHT_PORT'
		});
		expect(form.field('secrets.token')).toMatchObject({ value: null, secretSet: true });
	});

	it('resolves field keys through one cached GET /settings', async () => {
		const index = {
			sections: [
				{
					section: 'transport',
					label_key: 'settings.transport.label',
					help_key: 'settings.transport.help',
					schema: {
						type: 'object',
						properties: { utp: { type: 'boolean', 'x-group': 'transport' } }
					},
					fields: [
						{
							name: 'utp',
							env: null,
							label_key: 'settings.transport.utp.label',
							help_key: 'settings.transport.utp.help',
							read_only: false,
							restart_required: true
						}
					]
				}
			]
		};
		server.next('GET /settings', { status: 200, body: index });
		const a = open({ id: 'a', fields: ['transport.utp'] });
		const b = open({ id: 'b', fields: ['transport.utp'] });
		await flush();
		expect(a.specs.map((s) => s.key)).toEqual(['transport.utp']);
		expect(b.status).toBe('ready');
		expect(server.requests('GET', '/settings')).toHaveLength(1);
	});

	it('shows a load error and retries', async () => {
		server.next('GET /settings/crawler', server.err(500, 'internal'));
		const f = open({ id: 'x' });
		await flush();
		expect(f.status).toBe('load-error');
		expect(f.loadError?.status).toBe(500);
		await f.retry();
		await flush();
		expect(f.status).toBe('ready');
	});
});

describe('saving', () => {
	it('sends exactly one PATCH with only the dirty fields, then is clean', async () => {
		form.set('crawler.intensity', 'low');
		form.set('transport.utp', false);
		form.set('crawler.timeout_secs', 12); // back to saved: not a change
		expect(form.dirtyCount).toBe(2);
		const r = await form.save();
		expect(r).toEqual({ ok: true, restart: [] });
		expect(patches()).toHaveLength(1);
		expect(patches()[0]!.body).toEqual({
			crawler: { if_revision: 3, intensity: 'low' },
			transport: { if_revision: 7, utp: false }
		});
		expect(form.dirtyCount).toBe(0);
		expect(form.status).toBe('saved');
		expect(form.field('crawler.intensity')).toMatchObject({
			value: 'low',
			source: 'db',
			savedRecently: 'live'
		});
		expect(form.announcement?.text).toBe('Saved.');
		// The next save starts from the new revision.
		form.set('crawler.intensity', 'aggressive');
		await form.save();
		expect(patches()[1]!.body).toEqual({ crawler: { if_revision: 4, intensity: 'aggressive' } });
	});

	it('sends a reset as null, and reports fields waiting for a restart', async () => {
		server.next('PATCH /settings', () => ({
			status: 200,
			body: {
				sections: {
					crawler: values(
						'crawler',
						4,
						{ intensity: sv('balanced'), timeout_secs: sv(10), max_sockets: sv(0) },
						['timeout_secs']
					)
				}
			}
		}));
		form.reset('crawler.timeout_secs');
		expect(form.field('crawler.timeout_secs').pendingReset).toBe(true);
		const r = await form.save();
		expect(patches()[0]!.body).toEqual({ crawler: { if_revision: 3, timeout_secs: null } });
		expect(r).toEqual({ ok: true, restart: ['crawler.timeout_secs'] });
		expect(form.restartPending).toEqual(['crawler.timeout_secs']);
		expect(form.field('crawler.timeout_secs').savedRecently).toBe('restart');
		expect(form.announcement?.text).toMatch(/after a restart/);
	});

	it('offers no reset for a value that isn’t saved in the app, or a locked one', () => {
		form.reset('network.http_bind'); // from config.toml
		form.reset('crawler.intensity'); // default
		form.reset('network.dht_port'); // env
		expect(form.dirtyCount).toBe(0);
	});

	it('keeps an edit made while the save was in flight', async () => {
		const release = server.hold('PATCH /settings');
		form.set('crawler.intensity', 'low');
		const saving = form.save();
		await flush();
		expect(form.status).toBe('saving');
		form.set('crawler.intensity', 'aggressive');
		form.set('transport.utp', false);
		// A second save while one is in flight sends nothing.
		expect(await form.save()).toEqual({ ok: false, reason: 'busy' });
		release();
		await saving;
		expect(patches()).toHaveLength(1);
		expect(form.field('crawler.intensity')).toMatchObject({
			value: 'aggressive',
			dirty: true,
			saved: 'low'
		});
		expect(form.dirtyCount).toBe(2);
	});

	it('keeps an edit back to the old value made while the save was in flight', async () => {
		const release = server.hold('PATCH /settings');
		form.set('crawler.intensity', 'low');
		const saving = form.save();
		await flush();
		form.set('crawler.intensity', 'balanced'); // the value saved before this save
		release();
		await saving;
		expect(form.field('crawler.intensity')).toMatchObject({
			value: 'balanced',
			saved: 'low',
			dirty: true
		});
		expect(await form.save()).toMatchObject({ ok: true });
		expect(patches()[1]!.body).toEqual({ crawler: { if_revision: 4, intensity: 'balanced' } });
	});

	it('treats a stale refusal with a malformed current as a conflict, without throwing', async () => {
		server.next('PATCH /settings', {
			status: 409,
			body: {
				error: { code: 'stale_revision', message: 'x' },
				current: { crawler: { revision: 'nope' }, transport: null, network: [] }
			}
		});
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'conflict' });
		expect(form.field('crawler.intensity')).toMatchObject({ value: 'low', dirty: true });
	});

	it('returns nothing with nothing to save', async () => {
		expect(await form.save()).toEqual({ ok: false, reason: 'nothing' });
		expect(patches()).toHaveLength(0);
	});

	it('sends nothing with client errors, and lists them in field order', async () => {
		form.set('network.http_bind', 'nope');
		form.set('crawler.timeout_secs', 500);
		const r = await form.save();
		expect(r).toEqual({ ok: false, reason: 'invalid' });
		expect(patches()).toHaveLength(0);
		expect(form.errors).toEqual([
			{ key: 'crawler.timeout_secs', message: 'Must be between 1 and 120 seconds.' },
			{ key: 'network.http_bind', message: expect.stringMatching(/address and a port/) }
		]);
		expect(form.field('crawler.timeout_secs').error).toMatch(/between/);
		// Fixing a field clears its error as soon as it's valid.
		form.set('crawler.timeout_secs', 30);
		expect(form.field('crawler.timeout_secs').error).toBeUndefined();
	});

	it('validates a field on blur', () => {
		form.set('crawler.timeout_secs', 0);
		expect(form.field('crawler.timeout_secs').error).toBeUndefined();
		form.blur('crawler.timeout_secs');
		expect(form.field('crawler.timeout_secs').error).toMatch(/between/);
		form.set('crawler.timeout_secs', 12);
		form.blur('crawler.timeout_secs');
		expect(form.field('crawler.timeout_secs').error).toBeUndefined();
	});

	it('maps 422 invalid_setting onto the field, with the server’s message', async () => {
		server.next(
			'PATCH /settings',
			server.err(422, 'invalid_setting', {
				section: 'crawler',
				field: 'timeout_secs',
				message: 'Must be from 1 to 120.'
			})
		);
		form.set('crawler.timeout_secs', 30);
		expect(await form.save()).toEqual({ ok: false, reason: 'invalid' });
		expect(form.errors).toEqual([
			{ key: 'crawler.timeout_secs', message: 'Must be from 1 to 120.' }
		]);
		expect(form.dirtyCount).toBe(1);
	});

	it('maps 409 set_by_environment onto the field and refetches its section', async () => {
		server.change('transport', { utp: { source: 'env', env: 'LODESTAR_UTP', read_only: true } });
		const rev = server.sections.get('transport')!.revision;
		server.sections.get('transport')!.revision = 7; // env changes don't bump the revision
		form.set('transport.utp', false);
		const r = await form.save();
		await flush();
		expect(r).toEqual({ ok: false, reason: 'refused' });
		expect(server.requests('GET', '/settings/transport')).toHaveLength(2);
		expect(form.field('transport.utp')).toMatchObject({ locked: true, dirty: false });
		expect(form.field('transport.utp').note).toMatch(/LODESTAR_UTP/);
		expect(form.errors[0]).toMatchObject({ key: 'transport.utp' });
		expect(rev).toBe(8);
	});

	it('maps 409 read_only onto the field and refetches its section', async () => {
		server.next(
			'PATCH /settings',
			server.err(409, 'read_only', {
				section: 'crawler',
				field: 'intensity',
				message: 'Read-only.'
			})
		);
		server.sections.get('crawler')!.fields.intensity!.read_only = true;
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		await flush();
		expect(server.requests('GET', '/settings/crawler')).toHaveLength(2);
		expect(form.field('crawler.intensity')).toMatchObject({ locked: true, dirty: false });
		expect(form.errors).toEqual([{ key: 'crawler.intensity', message: 'Read-only.' }]);
	});

	it('merges on 409 stale_revision and waits for the user to save again', async () => {
		server.change('crawler', {
			intensity: { value: 'aggressive', source: 'db' },
			max_sockets: { value: 50, source: 'db' }
		});
		form.set('crawler.intensity', 'low');
		form.set('crawler.timeout_secs', 30);
		expect(await form.save()).toEqual({ ok: false, reason: 'conflict' });
		expect(patches()).toHaveLength(1);
		expect(form.conflicts).toEqual(['crawler.intensity']);
		expect(form.field('crawler.intensity').conflict).toEqual({ mine: 'low', theirs: 'aggressive' });
		expect(form.field('crawler.max_sockets')).toMatchObject({ value: 50, dirty: false });
		expect(form.field('crawler.timeout_secs')).toMatchObject({ value: 30, dirty: true });
		// Save waits until each conflict is resolved.
		expect(await form.save()).toEqual({ ok: false, reason: 'conflict' });
		expect(patches()).toHaveLength(1);
		form.resolve('crawler.intensity', 'mine');
		expect(await form.save()).toMatchObject({ ok: true });
		expect(patches()[1]!.body).toEqual({
			crawler: { if_revision: 4, intensity: 'low', timeout_secs: 30 }
		});
	});

	it('takes theirs on Use theirs', async () => {
		server.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		form.set('crawler.intensity', 'low');
		await form.save();
		form.resolve('crawler.intensity', 'theirs');
		expect(form.field('crawler.intensity')).toMatchObject({ value: 'aggressive', dirty: false });
		expect(form.dirtyCount).toBe(0);
	});

	it('reports unknown fields and sections in the summary and the console', async () => {
		const error = vi.spyOn(console, 'error').mockImplementation(() => {});
		server.next(
			'PATCH /settings',
			server.err(400, 'unknown_field', { section: 'crawler', field: 'intensity' })
		);
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		expect(form.errors).toEqual([{ message: expect.stringMatching(/Reload the page/) }]);
		expect(error).toHaveBeenCalled();
		server.next('PATCH /settings', server.err(404, 'unknown_section', { section: 'crawler' }));
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
	});

	it('asks an onConfirm handler and re-sends once with its keys', async () => {
		server.next(
			'PATCH /settings',
			server.err(422, 'adult_confirmation_required', {
				section: 'content',
				field: 'show_adult',
				message: 'Confirm 18+'
			})
		);
		const ask = vi.fn(async () => ({ confirm_adult: true }));
		form.onConfirm('adult_confirmation_required', ask);
		form.set('content.show_adult', true);
		expect(await form.save()).toMatchObject({ ok: true });
		expect(ask).toHaveBeenCalledWith({
			section: 'content',
			field: 'show_adult',
			message: 'Confirm 18+'
		});
		expect(patches().map((p) => p.body)).toEqual([
			{ content: { if_revision: 1, show_adult: true } },
			{ content: { if_revision: 1, show_adult: true, confirm_adult: true } }
		]);
	});

	it('cancels when the onConfirm handler returns null, keeping the edit', async () => {
		server.next(
			'PATCH /settings',
			server.err(422, 'adult_confirmation_required', { section: 'content', field: 'show_adult' })
		);
		form.onConfirm('adult_confirmation_required', async () => null);
		form.set('content.show_adult', true);
		expect(await form.save()).toEqual({ ok: false, reason: 'cancelled' });
		expect(patches()).toHaveLength(1);
		expect(form.dirtyCount).toBe(1);
	});

	it('asks only once: a second refusal shows on the field', async () => {
		const refusal = server.err(422, 'adult_confirmation_required', {
			section: 'content',
			field: 'show_adult',
			message: 'Confirm 18+'
		});
		server.next('PATCH /settings', refusal, refusal);
		const ask = vi.fn(async () => ({ confirm_adult: true }));
		form.onConfirm('adult_confirmation_required', ask);
		form.set('content.show_adult', true);
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		expect(ask).toHaveBeenCalledTimes(1);
		expect(patches()).toHaveLength(2);
		expect(form.field('content.show_adult').error).toBe('Confirm 18+');
	});

	it('shows a confirmation code with no handler on the field', async () => {
		server.next(
			'PATCH /settings',
			server.err(422, 'adult_confirmation_required', {
				section: 'content',
				field: 'show_adult',
				message: 'Confirm 18+'
			})
		);
		form.set('content.show_adult', true);
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		expect(form.field('content.show_adult').error).toBe('Confirm 18+');
	});

	it('keeps the draft and goes offline on a network failure', async () => {
		server.next('PATCH /settings', { status: 0, body: null });
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'network' });
		expect(form.status).toBe('offline');
		expect(form.dirtyCount).toBe(1);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toContain('"low"');
		await form.retry();
		expect(form.status).toBe('saved');
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toBeNull();
	});

	it('treats 429 and 503 like an outage, with retryAfter', async () => {
		server.next('PATCH /settings', { ...server.err(503, 'busy'), headers: { 'Retry-After': '5' } });
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'network' });
		expect(form.status).toBe('offline');
		expect(form.saveError?.retryAfter).toBe(5);
	});

	it('shows other server errors as a save error, keeping the edit', async () => {
		server.next('PATCH /settings', server.err(500, 'internal'));
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		expect(form.status).toBe('save-error');
		expect(form.dirtyCount).toBe(1);
	});

	it('hands a 401 to the sign-in redirect with the draft already stored', async () => {
		const login = vi.fn();
		onLoginRequired(login);
		server.next('PATCH /settings', server.err(401, 'login_required'));
		form.set('crawler.intensity', 'low'); // saved at once: no time for the draft's debounce
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		expect(login).toHaveBeenCalled();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toContain('"low"');
	});
});

describe('gates', () => {
	it.each(['privacy', 'security', 'destructive', 'disruptive'] as const)(
		'fails closed for %s with no handler: nothing is sent',
		async (danger) => {
			const error = vi.spyOn(console, 'error').mockImplementation(() => {});
			server.sections.set('gate', values('gate', 1, { f: sv(false) }));
			const f = open({
				id: `gate-${danger}`,
				fields: [spec('gate.f', { kind: 'boolean', danger })]
			});
			await flush();
			f.set('gate.f', true);
			expect(await f.save()).toEqual({ ok: false, reason: 'refused' });
			expect(patches()).toHaveLength(0);
			expect(server.requests('POST', '/settings/preview')).toHaveLength(0);
			expect(f.errors).toEqual([{ message: expect.stringMatching(/confirmation/) }]);
			expect(error).toHaveBeenCalledWith(expect.stringContaining('gate.f'));
			// With a handler it goes through, after the preview.
			f.gate(danger, async () => true);
			expect(await f.save()).toMatchObject({ ok: true });
			expect(patches()).toHaveLength(1);
		}
	);

	it('fails closed for privacy among other fields: nothing is sent', async () => {
		const error = vi.spyOn(console, 'error').mockImplementation(() => {});
		form.set('logging.data', 'full');
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
		expect(patches()).toHaveLength(0);
		expect(server.requests('POST', '/settings/preview')).toHaveLength(0);
		expect(form.errors).toEqual([{ message: expect.stringMatching(/confirmation/) }]);
		expect(error).toHaveBeenCalledWith(expect.stringContaining('logging.data'));
	});

	it('previews the whole batch first, then asks the disruptive handler once', async () => {
		const ask = vi.fn<(c: GateContext) => Promise<boolean>>(async () => true);
		form.gate('disruptive', ask);
		form.set('network.http_bind', '0.0.0.0:9000');
		form.set('crawler.intensity', 'low');
		expect(await form.save()).toMatchObject({ ok: true });
		const preview = server.requests('POST', '/settings/preview').at(-1)!;
		expect(preview.body).toEqual({
			crawler: { if_revision: 3, intensity: 'low' },
			network: { if_revision: 2, http_bind: '0.0.0.0:9000' }
		});
		expect(server.calls.indexOf(preview)).toBeLessThan(server.calls.indexOf(patches()[0]!));
		expect(ask).toHaveBeenCalledTimes(1);
		expect(ask.mock.calls[0]![0]).toMatchObject({
			fields: ['network.http_bind'],
			preview: { sections: { network: { fields: { http_bind: expect.anything() } } } }
		});
	});

	it('cancels when a gate says no', async () => {
		form.gate('disruptive', async () => false);
		form.set('network.http_bind', '0.0.0.0:9000');
		expect(await form.save()).toEqual({ ok: false, reason: 'cancelled' });
		expect(patches()).toHaveLength(0);
		expect(form.status).toBe('ready');
	});

	it('runs no gate for safe fields', async () => {
		const ask = vi.fn(async () => true);
		form.gate('disruptive', ask);
		form.set('crawler.intensity', 'low');
		await form.save();
		expect(ask).not.toHaveBeenCalled();
	});

	it('unregisters a gate', async () => {
		vi.spyOn(console, 'error').mockImplementation(() => {});
		const off = form.gate('disruptive', async () => true);
		off();
		form.set('network.http_bind', '0.0.0.0:9000');
		expect(await form.save()).toEqual({ ok: false, reason: 'refused' });
	});
});

describe('undo', () => {
	it('sends previous values, or null where nothing was saved, with the new revisions', async () => {
		form.set('crawler.intensity', 'low'); // was default
		form.set('crawler.timeout_secs', 30); // was saved: 12
		form.set('transport.utp', false);
		await form.save();
		expect(form.undoUntil).toBe(Date.now() + 10_000);
		expect(await form.undo()).toMatchObject({ ok: true });
		expect(patches()[1]!.body).toEqual({
			crawler: { if_revision: 4, intensity: null, timeout_secs: 12 },
			transport: { if_revision: 8, utp: null }
		});
		expect(form.undoUntil).toBeNull();
		expect(form.field('crawler.timeout_secs')).toMatchObject({ value: 12, source: 'db' });
		expect(form.field('crawler.intensity').source).toBe('default');
		expect(form.announcement?.text).toBe('Undone.');
	});

	it('is offered for 10 seconds', async () => {
		form.set('crawler.intensity', 'low');
		await form.save();
		vi.advanceTimersByTime(9_999);
		expect(form.undoUntil).not.toBeNull();
		vi.advanceTimersByTime(1);
		expect(form.undoUntil).toBeNull();
		expect(form.status).toBe('ready');
		expect(await form.undo()).toEqual({ ok: false, reason: 'nothing' });
		expect(patches()).toHaveLength(1);
	});

	it('says Undo isn’t possible after a change elsewhere, and never merges it', async () => {
		form.set('crawler.intensity', 'low');
		await form.save();
		server.change('crawler', { intensity: { value: 'aggressive', source: 'db' } });
		expect(await form.undo()).toEqual({ ok: false, reason: 'conflict' });
		await flush();
		expect(form.announcement?.text).toMatch(/Undo isn't possible/);
		expect(form.field('crawler.intensity')).toMatchObject({ value: 'aggressive', dirty: false });
		expect(form.conflicts).toEqual([]);
		expect(form.undoUntil).toBeNull();
	});

	it('retries a failed Undo, not a save', async () => {
		form.set('crawler.intensity', 'low');
		await form.save();
		server.next('PATCH /settings', { status: 0, body: null });
		expect(await form.undo()).toEqual({ ok: false, reason: 'network' });
		expect(form.status).toBe('offline');
		await form.retry();
		expect(patches().map((p) => p.body)).toEqual([
			{ crawler: { if_revision: 3, intensity: 'low' } },
			{ crawler: { if_revision: 4, intensity: null } },
			{ crawler: { if_revision: 4, intensity: null } }
		]);
		expect(form.field('crawler.intensity').source).toBe('default');
	});

	it('returns busy for Undo and Remove while a save is in flight', async () => {
		form.set('crawler.intensity', 'low');
		await form.save();
		const release = server.hold('PATCH /settings');
		form.set('transport.utp', false);
		const saving = form.save();
		await flush();
		expect(await form.undo()).toEqual({ ok: false, reason: 'busy' });
		expect(await form.removeSecret('secrets.token')).toEqual({ ok: false, reason: 'busy' });
		release();
		await saving;
		expect(patches()).toHaveLength(2);
	});

	it('offers no Undo for a save of a secret alone', async () => {
		form.set('secrets.token', SECRET);
		expect(await form.save()).toMatchObject({ ok: true });
		expect(form.undoUntil).toBeNull();
		expect(form.status).toBe('saved');
		vi.advanceTimersByTime(10_000);
		expect(form.status).toBe('ready');
	});

	it('leaves secrets out of Undo and says so', async () => {
		form.set('secrets.token', SECRET);
		form.set('crawler.intensity', 'low');
		await form.save();
		expect(form.undoSkipsSecrets).toBe(true);
		await form.undo();
		expect(patches()[1]!.body).toEqual({ crawler: { if_revision: 4, intensity: null } });
	});
});

describe('secrets', () => {
	it('sends a replacement only when set, and clears it after every save attempt', async () => {
		form.set('secrets.token', SECRET);
		expect(form.dirtyCount).toBe(1);
		vi.advanceTimersByTime(300);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test') ?? '').not.toContain(SECRET);
		server.next('PATCH /settings', server.err(500, 'internal'));
		await form.save();
		expect(patches()[0]!.body).toEqual({ secrets: { if_revision: 1, token: SECRET } });
		expect(form.dirtyCount).toBe(0);
		expect(form.field('secrets.token').value).toBeNull();
		expect(JSON.stringify(form.errors)).not.toContain(SECRET);
		form.set('secrets.token', SECRET);
		form.set('crawler.timeout_secs', 0); // invalid: nothing sent, the secret still cleared
		expect(await form.save()).toEqual({ ok: false, reason: 'invalid' });
		expect(form.field('secrets.token').dirty).toBe(false);
	});

	it('never stages a secret’s reset into Save', async () => {
		form.reset('secrets.token');
		expect(form.dirtyCount).toBe(0);
		expect(await form.save()).toEqual({ ok: false, reason: 'nothing' });
	});

	it('never writes a secret to the console, on any path', async () => {
		const spies = (['log', 'info', 'warn', 'error', 'debug'] as const).map((k) =>
			vi.spyOn(console, k).mockImplementation(() => {})
		);
		for (const reply of [
			server.err(500, 'internal'),
			server.err(400, 'unknown_field', { section: 'secrets', field: 'token' }),
			server.err(422, 'invalid_setting', { section: 'secrets', field: 'token' }),
			{ status: 0, body: null },
			{ status: 200, body: { sections: {} } }
		]) {
			server.next('PATCH /settings', reply);
			form.set('secrets.token', SECRET);
			await form.save();
		}
		const logged = JSON.stringify(spies.flatMap((s) => s.mock.calls));
		expect(logged).not.toContain(SECRET);
		expect(JSON.stringify(form.errors)).not.toContain(SECRET);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test') ?? '').not.toContain(SECRET);
	});

	it('removes a secret on its own, as one PATCH to null', async () => {
		form.set('crawler.intensity', 'low');
		expect(await form.removeSecret('secrets.token')).toMatchObject({ ok: true });
		expect(patches().map((p) => p.body)).toEqual([{ secrets: { if_revision: 1, token: null } }]);
		expect(form.field('secrets.token').secretSet).toBe(false);
		expect(form.dirtyCount).toBe(1); // the staged edit isn't part of it
		expect(await form.removeSecret('crawler.intensity')).toEqual({ ok: false, reason: 'nothing' });
	});
});

describe('events', () => {
	it('refreshes a clean section in place, quietly', async () => {
		const rev = server.change('transport', { utp: { value: false, source: 'db' } });
		form.received({ section: 'transport', revision: rev });
		await flush();
		expect(form.field('transport.utp')).toMatchObject({ value: false, dirty: false });
		expect(form.announcement?.text).toBe('Updated elsewhere.');
	});

	it('ignores a revision at or below the known one, and other sections', async () => {
		const before = server.calls.length;
		form.received({ section: 'transport', revision: 7 });
		form.received({ section: 'transport', revision: 2 });
		form.received({ section: 'search', revision: 99 });
		await flush();
		expect(server.calls.length).toBe(before);
	});

	it('merges a section with staged edits', async () => {
		form.set('crawler.intensity', 'low');
		form.set('crawler.timeout_secs', 30);
		const rev = server.change('crawler', {
			intensity: { value: 'aggressive', source: 'db' },
			max_sockets: { value: 9, source: 'db' }
		});
		form.received({ section: 'crawler', revision: rev });
		await flush();
		expect(form.conflicts).toEqual(['crawler.intensity']);
		expect(form.field('crawler.max_sockets').value).toBe(9);
		expect(form.field('crawler.timeout_secs')).toMatchObject({ value: 30, dirty: true });
		expect(patches()).toHaveLength(0); // never re-saved
	});

	it('holds events during its own save and drops those it caused', async () => {
		const release = server.hold('PATCH /settings');
		form.set('crawler.intensity', 'low');
		const saving = form.save();
		await flush();
		form.received({ section: 'crawler', revision: 4 }); // our own save's event
		release();
		await saving;
		await flush();
		expect(server.requests('GET', '/settings/crawler')).toHaveLength(1);
	});

	it('refetches after its save for a held event newer than the response', async () => {
		const release = server.hold('PATCH /settings');
		form.set('crawler.intensity', 'low');
		const saving = form.save();
		await flush();
		form.received({ section: 'crawler', revision: 9 });
		release();
		await saving;
		await flush();
		expect(server.requests('GET', '/settings/crawler')).toHaveLength(2);
	});

	it('refetches every section on resync', async () => {
		await form.resync();
		for (const s of ['crawler', 'transport', 'network', 'logging', 'content', 'secrets']) {
			expect(server.requests('GET', `/settings/${s}`), s).toHaveLength(2);
		}
	});

	it('counts fields the server dropped', async () => {
		form.set('crawler.intensity', 'low');
		const s = server.sections.get('crawler')!;
		delete s.fields.intensity;
		s.revision++;
		form.received({ section: 'crawler', revision: s.revision });
		await flush();
		expect(form.announcement?.text).toMatch(/no longer exist were left out of your changes: 1\./);
		expect(form.dirtyCount).toBe(0);
	});
});

describe('preview', () => {
	const previews = () => server.requests('POST', '/settings/preview');

	it('previews the automatic value on load, and the dirty batch 300 ms after the last change', async () => {
		vi.advanceTimersByTime(300);
		await flush();
		expect(previews().map((p) => p.body)).toEqual([
			{ crawler: { if_revision: 3, max_sockets: 0 } }
		]);
		form.set('network.http_bind', '0.0.0.0:9001');
		vi.advanceTimersByTime(200);
		form.set('network.http_bind', '0.0.0.0:9002');
		vi.advanceTimersByTime(299);
		expect(previews()).toHaveLength(1);
		vi.advanceTimersByTime(1);
		await flush();
		expect(previews()).toHaveLength(2);
		expect(previews()[1]!.body).toEqual({
			crawler: { if_revision: 3, max_sockets: 0 },
			network: { if_revision: 2, http_bind: '0.0.0.0:9002' }
		});
	});

	it('shows the field’s estimates and warnings', async () => {
		server.onPreview(() => ({
			status: 200,
			body: {
				sections: {
					crawler: {
						revision: 3,
						fields: {
							max_sockets: {
								estimates: [{ name: 'sockets', value: 400 }],
								restart_required: true,
								value: 0,
								warnings: []
							}
						}
					}
				}
			}
		}));
		vi.advanceTimersByTime(300);
		await flush();
		expect(form.field('crawler.max_sockets').preview).toEqual({
			restartRequired: true,
			estimates: [{ name: 'sockets', value: 400 }],
			warnings: []
		});
	});

	it('lets the latest preview win', async () => {
		vi.advanceTimersByTime(300);
		await flush();
		const release = server.hold('POST /settings/preview');
		let n = 0;
		server.onPreview(() => {
			n++;
			return {
				status: 200,
				body: {
					sections: {
						network: {
							revision: 2,
							fields: {
								http_bind: {
									estimates: [{ name: 'url', value: `v${n}` }],
									restart_required: true,
									value: null,
									warnings: []
								}
							}
						}
					}
				}
			};
		});
		form.set('network.http_bind', '0.0.0.0:9001');
		vi.advanceTimersByTime(300); // the first one is held
		await flush();
		form.set('network.http_bind', '0.0.0.0:9002');
		vi.advanceTimersByTime(300);
		await flush();
		release(); // the older one resolves last
		await flush();
		expect(form.field('network.http_bind').preview?.estimates).toEqual([
			{ name: 'url', value: 'v1' }
		]);
	});

	it('shows a refusal on the field without blocking Save', async () => {
		vi.advanceTimersByTime(300);
		await flush();
		server.onPreview(() =>
			server.err(422, 'invalid_setting', {
				section: 'network',
				field: 'http_bind',
				message: 'Port in use.'
			})
		);
		form.set('network.http_bind', '0.0.0.0:9001');
		vi.advanceTimersByTime(300);
		await flush();
		expect(form.field('network.http_bind').error).toBe('Port in use.');
		expect(form.errors).toEqual([]); // not in the summary
		form.gate('disruptive', async () => true);
		server.onPreview(() => ({ status: 200, body: { sections: {} } }));
		expect(await form.save()).toMatchObject({ ok: true });
	});
});

describe('drafts', () => {
	it('writes a draft 300 ms after the last edit and offers it to the next form', async () => {
		form.set('crawler.intensity', 'low');
		vi.advanceTimersByTime(299);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toBeNull();
		vi.advanceTimersByTime(1);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toContain('"low"');
		const next = open();
		await flush();
		expect(next.draft).toEqual({ fields: 1, dropped: 0 });
		expect(next.dirtyCount).toBe(0);
		next.restoreDraft();
		expect(next.draft).toBeNull();
		expect(next.field('crawler.intensity')).toMatchObject({ value: 'low', dirty: true });
	});

	it('drops an offered draft on Discard', async () => {
		form.set('crawler.intensity', 'low');
		vi.advanceTimersByTime(300);
		const next = open();
		await flush();
		next.dropDraft();
		expect(next.draft).toBeNull();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toBeNull();
	});

	it('doesn’t offer a draft that no longer differs from the server', async () => {
		form.set('crawler.intensity', 'low');
		vi.advanceTimersByTime(300);
		server.change('crawler', { intensity: { value: 'low', source: 'db' } });
		const next = open();
		await flush();
		expect(next.draft).toBeNull();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toBeNull();
	});

	it('clears the draft on a save that leaves nothing dirty, and on discard', async () => {
		form.set('crawler.intensity', 'low');
		vi.advanceTimersByTime(300);
		await form.save();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toBeNull();
		form.set('crawler.intensity', 'balanced');
		vi.advanceTimersByTime(300);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).not.toBeNull();
		form.discard();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toBeNull();
		expect(form.dirtyCount).toBe(0);
	});

	it('keeps an offered draft through a save and a discard until it’s answered', async () => {
		form.set('crawler.intensity', 'low');
		vi.advanceTimersByTime(300);
		const next = open();
		await flush();
		expect(next.draft).not.toBeNull();
		next.set('transport.utp', false);
		await next.save();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toContain('"low"');
		next.set('transport.utp', true);
		next.discard();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'test')).toContain('"low"');
		next.restoreDraft();
		expect(next.field('crawler.intensity')).toMatchObject({ value: 'low', dirty: true });
	});

	it('survives a draft naming an Object.prototype key', async () => {
		sessionStorage.setItem(
			DRAFT_PREFIX + 'proto',
			JSON.stringify({
				v: 1,
				at: 1,
				sections: { constructor: { revision: 1, base: { a: 1 }, mine: { a: 2 }, reset: [] } }
			})
		);
		const f = open({ id: 'proto' });
		await flush();
		expect(f.status).toBe('ready');
		expect(f.draft).toBeNull();
		f.received({ section: 'constructor', revision: 99 });
		f.received({ section: 'toString', revision: 99 });
		expect(f.field('constructor.fields' as never).locked).toBe(true);
	});

	it('keeps no drafts when turned off', async () => {
		const f = open({ id: 'nodraft', drafts: false });
		await flush();
		f.set('crawler.intensity', 'low');
		vi.advanceTimersByTime(300);
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'nodraft')).toBeNull();
	});
});

describe('instant mode', () => {
	let inst: SettingsForm;
	beforeEach(async () => {
		inst = open({ id: 'prefs', mode: 'instant' });
		await flush();
	});

	it('sends a toggle at once, as its own PATCH, and offers its Undo', async () => {
		inst.set('transport.utp', false);
		await flush();
		expect(patches().map((p) => p.body)).toEqual([{ transport: { if_revision: 7, utp: false } }]);
		expect(inst.field('transport.utp')).toMatchObject({
			value: false,
			dirty: false,
			undoUntil: Date.now() + 10_000
		});
		expect(await inst.undo('transport.utp')).toMatchObject({ ok: true });
		expect(patches()[1]!.body).toEqual({ transport: { if_revision: 8, utp: null } });
		expect(inst.field('transport.utp').undoUntil).toBeUndefined();
	});

	it('debounces text and numbers by 300 ms', async () => {
		inst.set('crawler.timeout_secs', 20);
		inst.set('crawler.timeout_secs', 25);
		await flush();
		expect(patches()).toHaveLength(0);
		vi.advanceTimersByTime(300);
		await flush();
		expect(patches().map((p) => p.body)).toEqual([
			{ crawler: { if_revision: 3, timeout_secs: 25 } }
		]);
		// Undo restores the previous saved value.
		await inst.undo('crawler.timeout_secs');
		expect(patches()[1]!.body).toEqual({ crawler: { if_revision: 4, timeout_secs: 12 } });
	});

	it('expires its Undo after 10 s', async () => {
		inst.set('transport.utp', false);
		await flush();
		vi.advanceTimersByTime(10_000);
		expect(inst.field('transport.utp').undoUntil).toBeUndefined();
		expect(await inst.undo('transport.utp')).toEqual({ ok: false, reason: 'nothing' });
	});

	it('puts the old value back on failure, with the error inline', async () => {
		server.next(
			'PATCH /settings',
			server.err(422, 'invalid_setting', { section: 'transport', field: 'utp', message: 'No.' })
		);
		inst.set('transport.utp', false);
		await flush();
		expect(inst.field('transport.utp')).toMatchObject({ value: true, dirty: false, error: 'No.' });
	});

	it('retries once with the new revision on 409 stale_revision', async () => {
		server.change('transport', {});
		inst.set('transport.utp', false);
		await flush();
		expect(patches().map((p) => p.body)).toEqual([
			{ transport: { if_revision: 7, utp: false } },
			{ transport: { if_revision: 8, utp: false } }
		]);
		expect(inst.field('transport.utp')).toMatchObject({ value: false, dirty: false });
	});

	it('holds events during its own change, so it never conflicts with itself', async () => {
		const release = server.hold('PATCH /settings', { applied: true });
		inst.set('crawler.timeout_secs', 20);
		vi.advanceTimersByTime(300);
		await flush();
		inst.set('crawler.timeout_secs', 25); // typed while the first change is in flight
		const rev = server.sections.get('crawler')!.revision; // already saved: 4
		expect(rev).toBe(4);
		inst.received({ section: 'crawler', revision: rev }); // its event, before the response
		await flush();
		release();
		await flush();
		vi.advanceTimersByTime(300);
		await flush();
		expect(inst.conflicts).toEqual([]);
		expect(inst.field('crawler.timeout_secs')).toMatchObject({ value: 25, dirty: false });
		expect(patches().map((p) => p.body)).toEqual([
			{ crawler: { if_revision: 3, timeout_secs: 20 } },
			{ crawler: { if_revision: 4, timeout_secs: 25 } }
		]);
	});

	it('gates danger classes and fails closed, putting the value back', async () => {
		vi.spyOn(console, 'error').mockImplementation(() => {});
		inst.set('logging.data', 'full');
		await flush();
		expect(patches()).toHaveLength(0);
		expect(inst.field('logging.data')).toMatchObject({ value: 'private', dirty: false });
		expect(inst.errors).toEqual([{ message: expect.stringMatching(/confirmation/) }]);
		const ask = vi.fn(async () => true);
		inst.gate('privacy', ask);
		inst.set('logging.data', 'full');
		await flush();
		expect(ask).toHaveBeenCalledTimes(1);
		expect(patches()).toHaveLength(1);
	});

	it('sends a secret only on an explicit commit, and clears it', async () => {
		inst.set('secrets.token', 'hun');
		inst.set('secrets.token', SECRET);
		vi.advanceTimersByTime(1_000);
		await flush();
		expect(patches()).toHaveLength(0); // never half-typed
		expect(await inst.save()).toMatchObject({ ok: true });
		expect(patches().map((p) => p.body)).toEqual([{ secrets: { if_revision: 1, token: SECRET } }]);
		expect(inst.field('secrets.token').dirty).toBe(false);
		expect(await inst.save()).toEqual({ ok: false, reason: 'nothing' });
	});

	it('keeps a double toggle: the change back is sent after the first', async () => {
		const release = server.hold('PATCH /settings');
		inst.set('transport.utp', false);
		await flush();
		inst.set('transport.utp', true); // back, while the first is in flight
		release();
		await flush();
		await flush();
		expect(patches().map((p) => p.body)).toEqual([
			{ transport: { if_revision: 7, utp: false } },
			{ transport: { if_revision: 8, utp: true } }
		]);
		expect(inst.field('transport.utp')).toMatchObject({ value: true, dirty: false });
	});

	it('offers no Undo for a secret', async () => {
		inst.set('secrets.token', SECRET);
		await inst.save();
		expect(patches()).toHaveLength(1);
		expect(inst.field('secrets.token').undoUntil).toBeUndefined();
		expect(await inst.undo('secrets.token')).toEqual({ ok: false, reason: 'nothing' });
		expect(patches()).toHaveLength(1);
	});

	it('has no drafts, and Save does nothing', async () => {
		inst.set('crawler.timeout_secs', 20);
		vi.advanceTimersByTime(300);
		await flush();
		expect(sessionStorage.getItem(DRAFT_PREFIX + 'prefs')).toBeNull();
		expect(await inst.save()).toEqual({ ok: false, reason: 'nothing' });
	});
});
