// Settings → Updates' state: what the server last said, and the requests that change it. A failed or
// refused request leaves `status` and `settings` as they were and sets `error`; nothing here says
// "installed" or "up to date" without a response that does.
import { api, ApiError } from '$lib/api/client';
import { events } from '$lib/state/events.svelte';
import { isLevel, isMode, isStep, type Level, type Mode, type Status, type Step } from './logic';

export interface Settings {
	mode: Mode;
	level: Level;
	/** The saved sentinel: `auto`, `on` or `off`. */
	securityFixes: string;
	revision: number;
	/** Every field's value as the server last said it (Advanced reads these). */
	values: Record<string, unknown>;
	/** The reason a field is locked (the environment variable), by field name. */
	locks: Record<string, string | 'read_only'>;
}

export type Failure =
	| 'load'
	| 'check'
	| 'pause'
	| 'skip'
	| 'clear'
	| 'setting'
	| 'locked'
	| 'stale'
	| 'busy'
	| 'install'
	| 'install_unknown_version'
	| 'install_version_skipped'
	| 'install_unavailable'
	| 'install_manual_confirmation'
	| 'install_not_saved'
	| 'install_failed';

/** `POST /updates/install` refusals by their code; anything else is the general `install` failure. */
const INSTALL_ANSWERS: Record<string, Failure> = {
	unknown_version: 'install_unknown_version',
	version_skipped: 'install_version_skipped',
	install_unavailable: 'install_unavailable',
	manual_confirmation_required: 'install_manual_confirmation',
	updates_not_saved: 'install_not_saved'
};

const SECTION = { path: { section: 'updates' } };

export class UpdatesStore {
	status = $state<Status>();
	settings = $state<Settings>();
	/** The container image option is offered where the deployment is Docker or Unraid. */
	imageOffered = $state(false);
	/** The install step under way, from the `updates` SSE topic. */
	step = $state<Step | null>(null);
	progress = $state<{ done: number | null; total: number | null }>({ done: null, total: null });
	loading = $state(true);
	busy = $state<string | null>(null);
	error = $state<Failure | null>(null);
	/** A server-worded reason for the last refusal, when it gave one. */
	reason = $state('');
	/** `GET /updates` said this process serves no update engine. */
	unavailable = $state(false);
	/** The version the live install step concerns. */
	stepVersion = $state<string | null>(null);
	/** Aborted on leave: closes a confirmation still open. */
	private ctl = new AbortController();
	readonly signal = this.ctl.signal;
	private off: (() => void) | undefined;
	private live = true;
	/** A response only replaces the status when no newer one has been taken since it was asked. */
	private seq = 0;

	start() {
		this.live = true;
		void this.load();
		this.off = events.on(
			'updates',
			(e) => {
				this.step = isStep(e.state) ? e.state : null;
				this.stepVersion = this.step ? e.version : null;
				this.progress = { done: e.bytes_done, total: e.bytes_total };
				if (e.state === 'failed') {
					// An install that was accepted and then failed: its stable code, never a URL.
					this.error = 'install_failed';
					this.reason = e.error ?? '';
				}
				if (!isStep(e.state)) void this.refresh();
			},
			{
				onResync: () => {
					// The stream was lost: what it said about a step is no longer known.
					this.step = null;
					void this.refresh();
				}
			}
		);
	}

	stop() {
		this.live = false;
		this.ctl.abort();
		this.off?.();
	}

	async load() {
		this.loading = true;
		await Promise.all([this.refresh(), this.loadSettings(), this.loadDeployment()]);
		this.loading = false;
	}

	/** Takes `GET /updates`; on failure the last status stays. */
	async refresh() {
		const ticket = this.seq;
		try {
			const s = await api.get('/updates');
			if (ticket === this.seq) {
				this.take(s);
				this.unavailable = false;
			}
		} catch (e) {
			if (e instanceof ApiError && e.code === 'updates_unavailable') this.unavailable = true;
			else this.fail('load', e);
		}
	}

