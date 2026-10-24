// The form's own questions (§10.10.5, §10.10.7): discard, leave, and the default disruptive
// confirmation. The dialog itself (`ConfirmDialog.svelte`) loads when one is first asked.
import { m } from '$lib/paraglide/messages.js';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { ask } from './dialogs';
import { hardNavigate } from './leave';
import type { SettingsForm } from './form.svelte';
import { fieldLabel } from './messages';
import type { GateContext } from './types';

export const askDiscard = (count: string, signal: AbortSignal) =>
	ask(
		{
			title: m.settings_form_discard_title({ count }),
			body: [m.settings_form_discard_body()],
			actions: [
				{ id: 'keep', label: m.settings_form_keep_editing() },
				{ id: 'discard', label: m.settings_form_discard(), variant: 'danger' }
			],
			initial: 'keep'
		},
		signal
	);

/**
 * Asks Save / Discard / Keep editing before leaving with unsaved changes, then continues to
 * `to` the way the user was going: `navigate` (the router, past the guard) or a full page load.
 */
export async function askToLeave(
	form: SettingsForm,
	to: URL,
	unload: boolean,
	signal: AbortSignal,
	navigate: (href: string) => Promise<void>
): Promise<void> {
	const answer = await askLeave(formatCount(form.dirtyCount, currentLocale()), signal);
	if (answer === 'save') {
		// Nothing to save any more (a merge or ⌘S settled it meanwhile) is as good as saved.
		const r = await form.save();
		if (!r.ok && r.reason !== 'nothing') return;
	} else if (answer === 'discard') form.discard();
	else return;
	// Another site, or a link that reloads the page: the router can't take it there. Nothing is
	// unsaved by now, so the unload passes the guard without a bypass.
	if (unload || to.origin !== location.origin) hardNavigate(to.href);
	else await navigate(to.href);
}

const askLeave = (count: string, signal: AbortSignal) =>
	ask(
		{
			title: m.settings_form_leave_title(),
			body: [m.settings_form_leave_body({ count })],
			actions: [
				{ id: 'keep', label: m.settings_form_keep_editing() },
				{ id: 'discard', label: m.settings_form_discard() },
				{ id: 'save', label: m.settings_form_save(), variant: 'primary' }
			],
			initial: 'save'
		},
		signal
	);

function effectLines(c: GateContext): string[] {
	const lines: string[] = [];
	for (const key of c.fields) {
		const [section, name] = [key.slice(0, key.indexOf('.')), key.slice(key.indexOf('.') + 1)];
		const p = c.preview.sections[section]?.fields[name];
		for (const e of p?.estimates ?? []) {
			if (e.name === 'url' && typeof e.value === 'string') {
				lines.push(m.settings_form_disruptive_url({ url: e.value }));
			}
		}
		for (const w of p?.warnings ?? []) lines.push(w.message);
	}
	return [...new Set(lines)];
}

/** The default disruptive confirmation: which fields, and what the preview says they'll do. */
export async function askDisruptive(
	form: SettingsForm,
	c: GateContext,
	signal: AbortSignal
): Promise<boolean> {
	const labels = c.fields
		.map((k) => form.specs.find((s) => s.key === k))
		.filter((s) => s !== undefined)
		.map((s) => fieldLabel(s));
	const effects = effectLines(c);
	const answer = await ask(
		{
			title: m.settings_form_disruptive_title(),
			body: [
				m.settings_form_disruptive_fields({ fields: labels.join(', ') }),
				...(effects.length ? effects : [m.settings_form_disruptive_generic()])
			],
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				{ id: 'save', label: m.settings_form_save(), variant: 'primary' }
			],
			initial: 'cancel'
		},
		signal
	);
	return answer === 'save';
}
