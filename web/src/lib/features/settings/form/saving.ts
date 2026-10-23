// The save pipeline (§10.10.5). Every save goes through it: busy → nothing → validate → gates →
// one PATCH /settings for the whole batch (atomic across sections) → apply, or map the refusal.
// Also Undo, the merge, drafts and validation, all on the form's `Engine` (`form.svelte.ts`).
import { ApiError, isAbort, latest } from '$lib/api/client';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { clearDraft, planRestore, readDraft } from './draft';
import { UNDO_MS, type Engine, type Sections } from './form.svelte';
import { merge, resolve as resolveField, sectionValues } from './merge';
import { getSection, patchBatch, previewBatch, type Batch } from './requests';
import {
	splitKey,
	type Danger,
	type FieldKey,
	type SaveResult,
	type SectionValues,
	type SettingsEvent,
	type SettingsPreview,
	type SettingValue
} from './types';
import { validate } from './validate';
import { applySaved, batchKeys, buildBatch, discardAll } from './batch';
import { isDirty, stage } from './values';

/** Gates run in this order, strictest first; one cancel stops the save. */
const GATE_ORDER: readonly Danger[] = ['security', 'destructive', 'privacy', 'disruptive'];

export type Sent = { ok: true; values: Record<string, SectionValues>; batch: Batch };
export type Failed = { ok: false; error: unknown; batch: Batch } | { ok: false; cancelled: true };

/** The latest polite announcement for the form's live region. */
export function announce(e: Engine, text: string) {
	e.announcement = { id: ++e.announceSeq, text };
}

const gateFor = (e: Engine, d: Danger) => e.gates.get(d) ?? e.fallbackGates.get(d);

/** The form's fields with something to save or to settle. */
function dirtyKeys(e: Engine): FieldKey[] {
	return e.specs
		.filter((s) => {
			const f = e.sections[s.section]?.fields[s.name];
			return f !== undefined && (isDirty(f) || f.conflict !== undefined);
		})
		.map((s) => s.key);
}

function devError(message: string) {
	if (import.meta.env.DEV) console.error(`settings: ${message}`);
}

/** Re-checks a field's error after a change or a blur: a client one is re-run, a server one goes. */
export function recheck(e: Engine, key: FieldKey) {
	const spec = e.byKey.get(key);
	const [section, name] = splitKey(key);
	const f = spec ? e.sec(section)?.fields[name] : undefined;
	if (!spec || !f) return;
	e.setFieldError(key, isDirty(f) ? validate(spec, f.mine) : undefined);
}

/** The note on a field whose edit the merge dropped because it's now locked. */
export function lockedNote(saved: SettingValue): string {
	return saved.env
		? m.settings_form_note_locked_env({ variable: saved.env })
		: m.settings_form_note_locked();
}

/** The section a refusal names, or the batch's only one. */
export function errorSection(err: ApiError, batch: Batch): string | undefined {
	if (err.section) return err.section;
	const names = Object.keys(batch);
	return names.length === 1 ? names[0] : undefined;
}

/** The form's field a refusal names, if any. */
export function errorKey(e: Engine, err: ApiError, batch: Batch): FieldKey | undefined {
	const section = errorSection(err, batch);
	if (!section || !err.field) return undefined;
	const key = `${section}.${err.field}` as FieldKey;
	return e.byKey.has(key) ? key : undefined;
}

// --- drafts --------------------------------------------------------------------------------

export function offerDraft(e: Engine) {
	const d = readDraft(e.id);
	if (!d) return;
	let plan: ReturnType<typeof planRestore>;
	try {
		plan = planRestore(d, e.sections, e.specOf);
	} catch {
		clearDraft(e.id); // a draft that can't be read turns drafts off, nothing else
		return;
	}
	if (plan.fields === 0) {
		clearDraft(e.id);
		return;
	}
	e.draftData = d;
	e.draft = { fields: plan.fields, dropped: plan.dropped };
}

export function restoreDraft(e: Engine) {
	if (!e.draftData) return;
	const plan = planRestore(e.draftData, e.sections, e.specOf);
	e.sections = plan.sections;
	e.draftData = null;
	e.draft = null;
	if (plan.conflicts.length) announce(e, m.settings_form_announce_conflicts());
	e.writeNow();
	e.schedulePreview();
}

export function dropDraft(e: Engine) {
	e.draftData = null;
	e.draft = null;
	clearDraft(e.id);
	e.writeNow();
}

