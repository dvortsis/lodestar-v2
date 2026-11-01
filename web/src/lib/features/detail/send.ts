// "Send to client" (§10.5, §10.16 `s`): the one module the buttons, the row menu and the key load
// on demand. Whether a client is configured is read once, lazily, from the saved settings
// (`integrations.clients` and `integrations.default_client`); nothing is fetched on first paint.
import { ApiError, api } from '$lib/api/client';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { m } from '$lib/paraglide/messages.js';
import { commandTarget } from './target';

// `s` (§10.16) exists only once a client is known: registered here, so with none it does nothing.
let registered = false;
function registerKey() {
	if (registered) return;
	registered = true;
	void import('$lib/keys').then(({ keys }) =>
		keys.register({
			id: 'detail.send',
			title: () => m.shortcuts_action_detail_send(),
			section: () => m.torrent(),
			keys: ['s'],
			when: () => commandTarget() !== null,
			run: () => {
				const hash = commandTarget();
				if (hash) void sendHash(hash);
			}
		})
	);
}

let known: Promise<string | null> | undefined;

/** The default client's name (else the first's), or null when none is saved or it can't be read. */
export function clientName(): Promise<string | null> {
	known ??= api
		.get('/settings/{section}', { path: { section: 'integrations' } })
		.then((res) => {
			const f = res.fields as Record<string, { value?: unknown } | undefined>;
			const list = Array.isArray(f.clients?.value) ? (f.clients.value as unknown[]) : [];
			const clients = list.filter(
				(c): c is { id?: unknown; name?: unknown } => typeof c === 'object' && c !== null
			);
			const pick = clients.find((c) => c.id === f.default_client?.value) ?? clients[0];
			const name = pick ? String(pick.name || pick.id || '') : '';
			return name || null;
		})
		.catch(() => {
			known = undefined; // offline or signed out: the next ask tries again
			return null;
		})
		.then((name) => (name && registerKey(), name));
	return known;
}

/** The button's and the menu item's text ("Send to qBittorrent"), or null with no client. */
export const sendLabel = () => clientName().then((name) => name && m.detail_send({ name }));

/** Send one torrent to the default client and say what happened. No retries. */
export async function sendHash(hash: string) {
	const name = (await clientName()) ?? '';
	const fail = (text: string) => toast.push({ kind: 'error', text });
	try {
		const { results } = await api.post('/send', { body: { hashes: [hash] } });
		const r = results[0];
		if (r?.outcome === 'sent') toast.push({ kind: 'success', text: m.detail_sent({ name }) });
		else if (r?.outcome === 'duplicate')
			toast.push({ kind: 'success', text: m.detail_send_duplicate({ name }) });
		else if (r?.message) fail(m.detail_send_refused({ name, reason: r.message }));
		else fail(m.detail_send_failed({ name }));
	} catch (e) {
		fail(
			e instanceof ApiError && e.status === 404
				? m.detail_send_no_client()
				: m.detail_send_failed({ name })
		);
	}
}
