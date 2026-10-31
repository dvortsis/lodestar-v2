// Settings → Logging's privacy changes (§10.10.7 Privacy, §12.1), loaded on the first one. Each is
// sent at once rather than staged: a deadline starts when you confirm it, and the switch shows on
// only once the server has saved it. So nothing turns on silently, a refused or failed request
// leaves the kind off, and the banner is reloaded from `GET /logs/catalog` after every change.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { formatBytes, formatCount } from '$lib/format';
import { m } from '$lib/paraglide/messages.js';
import { ask } from '../form/dialogs';
import type { SettingsForm } from '../form';
import type { LoggingCatalog } from './catalog.svelte';
import { kindLabel } from './labels';
import {
	OPT_INS,
	deadline,
	kindPatch,
	offPatch,
	presetPatch,
	type DataValues,
	type Duration,
	type LogKind,
	type OptIn
} from './rules';

export interface Context {
	catalog: LoggingCatalog;
	/** The screen's form, which merges the new revision; absent on `/logs`. */
	form?: SettingsForm;
}

async function saved(): Promise<DataValues & { consoleSameAsFile: boolean }> {
	const s = await api.get('/settings/{section}', { path: { section: 'logging' } });
	const f = (name: string) => s.fields[name]?.value;
	return {
		data: String(f('data') ?? 'private'),
		data_until: Number(f('data_until') ?? 0),
		kinds: { ...((f('kinds') as Record<string, number> | null) ?? {}) },
		consoleSameAsFile: f('console_detail') === 'file'
	};
}

async function send(c: Context, patch: Record<string, unknown>) {
	try {
		// `SettingsBatch` is untyped in the schema (form/requests.ts says why): cast here too.
		const body = { logging: patch } as unknown as components['schemas']['SettingsBatch'];
		const done = await api.patch('/settings', { body });
		const revision = done.sections.logging?.revision;
		// The form holds the events' settings: it merges the section's new revision.
		if (revision !== undefined) c.form?.received({ section: 'logging', revision });
	} finally {
		// Success or not, show what the server has now.
		await c.catalog.load();
	}
}

/**
 * Asks what will be recorded and for how long (§10.10.7). Focus starts on Cancel; 1 hour is the
 * main answer. Null when cancelled.
 */
async function askDuration(
	title: string,
	body: string[],
	untilOff: boolean
): Promise<Duration | null> {
	const answer = await ask({
		title,
		body,
		actions: [
			{ id: 'cancel', label: m.settings_form_cancel() },
			...(untilOff ? [{ id: 'off', label: m.settings_logging_for_until_off() }] : []),
			{ id: 'day', label: m.settings_logging_for_day() },
			{ id: 'hour', label: m.settings_logging_for_hour(), variant: 'primary' as const }
		],
		initial: 'cancel'
	});
	return answer === 'hour' || answer === 'day' || (untilOff && answer === 'off') ? answer : null;
}

function kindsBody(names: string, consoleSameAsFile: boolean): string[] {
	return [
		m.settings_logging_dialog_what({ kinds: names }),
		consoleSameAsFile ? m.settings_logging_dialog_console() : m.settings_logging_dialog_where(),
		m.settings_logging_dialog_ends()
	];
}

/** Turns a data kind on (an identifying one after the dialog) or off. False when cancelled. */
export async function setKind(c: Context, kind: LogKind, on: boolean): Promise<boolean> {
	const v = await saved();
	let until = 0;
	if (on && kind.identifying) {
		const d = await askDuration(
			m.settings_logging_dialog_title(),
			kindsBody(kindLabel(kind.kind), v.consoleSameAsFile),
			true
		);
		if (!d) return false;
		until = deadline(d, c.catalog.serverNow());
	}
	await send(c, kindPatch(v, kind, on, until, c.catalog.serverNow()));
	if (!on && kind.identifying) await offerDelete(c);
	return true;
}

/** Chooses a data preset; any but Private asks first. False when cancelled. */
export async function setPreset(c: Context, preset: string): Promise<boolean> {
	const v = await saved();
	let until = 0;
	if (preset !== 'private') {
		const names = (c.catalog.catalog?.kinds ?? [])
			.filter((k) => k.identifying && !k.opt_in && (preset === 'full' || k.kind === 'hash'))
			.map((k) => kindLabel(k.kind))
			.join(', ');
		const d = await askDuration(
			m.settings_logging_dialog_title(),
			kindsBody(names, v.consoleSameAsFile),
			true
		);
		if (!d) return false;
		until = deadline(d, c.catalog.serverNow());
	}
	await send(c, { ...presetPatch(preset, until) });
	if (preset === 'private') await offerDelete(c);
	return true;
}

/** Turns an opt-in on (after its own warning, 1 or 24 hours) or off. False when cancelled. */
export async function setOptIn(c: Context, kind: OptIn, on: boolean): Promise<boolean> {
	let until = 0;
	if (on) {
		const peer = kind === 'peer_activity';
		const d = await askDuration(
			peer ? m.settings_logging_optin_peer_title() : m.settings_logging_optin_filters_title(),
			[
				peer ? m.settings_logging_optin_peer_warning() : m.settings_logging_optin_filters_warning(),
				m.settings_logging_optin_where()
			],
			false
		);
		if (!d) return false;
		until = deadline(d, c.catalog.serverNow());
	}
	await send(c, { [OPT_INS[kind]]: until });
	if (!on) await offerDelete(c);
	return true;
}

/** "Turn off now" for the identifying kinds' banner. */
export async function turnOffIdentifying(c: Context): Promise<void> {
	const cat = c.catalog.catalog ?? (await c.catalog.load());
	if (!cat) throw new Error(m.settings_logging_unavailable());
	await send(c, { ...offPatch(await saved(), cat) });
	await offerDelete(c);
}

/** "Turn off now" on `/logs`: everything its one banner covers, identifying kinds and opt-ins. */
export async function turnOffAll(c: Context): Promise<void> {
	const cat = c.catalog.catalog ?? (await c.catalog.load());
	if (!cat) throw new Error(m.settings_logging_unavailable());
	await send(c, {
		...offPatch(await saved(), cat),
		peer_activity_until: 0,
		change_details_until: 0
	});
	await offerDelete(c);
}

/**
 * After kinds turn off, offers to delete the files written while they were on (§12.1), with the
 * count and size. Focus starts on Keep. Nothing to offer when no file has torrent details.
 */
export async function offerDelete(c: Context): Promise<number> {
	let files;
	try {
		files = (await api.get('/logs/files')).files.filter((f) => f.details || f.opt_in);
	} catch {
		return 0;
	}
	if (!files.length) return 0;
	const count = formatCount(files.length);
	const answer = await ask({
		title: m.settings_logging_delete_title(),
		body: [
			m.settings_logging_delete_body({
				count,
				size: formatBytes(files.reduce((n, f) => n + f.size, 0))
			})
		],
		actions: [
			{ id: 'keep', label: m.settings_logging_delete_keep() },
			{ id: 'delete', label: m.settings_logging_delete_confirm({ count }), variant: 'danger' }
		],
		initial: 'keep'
	});
	if (answer !== 'delete') return 0;
	const done = await api.delete('/logs/files', { body: { which: 'details' } });
	void c.catalog.load();
	return done.deleted;
}
