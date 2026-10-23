// "Restart now" and the address keep-or-revert calls (§10.10.6), loaded when used. The restart
// asks first (`ask()`: crawling pauses), then `POST /system/restart`, then hands over to the
// full-page waiting state (`RestartingScreen.svelte`) through `flow`.
import { ApiError, api } from '$lib/api/client';
import { formatSpan } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { toast } from '$lib/ui/feedback/toasts.svelte';
import { ask } from '../form/dialogs';
import type { SystemInfo } from './system';
import { startWaiting } from './flow.svelte';

/** Asks, then restarts. Resolves once the request is answered (the waiting state takes over). */
export async function restartNow(system: SystemInfo, signal?: AbortSignal): Promise<void> {
	const time = formatSpan(system.restart.estimated_seconds, currentLocale());
	const answer = await ask(
		{
			title: m.settings_ui_confirm_title(),
			body: [m.settings_ui_confirm_body({ time })],
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				{ id: 'restart', label: m.settings_ui_confirm_restart(), variant: 'danger' }
			],
			initial: 'cancel'
		},
		signal
	);
	if (answer !== 'restart') return;
	try {
		const accepted = await api.post('/system/restart');
		// A pending address change moves the page to the new address once it answers.
		const change = system.address_change;
		startWaiting({
			to: change?.state === 'awaiting_restart' ? change.url : undefined,
			estimate: accepted.estimated_seconds,
			before: system.started_at
		});
	} catch (e) {
		if (e instanceof ApiError && e.code === 'restart_unavailable') {
			toast.push({
				kind: 'error',
				text: m.settings_ui_restart_unavailable({ hint: e.message })
			});
		} else toast.push({ kind: 'error', text: m.settings_ui_restart_failed() });
	}
}

/** `POST /system/address/keep`: the new address stays. Returns the change as it is now. */
export const keepAddress = () => api.post('/system/address/keep');

/**
 * `POST /system/address/revert`. When the server restarts onto the previous address, the waiting
 * state moves the page there.
 */
export async function revertAddress(system: SystemInfo) {
	const outcome = await api.post('/system/address/revert');
	const previous = system.address_change?.previous_url;
	if (outcome.restarting && previous) {
		startWaiting({
			to: previous,
			estimate: system.restart.estimated_seconds,
			before: system.started_at
		});
	}
	return outcome;
}