	private take(s: Status) {
		this.seq++;
		if (this.live) this.status = s;
	}

	private async loadSettings() {
		try {
			const r = await api.get('/settings/{section}', SECTION);
			const f = r.fields;
			const locks: Settings['locks'] = {};
			for (const [name, v] of Object.entries(f)) {
				if (v.env) locks[name] = v.env;
				else if (v.read_only) locks[name] = 'read_only';
			}
			if (!isMode(f.mode?.value) || !isLevel(f.level?.value)) throw new Error('settings shape');
			this.settings = {
				mode: f.mode.value,
				level: f.level.value,
				securityFixes: String(f.security_fixes?.value ?? 'auto'),
				revision: r.revision,
				values: Object.fromEntries(Object.entries(f).map(([k, v]) => [k, v.value])),
				locks
			};
		} catch (e) {
			this.fail('load', e);
		}
	}

	private async loadDeployment() {
		try {
			const kind = (await api.get('/system')).deployment.kind;
			this.imageOffered = kind === 'docker' || kind === 'unraid';
		} catch {
			this.imageOffered = false;
		}
	}

	private fail(kind: Failure, e: unknown) {
		if (e instanceof ApiError && e.status === 401) return; // the shell takes the user to sign in
		this.error = kind;
		this.reason = e instanceof ApiError ? e.message : '';
		if (kind === 'install' && e instanceof ApiError) {
			this.error = INSTALL_ANSWERS[e.code] ?? 'install';
			// `install_unavailable` names its reason in parentheses: "(busy)".
			this.reason =
				e.code === 'install_unavailable' ? (/\(([a-z_]+)\)/.exec(e.message)?.[1] ?? '') : '';
		}
	}

	private async run(name: string, kind: Failure, f: () => Promise<Status | void>) {
		if (this.busy) {
			this.error = 'busy';
			this.reason = '';
			return false;
		}
		this.busy = name;
		this.error = null;
		this.reason = '';
		try {
			const s = await f();
			if (s) this.take(s);
			return true;
		} catch (e) {
			this.fail(kind, e);
			return false;
		} finally {
			this.busy = null;
		}
	}

	check = () => this.run('check', 'check', () => api.post('/updates/check', {}));
	pause = (days: number) =>
		this.run('pause', 'pause', () => api.post('/updates/pause', { body: { days } }));
	skip = (version: string) =>
		this.run('skip', 'skip', () => api.post('/updates/skip', { body: { version } }));
	/**
	 * Starts an install now. `confirmManual` is true only after the person confirmed the release's
	 * manual steps. The 202 answer carries the status; the steps arrive on the `updates` topic.
	 */
	install = async (version: string, confirmManual = false) => {
		return this.run('install', 'install', () =>
			api.post('/updates/install', {
				body: { version, when: 'now', confirm_manual: confirmManual }
			})
		);
	};
	clearHistory = () =>
		this.run('clear', 'clear', async () => {
			await api.post('/updates/history/clear', {});
			await this.refresh();
		});

	/** Saves one setting; the server's value comes back with the new revision. A locked one is refused here. */
	async set(name: string, value: unknown) {
		if (!this.settings || this.settings.locks[name]) return false;
		const revision = this.settings.revision;
		const ok = await this.run('setting', 'setting', async () => {
			try {
				await api.patch('/settings/{section}', {
					...SECTION,
					body: { if_revision: revision, [name]: value } as never
				});
			} catch (e) {
				if (e instanceof ApiError && e.status === 409) {
					await this.loadSettings();
					// A reload that failed already said so; keep that rather than hide it.
					if (this.error === null) this.error = e.code === 'stale_revision' ? 'stale' : 'locked';
					return;
				}
				throw e;
			}
			await this.loadSettings();
			await this.refresh();
		});
		return ok && this.error === null;
	}
}
