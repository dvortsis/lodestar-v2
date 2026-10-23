// The lockout dialog (§10.10.6): Network's own `form.gate('disruptive', …)`. It wins over the
// engine's default dialog. A change of the web address names the exact new address, says what
// happens after the restart and that nobody is locked out for good; a DHT change alone names its
// effect from the preview's warnings.
import { m } from '$lib/paraglide/messages.js';
import { ask } from '../form/dialogs';
import type { GateContext } from '../form/types';

/** The address the preview says the UI will be at, when it says one. */
export function previewUrl(c: GateContext): string | undefined {
	const p = c.preview.sections.network?.fields.http_bind;
	const url = p?.estimates.find((e) => e.name === 'url')?.value;
	return typeof url === 'string' ? url : undefined;
}

function warnings(c: GateContext): string[] {
	const lines: string[] = [];
	for (const key of c.fields) {
		const name = key.slice(key.indexOf('.') + 1);
		for (const w of c.preview.sections.network?.fields[name]?.warnings ?? []) lines.push(w.message);
	}
	return [...new Set(lines)];
}

/** The dialog's text, apart from asking, so it can be tested without a DOM. */
export function lockoutText(c: GateContext): { title: string; body: string[] } {
	if (c.fields.includes('network.http_bind')) {
		const url = previewUrl(c);
		return {
			title: m.settings_ui_lockout_title(),
			body: [
				url ? m.settings_ui_lockout_url({ url }) : m.settings_ui_lockout_url_unknown(),
				m.settings_ui_lockout_confirm(),
				...warnings(c)
			]
		};
	}
	const lines = warnings(c);
	const port = c.fields.includes('network.dht_port');
	const off = c.fields.includes('network.dht_enabled');
	return {
		title: m.settings_ui_dht_title(),
		body: lines.length
			? lines
			: [port ? m.settings_ui_dht_port() : '', off ? m.settings_ui_dht_toggle() : ''].filter(
					Boolean
				)
	};
}

/** The handler to register with `form.gate('disruptive', …)`. */
export function lockoutGate(signal?: AbortSignal) {
	return async (c: GateContext): Promise<boolean> => {
		const { title, body } = lockoutText(c);
		const answer = await ask(
			{
				title,
				body,
				actions: [
					{ id: 'cancel', label: m.settings_form_cancel() },
					{ id: 'save', label: m.settings_form_save(), variant: 'primary' }
				],
				initial: 'cancel'
			},
			signal
		);
		return answer === 'save';
	};
}