/** Drops every staged value and conflict; an offered draft stays until it's answered. */
export function discard(e: Engine) {
	const next: Sections = { ...e.sections };
	for (const s of new Set(e.specs.map((x) => x.section))) {
		if (next[s]) next[s] = discardAll(next[s]);
	}
	e.sections = next;
	e.fieldErrors = {};
	e.previewErrors = {};
	e.formErrors = [];
	clearTimeout(e.draftTimer);
	if (e.useDrafts && !e.draft) clearDraft(e.id);
	if (e.status === 'save-error' || e.status === 'offline') e.status = 'ready';
	e.saveError = undefined;
	e.schedulePreview();
}

// --- conflicts -----------------------------------------------------------------------------

export function resolve(e: Engine, key: FieldKey, pick: 'mine' | 'theirs') {
	const [section, name] = splitKey(key);
	const s = e.byKey.has(key) ? e.sec(section) : undefined;
	if (!s) return;
	e.setSection(resolveField(s, name, pick));
	e.scheduleDraft();
	e.schedulePreview();
}

/**
 * Merges the server's values into staged edits. `stale`: a refused save (always says so);
 * `event`: a change elsewhere (says so when something visible changed); `silent`: says only
 * what needs the user (conflicts, dropped fields).
 */
export function mergeCurrent(
	e: Engine,
	current: Record<string, SectionValues>,
	why: 'stale' | 'event' | 'silent'
) {
	const next: Sections = { ...e.sections };
	let conflicted = 0;
	let removed = 0;
	let changed = 0;
	for (const [section, v] of Object.entries(current)) {
		const s = next[section];
		if (!s) continue;
		const out = merge(s, v);
		next[section] = out.state;
		conflicted += out.conflicts.filter((n) => e.inForm(section, n)).length;
		removed += out.removed.filter((n) => e.inForm(section, n)).length;
		changed += out.updated.filter((n) => e.inForm(section, n)).length;
	}
	e.sections = next;
	const said: string[] = [];
	if (conflicted) said.push(m.settings_form_announce_conflicts());
	else if (why === 'stale') said.push(m.settings_form_announce_merged());
	else if (why === 'event' && changed) said.push(m.settings_form_announce_updated());
	if (removed) {
		said.push(m.settings_form_announce_removed({ count: formatCount(removed, currentLocale()) }));
	}
	if (said.length) announce(e, said.join(' '));
	e.scheduleDraft();
	e.schedulePreview();
}

/** Secrets are never kept once a save was tried (§10.10.4). */
function clearSecrets(e: Engine) {
	let next: Sections | undefined;
	for (const spec of e.specs) {
		if (!spec.writeOnly) continue;
		const s = (next ?? e.sections)[spec.section];
		if (s?.fields[spec.name]?.mine === undefined) continue;
		next ??= { ...e.sections };
		next[spec.section] = stage(s, spec.name, undefined, spec);
	}
	if (next) e.sections = next;
}

// --- events --------------------------------------------------------------------------------

function slotFor(e: Engine, section: string) {
	let slot = e.fetchSlots.get(section);
	if (!slot) e.fetchSlots.set(section, (slot = latest()));
	return slot;
}

/**
 * Refetches a section and merges it. `force` also applies the same revision (env and read-only
 * changes don't move it) and says nothing about unchanged-looking updates.
 */
export async function refetch(e: Engine, section: string, force: boolean): Promise<void> {
	const v = await slotFor(e, section).run((signal) => getSection(section, signal));
	if (e.destroyed) return;
	const s = e.sec(section);
	if (!s) return;
	if (e.inFlight?.has(section)) {
		// Our own save's response is authoritative; look again after it.
		e.held.set(section, Math.max(e.held.get(section) ?? 0, v.revision));
		return;
	}
	if (v.revision < s.revision || (v.revision === s.revision && !force)) return;
	mergeCurrent(e, { [section]: v }, force ? 'silent' : 'event');
}

/** A newer `settings` event: held during our own save, else refetched. */
export function received(e: Engine, ev: SettingsEvent) {
	if (e.inFlight?.has(ev.section)) {
		e.held.set(ev.section, Math.max(e.held.get(ev.section) ?? 0, ev.revision));
		return;
	}
	void e.refetch(ev.section);
}

// --- sending -------------------------------------------------------------------------------

