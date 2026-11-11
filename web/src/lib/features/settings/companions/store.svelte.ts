// Settings → Companions' state: the list as the server last said it, and the requests that change
// it. A failed or refused request leaves the list as it was; nothing here says "installed" or
// "removed" without a response that does. The server decides every install: the request is built
// only from the offer it sent.
import { api, ApiError } from '$lib/api/client';
import type {
	Companion,
	Detected,
	InstallOffer,
	RemovalPlan,
	Template,
	TemplateRequest
} from './logic';

export class CompanionsStore {
	items = $state<Companion[]>([]);
	loading = $state(true);
	/** Companions are off in this process (`503 companions_unavailable`). */
	unavailable = $state(false);
	loadFailed = $state(false);
	/** What the last detection found. */
	detected = $state<Detected[] | null>(null);
	private ctl = new AbortController();
	readonly signal = this.ctl.signal;
	private live = true;
	/** One health watch per companion, by id. Not reactive: nothing renders it. */
	private timers: Record<string, ReturnType<typeof setTimeout>> = {};

	/** After a restart, looks at the row every few seconds (up to a minute) so its health shows. */
	private watch(id: string, left = 20) {
		clearTimeout(this.timers[id]); // a newer restart replaces the watch, there is only ever one per companion
		delete this.timers[id];
		if (!this.live || left === 0) return;
		const t = setTimeout(async () => {
			delete this.timers[id];
			await this.refresh(id);
			if (this.items.find((x) => x.id === id)?.health !== 'healthy') this.watch(id, left - 1);
		}, 3000);
		this.timers[id] = t;
	}

	start() {
		this.live = true;
		void this.load();
	}
	stop() {
		this.live = false;
		this.ctl.abort();
		for (const t of Object.values(this.timers)) clearTimeout(t);
		this.timers = {};
	}

	async load() {
		try {
			const r = await api.get('/companions');
			if (!this.live) return;
			this.items = r.companions;
			this.unavailable = false;
			this.loadFailed = false;
		} catch (e) {
			if (!this.live) return;
			if (e instanceof ApiError && e.code === 'companions_unavailable') this.unavailable = true;
			else if (!(e instanceof ApiError && e.status === 401)) this.loadFailed = true;
		}
		this.loading = false;
	}

	/** Takes one row the server returned in answer to a change. */
	private take(c: Companion) {
		if (!this.live) return;
		this.items = this.items.map((x) => (x.id === c.id ? c : x));
	}

	async refresh(id: string) {
		try {
			this.take(await api.get('/companions/{id}', { path: { id } }));
		} catch {
			/* the list stays as it was */
		}
	}

	offer = (id: string): Promise<InstallOffer> =>
		api.get('/companions/{id}/install', { path: { id } });

	/** Installs what `offer` said, echoing its version and SHA-256 and nothing else. */
	async install(offer: InstallOffer) {
		this.take(
			await api.post('/companions/{id}/install', {
				path: { id: offer.id },
				body: { version: offer.version, sha256: offer.sha256 }
			})
		);
	}

	removalPlan = (id: string): Promise<RemovalPlan> =>
		api.get('/companions/{id}/remove', { path: { id } });

	async remove(id: string) {
		const r = await api.post('/companions/{id}/remove', { path: { id }, body: { confirm: id } });
		await this.load();
		return r;
	}

	async restart(id: string) {
		await api.post('/companions/{id}/restart', { path: { id } });
		await this.refresh(id);
		this.watch(id);
	}

	async logs(id: string): Promise<string[]> {
		return (await api.get('/companions/{id}/logs', { path: { id } })).lines;
	}

	async detect() {
		this.detected = (await api.post('/companions/detect', {})).found;
	}

	/** Adds a companion at a target the server found, or at an address typed in. */
	async addAt(id: string, target: Detected['target']) {
		await api.post('/companions/{id}/target', { path: { id }, body: target });
		await this.load();
	}

	template = (id: string, body: TemplateRequest): Promise<Template> =>
		api.post('/companions/{id}/template', { path: { id }, body });

	/** The reveal's response alone carries the value; the caller shows it once and drops it. */
	async reveal(id: string, field: string, password: string): Promise<string> {
		const r = await api.post('/companions/{id}/credentials/{field}/reveal', {
			path: { id, field },
			body: { password }
		});
		return r.value;
	}

	regenerate = async (id: string, field: string) => {
		await api.post('/companions/{id}/credentials/{field}/regenerate', { path: { id, field } });
	};

	/** Sends a pasted secret to be sealed. Nothing is kept here or echoed back. */
	async paste(id: string, value: string) {
		await api.post('/companions/{id}/paste', { path: { id }, body: { value } });
		await this.refresh(id);
	}
}
