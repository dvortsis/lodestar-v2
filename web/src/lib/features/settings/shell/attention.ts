// The Overview's "Needs attention" rows (§10.10.3): what the shell itself knows (a restart is
// waiting, the DHT port is closed, an address change needs an answer) and what each registered
// screen adds through its own `attention()`. Each provider runs on its own, so one that fails
// never hides the others. Loaded when the Overview or a badge needs it.
import { ApiError, api, isAbort } from '$lib/api/client';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import type { FieldKey } from '../form/types';
import type { AttentionItem, Registry } from './registry';
import type { SystemInfo } from './system';

export interface Attention {
	items: AttentionItem[];
	/** The network check couldn't be read, so its row is left out (the page says so once). */
	partial: boolean;
}

const RANK = { problem: 0, restart: 1, notice: 2 } as const;

/** Problems first, then restarts, then notices; within one, by the screens' order. */
export function sortItems(items: AttentionItem[], registry: Registry): AttentionItem[] {
	const order = new Map(registry.screens.map((r, i) => [r.screen.id, i]));
	const at = (id: string) => order.get(id) ?? registry.screens.length;
	return items
		.map((item, i) => ({ item, i }))
		.sort(
			(a, b) =>
				RANK[a.item.severity] - RANK[b.item.severity] ||
				at(a.item.screen) - at(b.item.screen) ||
				a.i - b.i
		)
		.map(({ item }) => item);
}

async function dhtPort(signal: AbortSignal): Promise<number | undefined> {
	const network = await api.get('/settings/{section}', { path: { section: 'network' }, signal });
	const port = network.fields.dht_port?.value;
	return typeof port === 'number' ? port : undefined;
}

export async function collectAttention(input: {
	system: SystemInfo;
	registry: Registry;
	signal: AbortSignal;
}): Promise<Attention> {
	const { system, registry, signal } = input;
	const items: AttentionItem[] = [];
	let partial = false;

	const waiting = system.restart_required;
	if (waiting.length > 0) {
		const count = waiting.length;
		items.push({
			id: 'restart',
			screen: registry.screenOf(waiting[0] as FieldKey)?.screen.id ?? 'general',
			severity: 'restart',
			text: () =>
				count === 1
					? m.settings_ui_attn_restart_one()
					: m.settings_ui_attn_restart({ count: formatCount(count, currentLocale()) }),
			action: system.restart.available
				? {
						label: () => m.settings_ui_attn_restart_now(),
						run: async () => (await import('./restart-flow')).restartNow(system)
					}
				: {
						label: () => m.settings_ui_attn_restart_how(),
						href: '#settings-restart-banner'
					}
		});
	}

	const change = system.address_change;
	if (change?.state === 'awaiting_confirmation') {
		items.push({
			id: 'address-confirm',
			screen: 'network',
			severity: 'problem',
			text: () => m.settings_ui_attn_address_wait({ url: change.previous_url }),
			action: { label: () => m.settings_ui_attn_review(), href: '/settings/network' }
		});
	} else if (change?.state === 'reverted') {
		items.push({
			id: 'address-reverted',
			screen: 'network',
			severity: 'notice',
			text: () => m.settings_ui_attn_address_reverted({ url: change.previous_url }),
			action: {
				label: () =>
					m.settings_ui_attn_open({ screen: registry.get('network')?.screen.title() ?? 'Network' }),
				href: '/settings/network'
			}
		});
	}

	const jobs: Promise<void>[] = [
		(async () => {
			try {
				const check = await api.get('/network/check', { signal });
				if (check.reachable === false) {
					// The configured port is what the user changes; the one others see is the fallback.
					const port =
						(await dhtPort(signal).catch(() => undefined)) ?? check.external.port ?? undefined;
					if (port !== undefined) {
						items.push({
							id: 'dht-unreachable',
							screen: 'network',
							severity: 'problem',
							text: () => m.settings_ui_attn_unreachable({ port: String(port) }),
							action: {
								label: () =>
									m.settings_ui_attn_open({
										screen: registry.get('network')?.screen.title() ?? 'Network'
									}),
								href: '/settings/network'
							}
						});
					}
				}
			} catch (e) {
				if (!isAbort(e) && !(e instanceof ApiError && e.status === 401)) partial = true;
			}
		})(),
		...registry.screens.map(async (r) => {
			try {
				items.push(...((await r.screen.attention?.({ system, signal })) ?? []));
			} catch {
				// A row that couldn't load never hides the others.
			}
		})
	];
	await Promise.all(jobs);
	return { items: sortItems(items, registry), partial };
}
