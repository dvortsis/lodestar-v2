// Settings → Logging's privacy changes (§10.10.7 Privacy, §12.1), loaded on the first one. Each is
// sent at once rather than staged: a deadline starts when you confirm it, and the switch shows on
// only once the server has saved it. So nothing turns on silently, a refused or failed request
// leaves the kind off, and the banner is reloaded from `GET /logs/catalog` after every change.
// Every change is built from the section as it is after the dialog is answered, and sent with its
// revision, so a change made meanwhile (another tab, `/logs`) is never overwritten.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';
import { formatBytes, formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { ask } from '../form/dialogs';
import type { SettingsForm } from '../form';
import type { LoggingCatalog } from './catalog.svelte';
import { kindLabel } from './labels';
import {
	OPT_INS,
	ahead,
	deadline,
	kindPatch,
	offPatch,
	presetPatch,
	type Catalog,
	type DataValues,
	type Duration,
	type LogKind,
	type OptIn
} from './rules';

export interface Context {
	catalog: LoggingCatalog;
	/** The screen's form, which merges the new revision; absent on `/logs`. */
	form?: SettingsForm;
	/** Aborted when the screen goes: an open question closes unanswered (nothing turns on). */
	signal?: AbortSignal;
}

type Saved = DataValues & { consoleSameAsFile: boolean; revision: number };

async function saved(): Promise<Saved> {
	const s = await api.get('/settings/{section}', { path: { section: 'logging' } });
	const f = (name: string) => s.fields[name]?.value;
	return {
		data: String(f('data') ?? 'private'),
		data_until: Number(f('data_until') ?? 0),
		kinds: { ...((f('kinds') as Record<string, number> | null) ?? {}) },
		consoleSameAsFile: f('console_detail') === 'file',
		revision: s.revision
	};
}

/** The kinds that would put torrent details in a file: identifying ones and the opt-ins. */
const detailed = (c: Catalog | undefined, now: number) =>
	new Set(
		(c?.kinds ?? [])
			.filter((k) => (k.identifying || k.opt_in) && ahead(k.until, now))
			.map((k) => k.kind)
	);

/**
 * Sends one change at the revision it was built from, then reads the server's state again. When
 * any kind that writes torrent details turned off, offers to delete the files written meanwhile.
 */
async function send(c: Context, revision: number, patch: Record<string, unknown>) {
	const before = detailed(c.catalog.catalog, c.catalog.serverNow());
	try {
		// `SettingsBatch` is untyped in the schema (form/requests.ts says why): cast here too.
		const body = {
			logging: { if_revision: revision, ...patch }
		} as unknown as components['schemas']['SettingsBatch'];
		const done = await api.patch('/settings', { body });
		const next = done.sections.logging?.revision;
		// The form holds the events' settings: it merges the section's new revision.
		if (next !== undefined) c.form?.received({ section: 'logging', revision: next });
	} finally {
		// Success or not, show what the server has now.
		await c.catalog.load();
	}
	// Saved, but if the state can't be read the screen mustn't look as if nothing changed.
	if (!c.catalog.catalog || c.catalog.error) throw new Error(m.settings_logging_unread());
	const after = detailed(c.catalog.catalog, c.catalog.serverNow());
	if ([...before].some((k) => !after.has(k))) await offerDelete(c);
}

/**
 * Asks what will be recorded and for how long (§10.10.7). Focus starts on Cancel; 1 hour is the
 * main answer. Null when cancelled.
 */
async function askDuration(
	title: string,
	body: string[],
	untilOff: boolean,
	signal?: AbortSignal
): Promise<Duration | null> {
	const answer = await ask(
		{
			title,
			body,
			actions: [
				{ id: 'cancel', label: m.settings_form_cancel() },
				...(untilOff ? [{ id: 'off', label: m.settings_logging_for_until_off() }] : []),
				{ id: 'day', label: m.settings_logging_for_day() },
				{ id: 'hour', label: m.settings_logging_for_hour(), variant: 'primary' as const }
			],
			initial: 'cancel'
		},
		signal
	);
	return answer === 'hour' || answer === 'day' || (untilOff && answer === 'off') ? answer : null;
}

async function askKinds(names: string, signal?: AbortSignal): Promise<Duration | null> {
	// Read for the wording only: the change itself is built after the answer.
	const { consoleSameAsFile } = await saved();
	return askDuration(
		m.settings_logging_dialog_title(),
		[
			m.settings_logging_dialog_what({ kinds: names }),
			consoleSameAsFile ? m.settings_logging_dialog_console() : m.settings_logging_dialog_where(),
			m.settings_logging_dialog_ends()
		],
		true,
		signal
	);
}

/** Turns a data kind on (an identifying one after the dialog) or off. False when cancelled. */
export async function setKind(c: Context, kind: LogKind, on: boolean): Promise<boolean> {
	let d: Duration | null = null;
	if (on && kind.identifying) {
		d = await askKinds(kindLabel(kind.kind), c.signal);
		if (!d) return false;
	}
	const v = await saved();
	const now = c.catalog.serverNow();
	await send(c, v.revision, kindPatch(v, kind, on, d ? deadline(d, now) : 0, now));
	return true;
}

/** Chooses a data preset; any but Private asks first. False when cancelled. */
export async function setPreset(c: Context, preset: string): Promise<boolean> {
	let d: Duration | null = null;
	if (preset !== 'private') {
		const names = (c.catalog.catalog?.kinds ?? [])
			.filter((k) => k.identifying && !k.opt_in && (preset === 'full' || k.kind === 'hash'))
			.map((k) => kindLabel(k.kind))
			.join(', ');
		d = await askKinds(names, c.signal);
		if (!d) return false;
	}
	const v = await saved();
	const until = d ? deadline(d, c.catalog.serverNow()) : 0;
	await send(c, v.revision, { ...presetPatch(preset, until, v, c.catalog.catalog) });
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
			false,
			c.signal
		);
		if (!d) return false;
		until = deadline(d, c.catalog.serverNow());
	}
	await send(c, (await saved()).revision, { [OPT_INS[kind]]: until });
	return true;
}

async function current(c: Context): Promise<Catalog> {
	const cat = c.catalog.catalog ?? (await c.catalog.load());
	if (!cat) throw new Error(m.settings_logging_unavailable());
	return cat;
}

/** "Turn off now" for the identifying kinds' banner. */
export async function turnOffIdentifying(c: Context): Promise<void> {
	const cat = await current(c);
	const v = await saved();
	await send(c, v.revision, { ...offPatch(v, cat) });
}

/** "Turn off now" on `/logs`: everything its one banner covers, identifying kinds and opt-ins. */
export async function turnOffAll(c: Context): Promise<void> {
	const cat = await current(c);
	const v = await saved();
	await send(c, v.revision, {
		...offPatch(v, cat),
		peer_activity_until: 0,
		change_details_until: 0
	});
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
	const count = formatCount(files.length, currentLocale());
	const answer = await ask(
		{
			title: m.settings_logging_delete_title(),
			body: [
				m.settings_logging_delete_body({
					count,
					size: formatBytes(
						files.reduce((n, f) => n + f.size, 0),
						currentLocale()
					)
				})
			],
			actions: [
				{ id: 'keep', label: m.settings_logging_delete_keep() },
				{ id: 'delete', label: m.settings_logging_delete_confirm({ count }), variant: 'danger' }
			],
			initial: 'keep'
		},
		c.signal
	);
	if (answer !== 'delete') return 0;
	const done = await api.delete('/logs/files', { body: { which: 'details' } });
	void c.catalog.load();
	return done.deleted;
}