/** One PATCH /settings; a 422 with an `onConfirm` handler is asked and re-sent once. */
export async function send(e: Engine, batch: Batch): Promise<Sent | Failed> {
	let body = batch;
	for (let attempt = 0; ; attempt++) {
		try {
			const res = await patchBatch(body);
			return { ok: true, values: res.sections, batch: body };
		} catch (err) {
			const handler =
				err instanceof ApiError && err.status === 422 && attempt === 0
					? e.confirms.get(err.code)
					: undefined;
			const section = err instanceof ApiError ? errorSection(err, body) : undefined;
			if (!handler || !section || !(err instanceof ApiError))
				return { ok: false, error: err, batch: body };
			let extra: Record<string, unknown> | null;
			try {
				extra = await handler({ section, field: err.field, message: err.message });
			} catch {
				extra = null;
			}
			if (extra === null) return { ok: false, cancelled: true };
			body = { ...body, [section]: { ...body[section], ...extra } };
		}
	}
}

/** Maps a refusal (§10.10.5 and `crates/api/src/settings.rs`'s codes) onto the form. */
function refused(e: Engine, err: unknown, batch: Batch): SaveResult {
	if (isAbort(err)) return { ok: false, reason: 'cancelled' };
	if (!(err instanceof ApiError)) {
		e.status = 'save-error';
		e.saveError = new ApiError(0, 'error', String(err));
		return { ok: false, reason: 'refused' };
	}
	const key = errorKey(e, err, batch);
	const onField = (message: string) => {
		if (key) e.setFieldError(key, message);
		else e.formErrors = [...e.formErrors, message];
	};
	if (err.status === 0 || err.status === 429 || err.status === 503) {
		e.status = 'offline';
		e.saveError = err;
		e.writeNow();
		return { ok: false, reason: 'network' };
	}
	e.status = 'ready';
	e.saveError = undefined;
	if (err.status === 409 && err.code === 'stale_revision') {
		mergeCurrent(e, sectionValues(err.current), 'stale');
		return { ok: false, reason: 'conflict' };
	}
	if (err.status === 409 && (err.code === 'set_by_environment' || err.code === 'read_only')) {
		onField(err.message);
		const section = errorSection(err, batch);
		if (section) void e.refetch(section, true);
		return { ok: false, reason: 'refused' };
	}
	if (err.code === 'unknown_field' || err.code === 'unknown_section') {
		if (import.meta.env.DEV)
			devError(`the server refused ${err.code} (${err.section ?? ''}.${err.field ?? ''})`);
		e.formErrors = [...e.formErrors, m.settings_form_error_schema()];
		return { ok: false, reason: 'refused' };
	}
	if (err.status === 422) {
		onField(err.message);
		return { ok: false, reason: err.code === 'invalid_setting' ? 'invalid' : 'refused' };
	}
	if (err.status === 401) return { ok: false, reason: 'refused' }; // the sign-in redirect runs
	e.status = 'save-error';
	e.saveError = err;
	e.writeNow();
	return { ok: false, reason: 'refused' };
}

function startUndo(e: Engine, prev: Sections, sent: Batch, values: Record<string, SectionValues>) {
	clearTimeout(e.undoTimer);
	const batch: Batch = {};
	let secrets = false;
	for (const key of batchKeys(sent)) {
		const [section, name] = splitKey(key);
		if (!e.inForm(section, name)) continue; // a confirmation key (`confirm_adult`)
		if (e.specOf(section, name)?.writeOnly) {
			secrets = true;
			continue;
		}
		const before = prev[section]?.fields[name]?.saved;
		const after = values[section];
		if (!before || !after) continue;
		batch[section] ??= { if_revision: after.revision };
		batch[section][name] = before.source === 'db' ? before.value : null;
	}
	e.undoSkipsSecrets = secrets;
	if (Object.keys(batch).length === 0) {
		// Nothing to undo (a secret only): no Undo is offered, "Saved" still shows for a while.
		e.undoBatch = null;
		e.undoUntil = null;
		e.undoTimer = setTimeout(() => endUndo(e), UNDO_MS);
		return;
	}
	e.undoBatch = batch;
	e.undoUntil = Date.now() + UNDO_MS;
	e.undoTimer = setTimeout(() => endUndo(e), UNDO_MS);
}

function endUndo(e: Engine) {
	clearTimeout(e.undoTimer);
	e.undoBatch = null;
	e.undoUntil = null;
	e.undoSkipsSecrets = false;
	e.savedRecently = {};
	if (e.status === 'saved') e.status = 'ready';
}

