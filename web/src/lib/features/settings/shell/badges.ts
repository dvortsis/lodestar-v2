// The navigation's badges (§10.10.2): icon-free text a screen's link carries when something there
// needs attention, from the same items the Overview lists. A problem wins over "Restart needed".
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import type { FieldKey } from '../form/types';
import type { AttentionItem, Registry } from './registry';

export interface Badge {
	/** The visible words ("1 problem", "Restart needed"). */
	text: string;
	/** Appended to the link's accessible name ("Network & VPN, 1 problem"). */
	label: string;
	kind: 'problem' | 'restart' | 'notice';
}

export function buildBadges(
	items: readonly AttentionItem[],
	restartKeys: readonly string[],
	registry: Registry
): Record<string, Badge> {
	const out: Record<string, Badge> = {};
	const problems = new Map<string, number>();
	for (const item of items) {
		if (item.severity === 'problem')
			problems.set(item.screen, (problems.get(item.screen) ?? 0) + 1);
	}
	for (const [screen, n] of problems) {
		const text =
			n === 1
				? m.settings_ui_badge_problem_one()
				: m.settings_ui_badge_problem({ count: formatCount(n, currentLocale()) });
		out[screen] = { text, label: text, kind: 'problem' };
	}
	for (const key of restartKeys) {
		const id = registry.screenOf(key as FieldKey)?.screen.id;
		if (id && !out[id]) {
			const text = m.settings_ui_badge_restart();
			out[id] = { text, label: text, kind: 'restart' };
		}
	}
	return out;
}