/** Applies a successful response; returns the saved fields waiting for a restart. */
export function applyResponse(
	e: Engine,
	sent: Batch,
	values: Record<string, SectionValues>
): FieldKey[] {
	const next: Sections = { ...e.sections };
	const restart: FieldKey[] = [];
	const recent: Record<string, 'live' | 'restart'> = {};
	for (const [section, v] of Object.entries(values)) {
		if (!next[section]) continue;
		const patch = sent[section] ?? {};
		next[section] = applySaved(next[section], v, patch);
		for (const name of Object.keys(patch)) {
			if (!e.inForm(section, name)) continue;
			const key = `${section}.${name}` as FieldKey;
			const waits = v.restart_required.includes(name);
			recent[key] = waits ? 'restart' : 'live';
			if (waits) restart.push(key);
			e.setFieldError(key, undefined);
		}
	}
	e.sections = next;
	e.savedRecently = recent;
	e.restartPending = e.specs
		.filter((s) => e.sections[s.section]?.restart.includes(s.name))
		.map((s) => s.key);
	return restart;
}

function afterSend(e: Engine, sent: Set<string>) {
	e.inFlight = null;
	e.sending = new Set();
	// Events for the saved sections at or below the response's revision were our own.
	const later = [...e.held].filter(
		([s, rev]) => sent.has(s) && rev > (e.sections[s]?.revision ?? 0)
	);
	e.held = new Map();
	for (const [s] of later) void e.refetch(s);
}

/** Runs `f` with the sections' events held until it's done; `keys` are in flight. */
export async function holding<T>(
	e: Engine,
	sent: Set<string>,
	keys: Iterable<string> | undefined,
	f: () => Promise<T>
): Promise<T> {
	e.inFlight = sent;
	if (keys) e.sending = new Set(keys);
	try {
		return await f();
	} finally {
		afterSend(e, sent);
	}
}

// --- saving --------------------------------------------------------------------------------

export async function save(e: Engine): Promise<SaveResult> {
	const result = await saveNow(e);
	e.lastSave = { id: ++e.saveSeq, result };
	return result;
}

async function saveNow(e: Engine): Promise<SaveResult> {
	// An instant change waiting for instant mode to load is as good as in flight.
	if (e.inFlight || e.instantPending) return { ok: false, reason: 'busy' };
	if (e.mode === 'instant') {
		// The explicit commit: the staged secrets, each as its own change.
		const secrets = e.specs.some((spec) => {
			const f = e.sec(spec.section)?.fields[spec.name];
			return spec.writeOnly && f !== undefined && isDirty(f);
		});
		if (!secrets) return { ok: false, reason: 'nothing' };
		try {
			return await (e.instant ?? (await e.instantMode())).saveSecrets();
		} catch {
			// Instant mode didn't load: the secrets aren't kept once tried (§10.10.4), and say so.
			for (const spec of e.specs) {
				const s = e.sec(spec.section);
				if (!spec.writeOnly || s?.fields[spec.name]?.mine === undefined) continue;
				e.setSection(stage(s, spec.name, undefined, spec));
				e.setFieldError(spec.key, m.settings_form_chunk_failed());
			}
			return { ok: false, reason: 'network' };
		}
	}
	e.formErrors = [];
	if (e.conflicts.length) {
		announce(e, m.settings_form_announce_conflicts());
		return { ok: false, reason: 'conflict' };
	}
	const batch = buildBatch(e.sections, e.inForm);
	if (Object.keys(batch).length === 0) return { ok: false, reason: 'nothing' };
	// The draft is stored before anything is sent: a 401's redirect or a crash keeps it.
	e.writeNow();
	const keys = batchKeys(batch).filter((k) => e.byKey.has(k)) as FieldKey[];
	// Validate every dirty field.
	const errors: Record<string, string> = { ...e.fieldErrors };
	let invalid = false;
	for (const key of keys) {
		const [section, name] = splitKey(key);
		const message = validate(e.byKey.get(key)!, e.sections[section]?.fields[name]?.mine);
		if (message === undefined) delete errors[key];
		else {
			errors[key] = message;
			invalid = true;
		}
	}
	e.fieldErrors = errors;
	if (invalid) {
		clearSecrets(e);
		e.status = 'ready';
		return { ok: false, reason: 'invalid' };
	}
	e.failedOp = 'save';
	try {
		return await holding(e, new Set(Object.keys(batch)), keys, async () => {
			const gated = await runGates(e, batch, keys);
			if (gated) return gated;
			// Only now: while a confirmation is open, nothing is being saved yet.
			e.status = 'saving';
			const prev = e.sections;
			const r = await send(e, batch);
			if (e.destroyed) return { ok: false, reason: 'cancelled' };
			if (!r.ok) {
				e.status = 'ready';
				return 'cancelled' in r ? { ok: false, reason: 'cancelled' } : refused(e, r.error, r.batch);
			}
			const restart = applyResponse(e, r.batch, r.values);
			startUndo(e, prev, r.batch, r.values);
			e.status = 'saved';
			e.saveError = undefined;
			if (e.useDrafts) {
				if (dirtyKeys(e).length === 0) {
					clearTimeout(e.draftTimer);
					if (!e.draft) clearDraft(e.id);
				} else e.writeNow();
			}
			announce(
				e,
				restart.length ? m.settings_form_announce_saved_restart() : m.settings_form_announce_saved()
			);
			e.schedulePreview();
			return { ok: true, restart };
		});
	} finally {
		clearSecrets(e);
	}
}

/** §10.10.7: each danger class among the dirty fields asks once; no handler fails closed. */
export async function runGates(
	e: Engine,
	batch: Batch,
	keys: FieldKey[]
): Promise<SaveResult | undefined> {
	const kinds = GATE_ORDER.filter((d) => keys.some((k) => e.byKey.get(k)?.danger === d));
	if (kinds.length === 0) return undefined;
	const missing = kinds.filter((d) => !gateFor(e, d));
	if (missing.length) {
		const fields = keys.filter((k) => missing.includes(e.byKey.get(k)!.danger!));
		if (import.meta.env.DEV)
			devError(`no confirmation is registered for ${fields.join(', ')} (${missing.join(', ')})`);
		e.formErrors = [...e.formErrors, m.settings_form_error_needs_confirmation()];
		e.status = 'ready';
		return { ok: false, reason: 'refused' };
	}
	let p: SettingsPreview;
	try {
		p = await previewBatch(batch);
	} catch (err) {
		e.status = 'ready';
		return refused(e, err, batch);
	}
	for (const d of kinds) {
		const fields = keys.filter((k) => e.byKey.get(k)!.danger === d);
		let ok = false;
		try {
			ok = await gateFor(e, d)!({ fields, preview: p });
		} catch {
			ok = false;
		}
		if (!ok) {
			e.status = 'ready';
			return { ok: false, reason: 'cancelled' };
		}
	}
	return undefined;
}

/** The last staged save's Undo. */
export async function undo(e: Engine): Promise<SaveResult> {
	if (e.inFlight) return { ok: false, reason: 'busy' };
	const batch = e.undoBatch;
	if (!batch || e.undoUntil === null || Date.now() > e.undoUntil)
		return { ok: false, reason: 'nothing' };
	const sent = new Set(Object.keys(batch));
	e.failedOp = 'undo';
	return holding(e, sent, batchKeys(batch), async () => {
		e.status = 'saving';
		const r = await send(e, batch);
		if (!r.ok) {
			e.status = 'ready';
			if ('cancelled' in r) return { ok: false, reason: 'cancelled' };
			const err = r.error;
			if (err instanceof ApiError && err.code === 'stale_revision') {
				// Never merge an Undo: say so and show what's there now.
				endUndo(e);
				announce(e, m.settings_form_announce_undo_stale());
				for (const s of sent) void e.refetch(s, true);
				return { ok: false, reason: 'conflict' };
			}
			return refused(e, err, r.batch);
		}
		applyResponse(e, r.batch, r.values);
		endUndo(e);
		e.restartPending = e.specs
			.filter((s) => e.sections[s.section]?.restart.includes(s.name))
			.map((s) => s.key);
		e.status = 'ready';
		announce(e, m.settings_form_announce_undone());
		e.schedulePreview();
		return { ok: true, restart: [] };
	});
}

/** Removes a saved secret on its own (one PATCH to `null`), after the UI's confirmation. */
export async function removeSecret(e: Engine, key: FieldKey): Promise<SaveResult> {
	const spec = e.byKey.get(key);
	const [section, name] = splitKey(key);
	const s = e.sections[section];
	if (!spec?.writeOnly || !s) return { ok: false, reason: 'nothing' };
	if (e.inFlight) return { ok: false, reason: 'busy' };
	const batch: Batch = { [section]: { if_revision: s.revision, [name]: null } };
	return holding(e, new Set([section]), undefined, async () => {
		const r = await send(e, batch);
		if (!r.ok)
			return 'cancelled' in r ? { ok: false, reason: 'cancelled' } : refused(e, r.error, r.batch);
		applyResponse(e, r.batch, r.values);
		e.savedRecently = {};
		announce(e, m.settings_form_announce_secret_removed());
		return { ok: true, restart: [] };
	});
}

/** Re-runs what failed while offline or with a server error. */
export async function retry(e: Engine) {
	if (e.status !== 'offline' && e.status !== 'save-error') return;
	if (e.failedOp === 'save') {
		await save(e);
		return;
	}
	const r = await undo(e);
	if (!r.ok && r.reason === 'nothing') {
		// Undo's window closed while offline: there's nothing left to retry.
		e.status = 'ready';
		e.saveError = undefined;
	}
}
