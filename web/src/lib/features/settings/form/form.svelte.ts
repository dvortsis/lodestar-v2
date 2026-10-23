// One form per screen or wizard step (§10.10.1, §10.10.5). A plain store: everything tied to a
// component's lifetime (the `settings` subscription, ⌘/Ctrl+S, the leave guard) is wired by
// `SettingsForm.svelte`, which calls `received`/`resync` and registers the default gate.
//
// Every save goes through one pipeline: busy → nothing → validate → gates → one PATCH /settings
// for the whole batch (atomic across sections) → apply, or map the refusal.
//
// The Maps and Sets here are bookkeeping that nothing renders; reactive state is `$state.raw`
// records replaced on every change.
/* eslint-disable svelte/prefer-svelte-reactivity */
import { ApiError, isAbort, latest } from '$lib/api/client';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { m } from '$lib/paraglide/messages.js';
import { clearDraft, draftOf, planRestore, readDraft, writeDraft, type Draft } from './draft';
import { merge, resolve as resolveField } from './merge';
import { getSection, patchBatch, previewBatch, type Batch } from './requests';
import { devWarn, loadSettingsIndex } from './schema';
import {
	splitKey,
	type ConfirmHandler,
	type Danger,
	type FieldKey,
	type FieldSpec,
	type FieldState,
	type FormStatus,
	type GateHandler,
	type SaveResult,
	type SectionValues,
	type SettingsEvent,
	type SettingsPreview,
	type SettingValue
} from './types';
import { validate } from './validate';
import {
	applySaved,
	buildBatch,
	batchKeys,
	discardAll,
	fromValues,
	isDirty,
	isLocked,
	RESET,
	stage,
	wire,
	type SectionState
} from './values';

/** How long Undo is offered after a save (§10.10.5). */
export const UNDO_MS = 10_000;
/** Debounce for drafts, previews and instant-mode text and numbers. */
export const DEBOUNCE_MS = 300;
/** Gates run in this order, strictest first; one cancel stops the save. */
const GATE_ORDER: readonly Danger[] = ['security', 'destructive', 'privacy', 'disruptive'];
/** Kinds whose instant-mode change is sent at once rather than debounced. */
const INSTANT_NOW = new Set(['boolean', 'enum']);

export interface SettingsFormOptions {
	/** Screen id: the draft key and the key scope (`crawler`, `wizard.intensity`). */
	id: string;
	/** Any sections; one save covers all of them. */
	fields: FieldKey[] | FieldSpec[];
	/** `instant` for Preferences screens: each change saves on its own. */
	mode?: 'staged' | 'instant';
	/** Drafts in `sessionStorage` (staged mode only). */
	drafts?: boolean;
	/** "Copy link" target. */
	href?: (key: FieldKey) => string;
}

export interface SettingsForm {
	readonly id: string;
	readonly mode: 'staged' | 'instant';
	readonly specs: FieldSpec[];
	readonly status: FormStatus;
	readonly loadError?: ApiError;
	/** The failed save behind an `offline` or `save-error` status (`retryAfter` for 429/503). */
	readonly saveError?: ApiError;
	readonly dirtyCount: number;
	/** The error summary, in field order (errors without a field first). */
	readonly errors: { key?: FieldKey; message: string }[];
	readonly conflicts: FieldKey[];
	readonly restartPending: FieldKey[];
	readonly undoUntil: number | null;
	/** The last save included a secret, which Undo leaves out. */
	readonly undoSkipsSecrets: boolean;
	readonly draft: null | { fields: number; dropped: number };
	/** The latest polite announcement; `id` changes with each one. */
	readonly announcement: { id: number; text: string } | null;
	href(key: FieldKey): string;
	field(key: FieldKey): FieldState;
	set(key: FieldKey, value: unknown): void;
	reset(key: FieldKey): void;
	blur(key: FieldKey): void;
	save(): Promise<SaveResult>;
	discard(): void;
	/** Staged: the last save's Undo. Instant: that field's Undo. */
	undo(key?: FieldKey): Promise<SaveResult>;
	resolve(key: FieldKey, pick: 'mine' | 'theirs'): void;
	restoreDraft(): void;
	dropDraft(): void;
	retry(): Promise<void>;
	/** Removes a saved secret on its own (one PATCH to `null`), after the UI's confirmation. */
	removeSecret(key: FieldKey): Promise<SaveResult>;
	onConfirm(code: string, handler: ConfirmHandler): () => void;
	gate(danger: Danger, handler: GateHandler): () => void;
	/** A `settings` event (`SettingsForm.svelte` subscribes). */
	received(e: SettingsEvent): void;
	/** The events stream resynced: refetch every section. */
	resync(): Promise<void>;
	destroy(): void;
}

type Sections = Record<string, SectionState>;

function devError(message: string) {
	if (import.meta.env.DEV) console.error(`settings: ${message}`);
}

export function createSettingsForm(opts: SettingsFormOptions): SettingsForm {
	const mode = opts.mode ?? 'staged';
	const useDrafts = mode === 'staged' && (opts.drafts ?? true);
	const href = opts.href ?? ((key: FieldKey) => `${globalThis.location?.pathname ?? ''}#${key}`);

	let specs = $state.raw<FieldSpec[]>([]);
	let byKey = new Map<string, FieldSpec>();
	let sections = $state.raw<Sections>({});
	let status = $state<FormStatus>('loading');
	let loadError = $state.raw<ApiError | undefined>();
	let saveError = $state.raw<ApiError | undefined>();
	let fieldErrors = $state.raw<Record<string, string>>({});
	let previewErrors = $state.raw<Record<string, string>>({});
	let formErrors = $state.raw<string[]>([]);
	let preview = $state.raw<SettingsPreview | null>(null);
	let restartPending = $state.raw<FieldKey[]>([]);
	let savedRecently = $state.raw<Record<string, 'live' | 'restart'>>({});
	let undoUntil = $state<number | null>(null);
	let undoSkipsSecrets = $state(false);
	let instantUndo = $state.raw<Record<string, { until: number; prev: SettingValue }>>({});
	let draftOffer = $state.raw<null | { fields: number; dropped: number }>(null);
	let announcement = $state.raw<{ id: number; text: string } | null>(null);

	// Not rendered: plain variables.
	let undoBatch: Batch | null = null;
	let undoTimer: ReturnType<typeof setTimeout> | undefined;
	let draftTimer: ReturnType<typeof setTimeout> | undefined;
	let previewTimer: ReturnType<typeof setTimeout> | undefined;
	let draftData: Draft | null = null;
	let inFlight: Set<string> | null = null; // sections of the save in flight
	let failedOp: 'save' | 'undo' = 'save'; // what Retry re-runs
	let held = new Map<string, number>(); // events held during it
	let destroyed = false;
	let announceSeq = 0;
	let instantChain: Promise<unknown> = Promise.resolve();
	const instantTimers = new Map<string, ReturnType<typeof setTimeout>>();
	const instantUndoTimers = new Map<string, ReturnType<typeof setTimeout>>();
	const confirms = new Map<string, ConfirmHandler>();
	const gates = new Map<Danger, GateHandler>();
	const fetchSlots = new Map<string, ReturnType<typeof latest>>();
	const previewSlot = latest();

	const specOf = (section: string, name: string) => byKey.get(`${section}.${name}`);
	/** A section by a name from outside (an event, a caller's key): own properties only. */
	const sec = (section: string): SectionState | undefined =>
		Object.hasOwn(sections, section) ? sections[section] : undefined;
	const inForm = (section: string, name: string) => byKey.has(`${section}.${name}`);

	function announce(text: string) {
		announcement = { id: ++announceSeq, text };
	}

	function setSection(s: SectionState) {
		sections = { ...sections, [s.section]: s };
	}

	function setFieldError(key: string, message: string | undefined) {
		if (fieldErrors[key] === message) return;
		const next = { ...fieldErrors };
		if (message === undefined) delete next[key];
		else next[key] = message;
		fieldErrors = next;
	}

	function formBatch(): Batch {
		return buildBatch(sections, inForm);
	}

	function dirtyKeys(): FieldKey[] {
		return specs
			.filter((s) => {
				const f = sections[s.section]?.fields[s.name];
				return f !== undefined && (isDirty(f) || f.conflict !== undefined);
			})
			.map((s) => s.key);
	}

	const dirtyCount = $derived.by(() => {
		let n = 0;
		for (const s of specs) {
			const f = sections[s.section]?.fields[s.name];
			if (f && (isDirty(f) || f.conflict)) n++;
		}
		return n;
	});

	const conflicts = $derived(
		specs.filter((s) => sections[s.section]?.fields[s.name]?.conflict).map((s) => s.key)
	);

	// --- loading ---------------------------------------------------------------------------

	async function load() {
		status = 'loading';
		loadError = undefined;
		try {
			let list: FieldSpec[];
			if (opts.fields.length && typeof opts.fields[0] === 'string') {
				const index = await loadSettingsIndex();
				list = [];
				for (const key of opts.fields as FieldKey[]) {
					const spec = index.spec(key);
					if (spec) list.push(spec);
					else devWarn(`${opts.id}: no setting ${key}`);
				}
			} else list = opts.fields as FieldSpec[];
			byKey = new Map(list.map((s) => [s.key, s]));
			specs = list;
			const names = [...new Set(list.map((s) => s.section))];
			const values = await Promise.all(names.map((s) => getSection(s)));
			if (destroyed) return;
			const next: Sections = {};
			for (const v of values) next[v.section] = fromValues(v);
			sections = next;
			status = 'ready';
			if (useDrafts) offerDraft();
			schedulePreview();
		} catch (e) {
			if (destroyed || isAbort(e)) return;
			loadError = e instanceof ApiError ? e : new ApiError(0, 'network', String(e));
			status = 'load-error';
		}
	}

	function offerDraft() {
		const d = readDraft(opts.id);
		if (!d) return;
		let plan: ReturnType<typeof planRestore>;
		try {
			plan = planRestore(d, sections, specOf);
		} catch {
			clearDraft(opts.id); // a draft that can't be read turns drafts off, nothing else
			return;
		}
		if (plan.fields === 0) {
			clearDraft(opts.id);
			return;
		}
		draftData = d;
		draftOffer = { fields: plan.fields, dropped: plan.dropped };
	}

	// --- field view --------------------------------------------------------------------------

	function field(key: FieldKey): FieldState {
		const spec = byKey.get(key);
		const [section, name] = splitKey(key);
		const f = spec ? sec(section)?.fields[name] : undefined;
		if (!spec || !f) {
			return {
				spec: spec ?? placeholderSpec(key),
				value: undefined,
				saved: undefined,
				source: 'default',
				env: null,
				locked: true,
				dirty: false,
				changed: false,
				pendingReset: false,
				secretSet: false
			};
		}
		const p = preview?.sections[section]?.fields[name];
		const state: FieldState = {
			spec,
			value: f.mine === undefined || f.mine === RESET ? f.saved.value : f.mine,
			saved: f.saved.value,
			source: f.saved.source,
			env: f.saved.env,
			locked: isLocked(f.saved) || spec.readOnly,
			dirty: isDirty(f) || f.conflict !== undefined,
			changed: f.saved.source !== 'default',
			pendingReset: f.mine === RESET,
			secretSet: spec.writeOnly && f.saved.source !== 'default'
		};
		if (spec.writeOnly && f.mine === undefined) state.value = null;
		const error = fieldErrors[key] ?? previewErrors[key];
		if (error !== undefined) state.error = error;
		if (f.note === 'locked') {
			state.note = f.saved.env
				? m.settings_form_note_locked_env({ variable: f.saved.env })
				: m.settings_form_note_locked();
		}
		if (f.conflict) state.conflict = { ...f.conflict };
		if (p) {
			state.preview = {
				restartRequired: p.restart_required,
				estimates: p.estimates,
				warnings: p.warnings
			};
		}
		if (savedRecently[key]) state.savedRecently = savedRecently[key];
		if (instantUndo[key]) state.undoUntil = instantUndo[key].until;
		return state;
	}

	// --- editing -----------------------------------------------------------------------------

	function set(key: FieldKey, value: unknown) {
		const spec = byKey.get(key);
		const [section, name] = splitKey(key);
		const s = spec ? sec(section) : undefined;
		if (!spec || !s) return;
		const next = stage(s, name, value, spec);
		if (next === s) return;
		setSection(next);
		const f = next.fields[name]!;
		// A server error no longer applies once the value changes; a client one is re-checked.
		if (fieldErrors[key] !== undefined) {
			setFieldError(key, isDirty(f) ? validate(spec, f.mine) : undefined);
		}
		if (previewErrors[key] !== undefined) {
			previewErrors = without(previewErrors, key);
		}
		if (mode === 'instant') return scheduleInstant(spec);
		scheduleDraft();
		schedulePreview();
	}

	function reset(key: FieldKey) {
		set(key, RESET);
	}

	function blur(key: FieldKey) {
		const spec = byKey.get(key);
		const [section, name] = splitKey(key);
		const f = spec ? sec(section)?.fields[name] : undefined;
		if (!spec || !f) return;
		setFieldError(key, isDirty(f) ? validate(spec, f.mine) : undefined);
	}

	function discard() {
		const next: Sections = { ...sections };
		for (const s of new Set(specs.map((x) => x.section))) {
			if (next[s]) next[s] = discardAll(next[s]);
		}
		sections = next;
		fieldErrors = {};
		previewErrors = {};
		formErrors = [];
		clearTimeout(draftTimer);
		// An offered draft stays until the user restores or discards it.
		if (useDrafts && !draftOffer) clearDraft(opts.id);
		if (status === 'save-error' || status === 'offline') status = 'ready';
		saveError = undefined;
		schedulePreview();
	}

	function resolve(key: FieldKey, pick: 'mine' | 'theirs') {
		const [section, name] = splitKey(key);
		const s = byKey.has(key) ? sec(section) : undefined;
		if (!s) return;
		setSection(resolveField(s, name, pick));
		scheduleDraft();
		schedulePreview();
	}

	/** Secrets are never kept once a save was tried (§10.10.4). */
	function clearSecrets() {
		let next: Sections | undefined;
		for (const spec of specs) {
			if (!spec.writeOnly) continue;
			const s = (next ?? sections)[spec.section];
			if (s?.fields[spec.name]?.mine === undefined) continue;
			next ??= { ...sections };
			next[spec.section] = stage(s, spec.name, undefined, spec);
		}
		if (next) sections = next;
	}

	// --- drafts ------------------------------------------------------------------------------

	function writeNow() {
		clearTimeout(draftTimer);
		draftTimer = undefined;
		// An offered draft isn't overwritten before the user restores or discards it.
		if (!useDrafts || draftOffer || status === 'loading') return;
		writeDraft(opts.id, draftOf(sections, specOf, Date.now()));
	}

	function scheduleDraft() {
		if (!useDrafts) return;
		clearTimeout(draftTimer);
		draftTimer = setTimeout(writeNow, DEBOUNCE_MS);
	}

	function restoreDraft() {
		if (!draftData) return;
		const plan = planRestore(draftData, sections, specOf);
		sections = plan.sections;
		draftData = null;
		draftOffer = null;
		if (plan.conflicts.length) announce(m.settings_form_announce_conflicts());
		writeNow();
		schedulePreview();
	}

	function dropDraft() {
		draftData = null;
		draftOffer = null;
		clearDraft(opts.id);
		writeNow();
	}

	// --- preview -----------------------------------------------------------------------------

	/** The dirty batch, plus fields at their automatic value so their effective value shows. */
	function previewBody(): Batch {
		const batch = buildBatch(sections, (s, n) => inForm(s, n) && !specOf(s, n)!.writeOnly);
		for (const spec of specs) {
			if (!spec.preview || spec.auto === undefined || spec.writeOnly) continue;
			const s = sections[spec.section];
			const f = s?.fields[spec.name];
			if (!s || !f || f.mine !== undefined || isLocked(f.saved) || spec.readOnly) continue;
			if (f.saved.value !== spec.auto) continue;
			(batch[spec.section] ??= { if_revision: s.revision })[spec.name] = f.saved.value;
		}
		return batch;
	}

	function schedulePreview() {
		if (destroyed || !specs.some((s) => s.preview)) return;
		clearTimeout(previewTimer);
		previewTimer = setTimeout(runPreview, DEBOUNCE_MS);
	}

	async function runPreview() {
		const body = previewBody();
		if (Object.keys(body).length === 0) {
			previewSlot.cancel();
			preview = null;
			previewErrors = {};
			return;
		}
		try {
			preview = await previewSlot.run((signal) => previewBatch(body, signal));
			previewErrors = {};
		} catch (e) {
			if (isAbort(e)) return;
			// A refusal shows on its field but never blocks typing or Save.
			const key = e instanceof ApiError ? errorKey(e, body) : undefined;
			previewErrors = key && e instanceof ApiError ? { [key]: e.message } : {};
		}
	}

	// --- sending -----------------------------------------------------------------------------

	type Sent = { ok: true; values: Record<string, SectionValues>; batch: Batch };
	type Failed = { ok: false; error: unknown; batch: Batch } | { ok: false; cancelled: true };

	/** One PATCH /settings; a 422 with an `onConfirm` handler is asked and re-sent once. */
	async function send(batch: Batch): Promise<Sent | Failed> {
		let body = batch;
		for (let attempt = 0; ; attempt++) {
			try {
				const res = await patchBatch(body);
				return { ok: true, values: res.sections, batch: body };
			} catch (e) {
				const handler =
					e instanceof ApiError && e.status === 422 && attempt === 0
						? confirms.get(e.code)
						: undefined;
				const section = e instanceof ApiError ? errorSection(e, body) : undefined;
				if (!handler || !section || !(e instanceof ApiError))
					return { ok: false, error: e, batch: body };
				let extra: Record<string, unknown> | null;
				try {
					extra = await handler({ section, field: e.field, message: e.message });
				} catch {
					extra = null;
				}
				if (extra === null) return { ok: false, cancelled: true };
				body = { ...body, [section]: { ...body[section], ...extra } };
			}
		}
	}

	function errorSection(e: ApiError, batch: Batch): string | undefined {
		if (e.section) return e.section;
		const names = Object.keys(batch);
		return names.length === 1 ? names[0] : undefined;
	}

	function errorKey(e: ApiError, batch: Batch): FieldKey | undefined {
		const section = errorSection(e, batch);
		if (!section || !e.field) return undefined;
		const key = `${section}.${e.field}` as FieldKey;
		return byKey.has(key) ? key : undefined;
	}

	/** Maps a refusal (§10.10.5 and `crates/api/src/settings.rs`'s codes) onto the form. */
	function refused(e: unknown, batch: Batch): SaveResult {
		if (isAbort(e)) return { ok: false, reason: 'cancelled' };
		if (!(e instanceof ApiError)) {
			status = 'save-error';
			saveError = new ApiError(0, 'error', String(e));
			return { ok: false, reason: 'refused' };
		}
		const key = errorKey(e, batch);
		const onField = (message: string) => {
			if (key) setFieldError(key, message);
			else formErrors = [...formErrors, message];
		};
		if (e.status === 0 || e.status === 429 || e.status === 503) {
			status = 'offline';
			saveError = e;
			writeNow();
			return { ok: false, reason: 'network' };
		}
		status = 'ready';
		saveError = undefined;
		if (e.status === 409 && e.code === 'stale_revision') {
			mergeCurrent(e.current ?? {}, 'stale');
			return { ok: false, reason: 'conflict' };
		}
		if (e.status === 409 && (e.code === 'set_by_environment' || e.code === 'read_only')) {
			onField(e.message);
			const section = errorSection(e, batch);
			if (section) void refetch(section, true);
			return { ok: false, reason: 'refused' };
		}
		if (e.code === 'unknown_field' || e.code === 'unknown_section') {
			devError(`the server refused ${e.code} (${e.section ?? ''}.${e.field ?? ''})`);
			formErrors = [...formErrors, m.settings_form_error_schema()];
			return { ok: false, reason: 'refused' };
		}
		if (e.status === 422) {
			onField(e.message);
			return { ok: false, reason: e.code === 'invalid_setting' ? 'invalid' : 'refused' };
		}
		if (e.status === 401) return { ok: false, reason: 'refused' }; // the sign-in redirect runs
		status = 'save-error';
		saveError = e;
		writeNow();
		return { ok: false, reason: 'refused' };
	}

	/**
	 * Merges the server's values into staged edits. `stale`: a refused save (always says so);
	 * `event`: a change elsewhere (says so when something visible changed); `silent`: says only
	 * what needs the user (conflicts, dropped fields).
	 */
	function mergeCurrent(current: Record<string, SectionValues>, why: 'stale' | 'event' | 'silent') {
		const next: Sections = { ...sections };
		let conflicted = 0;
		let removed = 0;
		let changed = 0;
		for (const [section, v] of Object.entries(current)) {
			const s = next[section];
			if (!s) continue;
			const out = merge(s, v);
			next[section] = out.state;
			conflicted += out.conflicts.filter((n) => inForm(section, n)).length;
			removed += out.removed.filter((n) => inForm(section, n)).length;
			changed += out.updated.filter((n) => inForm(section, n)).length;
		}
		sections = next;
		const said: string[] = [];
		if (conflicted) said.push(m.settings_form_announce_conflicts());
		else if (why === 'stale') said.push(m.settings_form_announce_merged());
		else if (why === 'event' && changed) said.push(m.settings_form_announce_updated());
		if (removed) {
			said.push(m.settings_form_announce_removed({ count: formatCount(removed, currentLocale()) }));
		}
		if (said.length) announce(said.join(' '));
		scheduleDraft();
		schedulePreview();
	}

	function startUndo(prev: Sections, sent: Batch, values: Record<string, SectionValues>) {
		clearTimeout(undoTimer);
		const batch: Batch = {};
		let secrets = false;
		for (const key of batchKeys(sent)) {
			const [section, name] = splitKey(key);
			if (!inForm(section, name)) continue; // a confirmation key (`confirm_adult`)
			if (specOf(section, name)?.writeOnly) {
				secrets = true;
				continue;
			}
			const before = prev[section]?.fields[name]?.saved;
			const after = values[section];
			if (!before || !after) continue;
			batch[section] ??= { if_revision: after.revision };
			batch[section][name] = before.source === 'db' ? before.value : null;
		}
		undoSkipsSecrets = secrets;
		if (Object.keys(batch).length === 0) {
			// Nothing to undo (a secret only): no Undo is offered, "Saved" still shows for a while.
			undoBatch = null;
			undoUntil = null;
			undoTimer = setTimeout(endUndo, UNDO_MS);
			return;
		}
		undoBatch = batch;
		undoUntil = Date.now() + UNDO_MS;
		undoTimer = setTimeout(endUndo, UNDO_MS);
	}

	function endUndo() {
		clearTimeout(undoTimer);
		undoBatch = null;
		undoUntil = null;
		undoSkipsSecrets = false;
		savedRecently = {};
		if (status === 'saved') status = 'ready';
	}

	/** Applies a successful response; returns the saved fields waiting for a restart. */
	function applyResponse(sent: Batch, values: Record<string, SectionValues>): FieldKey[] {
		const next: Sections = { ...sections };
		const restart: FieldKey[] = [];
		const recent: Record<string, 'live' | 'restart'> = {};
		for (const [section, v] of Object.entries(values)) {
			if (!next[section]) continue;
			const patch = sent[section] ?? {};
			next[section] = applySaved(next[section], v, patch);
			for (const name of Object.keys(patch)) {
				if (!inForm(section, name)) continue;
				const key = `${section}.${name}` as FieldKey;
				const waits = v.restart_required.includes(name);
				recent[key] = waits ? 'restart' : 'live';
				if (waits) restart.push(key);
				setFieldError(key, undefined);
			}
		}
		sections = next;
		savedRecently = recent;
		restartPending = specs
			.filter((s) => sections[s.section]?.restart.includes(s.name))
			.map((s) => s.key);
		return restart;
	}

	function afterSend(sent: Set<string>) {
		inFlight = null;
		// Events for the saved sections at or below the response's revision were our own.
		const later = [...held].filter(([s, rev]) => sent.has(s) && rev > (sections[s]?.revision ?? 0));
		held = new Map();
		for (const [s] of later) void refetch(s);
	}

	async function save(): Promise<SaveResult> {
		if (inFlight) return { ok: false, reason: 'busy' };
		if (mode === 'instant') return { ok: false, reason: 'nothing' };
		formErrors = [];
		if (conflicts.length) {
			announce(m.settings_form_announce_conflicts());
			return { ok: false, reason: 'conflict' };
		}
		const batch = formBatch();
		if (Object.keys(batch).length === 0) return { ok: false, reason: 'nothing' };
		// The draft is stored before anything is sent: a 401's redirect or a crash keeps it.
		writeNow();
		const keys = batchKeys(batch).filter((k) => byKey.has(k)) as FieldKey[];
		// Validate every dirty field.
		const errors: Record<string, string> = { ...fieldErrors };
		let invalid = false;
		for (const key of keys) {
			const [section, name] = splitKey(key);
			const message = validate(byKey.get(key)!, sections[section]?.fields[name]?.mine);
			if (message === undefined) delete errors[key];
			else {
				errors[key] = message;
				invalid = true;
			}
		}
		fieldErrors = errors;
		if (invalid) {
			clearSecrets();
			status = 'ready';
			return { ok: false, reason: 'invalid' };
		}
		const sent = new Set(Object.keys(batch));
		inFlight = sent;
		failedOp = 'save';
		status = 'saving';
		try {
			const gated = await runGates(batch, keys);
			if (gated) return gated;
			const prev = sections;
			const r = await send(batch);
			if (destroyed) return { ok: false, reason: 'cancelled' };
			if (!r.ok) {
				status = 'ready';
				return 'cancelled' in r ? { ok: false, reason: 'cancelled' } : refused(r.error, r.batch);
			}
			const restart = applyResponse(r.batch, r.values);
			startUndo(prev, r.batch, r.values);
			status = 'saved';
			saveError = undefined;
			if (useDrafts) {
				if (dirtyKeys().length === 0) {
					clearTimeout(draftTimer);
					if (!draftOffer) clearDraft(opts.id);
				} else writeNow();
			}
			announce(
				restart.length ? m.settings_form_announce_saved_restart() : m.settings_form_announce_saved()
			);
			schedulePreview();
			return { ok: true, restart };
		} finally {
			clearSecrets();
			afterSend(sent);
		}
	}

	/** §10.10.7: each danger class among the dirty fields asks once; no handler fails closed. */
	async function runGates(batch: Batch, keys: FieldKey[]): Promise<SaveResult | undefined> {
		const kinds = GATE_ORDER.filter((d) => keys.some((k) => byKey.get(k)?.danger === d));
		if (kinds.length === 0) return undefined;
		const missing = kinds.filter((d) => !gates.has(d));
		if (missing.length) {
			const fields = keys.filter((k) => missing.includes(byKey.get(k)!.danger!));
			devError(`no confirmation is registered for ${fields.join(', ')} (${missing.join(', ')})`);
			formErrors = [...formErrors, m.settings_form_error_needs_confirmation()];
			status = 'ready';
			return { ok: false, reason: 'refused' };
		}
		let p: SettingsPreview;
		try {
			p = await previewBatch(batch);
		} catch (e) {
			status = 'ready';
			return refused(e, batch);
		}
		for (const d of kinds) {
			const fields = keys.filter((k) => byKey.get(k)!.danger === d);
			let ok = false;
			try {
				ok = await gates.get(d)!({ fields, preview: p });
			} catch {
				ok = false;
			}
			if (!ok) {
				status = 'ready';
				return { ok: false, reason: 'cancelled' };
			}
		}
		return undefined;
	}

	async function undo(key?: FieldKey): Promise<SaveResult> {
		if (mode === 'instant') {
			if (!key) return { ok: false, reason: 'nothing' };
			const run = instantChain.then(() => {
				const spec = byKey.get(key);
				return spec ? holding(spec.section, () => undoInstant(key)) : undoInstant(key);
			});
			instantChain = run;
			return run;
		}
		if (inFlight) return { ok: false, reason: 'busy' };
		const batch = undoBatch;
		if (!batch || undoUntil === null || Date.now() > undoUntil)
			return { ok: false, reason: 'nothing' };
		const sent = new Set(Object.keys(batch));
		inFlight = sent;
		failedOp = 'undo';
		status = 'saving';
		try {
			const r = await send(batch);
			if (!r.ok) {
				status = 'ready';
				if ('cancelled' in r) return { ok: false, reason: 'cancelled' };
				const e = r.error;
				if (e instanceof ApiError && e.code === 'stale_revision') {
					// Never merge an Undo: say so and show what's there now.
					endUndo();
					announce(m.settings_form_announce_undo_stale());
					for (const s of sent) void refetch(s, true);
					return { ok: false, reason: 'conflict' };
				}
				return refused(e, r.batch);
			}
			applyResponse(r.batch, r.values);
			endUndo();
			restartPending = specs
				.filter((s) => sections[s.section]?.restart.includes(s.name))
				.map((s) => s.key);
			status = 'ready';
			announce(m.settings_form_announce_undone());
			schedulePreview();
			return { ok: true, restart: [] };
		} finally {
			afterSend(sent);
		}
	}

	async function removeSecret(key: FieldKey): Promise<SaveResult> {
		const spec = byKey.get(key);
		const [section, name] = splitKey(key);
		const s = sections[section];
		if (!spec?.writeOnly || !s) return { ok: false, reason: 'nothing' };
		if (inFlight) return { ok: false, reason: 'busy' };
		const batch: Batch = { [section]: { if_revision: s.revision, [name]: null } };
		const sent = new Set([section]);
		inFlight = sent;
		try {
			const r = await send(batch);
			if (!r.ok)
				return 'cancelled' in r ? { ok: false, reason: 'cancelled' } : refused(r.error, r.batch);
			applyResponse(r.batch, r.values);
			savedRecently = {};
			announce(m.settings_form_announce_secret_removed());
			return { ok: true, restart: [] };
		} finally {
			afterSend(sent);
		}
	}

	// --- instant mode ------------------------------------------------------------------------

	function scheduleInstant(spec: FieldSpec) {
		clearTimeout(instantTimers.get(spec.key));
		const run = () => {
			instantTimers.delete(spec.key);
			instantChain = instantChain.then(() => sendInstant(spec));
		};
		if (INSTANT_NOW.has(spec.kind)) run();
		else instantTimers.set(spec.key, setTimeout(run, DEBOUNCE_MS));
	}

	/** Runs an instant-mode send with its section's events held, like a staged save's. */
	async function holding<T>(section: string, f: () => Promise<T>): Promise<T> {
		const sent = new Set([section]);
		inFlight = sent;
		try {
			return await f();
		} finally {
			afterSend(sent);
		}
	}

	function sendInstant(spec: FieldSpec): Promise<SaveResult> {
		return holding(spec.section, () => sendInstantNow(spec));
	}

	async function sendInstantNow(spec: FieldSpec, retried = false): Promise<SaveResult> {
		const { section, name, key } = spec;
		const s = sections[section];
		const f = s?.fields[name];
		if (destroyed || !f || !isDirty(f)) return { ok: false, reason: 'nothing' };
		const message = validate(spec, f.mine);
		if (message !== undefined) {
			setFieldError(key, message);
			return { ok: false, reason: 'invalid' };
		}
		const prev = f.saved;
		const batch: Batch = { [section]: { if_revision: s.revision, [name]: wire(f.mine) } };
		// Danger classes gate instant changes too, and fail closed the same way.
		const gated = await runGates(batch, [key]);
		if (gated) {
			const now = sections[section];
			if (now) setSection(stage(now, name, undefined, spec));
			return gated;
		}
		const r = await send(batch);
		if (destroyed) return { ok: false, reason: 'cancelled' };
		if (r.ok) {
			applyResponse(r.batch, r.values);
			savedRecently = {};
			if (!spec.writeOnly) offerInstantUndo(key, prev); // a secret's old value is never known
			announce(m.settings_form_announce_changed());
			return { ok: true, restart: [] };
		}
		const e = 'error' in r ? r.error : undefined;
		if (!retried && e instanceof ApiError && e.code === 'stale_revision') {
			// The user just acted: take the new revision and try once more.
			const v = e.current?.[section] ?? (await getSection(section).catch(() => undefined));
			const cur = sections[section];
			if (v && cur) {
				setSection({ ...cur, revision: v.revision, restart: v.restart_required });
				return sendInstantNow(spec, true);
			}
		}
		// Put the old value back, with the error inline.
		const now = sections[section];
		if (now) setSection(stage(now, name, undefined, spec));
		const text =
			e instanceof ApiError
				? e.status === 0
					? m.settings_form_offline()
					: e.message
				: m.settings_form_error_server();
		if (!('cancelled' in r)) setFieldError(key, text);
		return { ok: false, reason: 'cancelled' in r ? 'cancelled' : 'refused' };
	}

	function offerInstantUndo(key: FieldKey, prev: SettingValue) {
		clearTimeout(instantUndoTimers.get(key));
		instantUndo = { ...instantUndo, [key]: { until: Date.now() + UNDO_MS, prev } };
		instantUndoTimers.set(
			key,
			setTimeout(() => {
				instantUndo = without(instantUndo, key);
				instantUndoTimers.delete(key);
			}, UNDO_MS)
		);
	}

	async function undoInstant(key: FieldKey): Promise<SaveResult> {
		const u = instantUndo[key];
		const spec = byKey.get(key);
		if (!u || !spec || Date.now() > u.until) return { ok: false, reason: 'nothing' };
		clearTimeout(instantUndoTimers.get(key));
		instantUndo = without(instantUndo, key);
		const s = sections[spec.section];
		if (!s) return { ok: false, reason: 'nothing' };
		const batch: Batch = {
			[spec.section]: {
				if_revision: s.revision,
				[spec.name]: u.prev.source === 'db' ? u.prev.value : null
			}
		};
		const r = await send(batch);
		if (!r.ok) {
			if ('cancelled' in r) return { ok: false, reason: 'cancelled' };
			const e = r.error;
			if (e instanceof ApiError && e.code === 'stale_revision') {
				announce(m.settings_form_announce_undo_stale());
				void refetch(spec.section, true);
				return { ok: false, reason: 'conflict' };
			}
			setFieldError(key, e instanceof ApiError ? e.message : m.settings_form_error_server());
			return { ok: false, reason: 'refused' };
		}
		applyResponse(r.batch, r.values);
		savedRecently = {};
		announce(m.settings_form_announce_undone());
		return { ok: true, restart: [] };
	}

	// --- events ------------------------------------------------------------------------------

	function slotFor(section: string) {
		let slot = fetchSlots.get(section);
		if (!slot) fetchSlots.set(section, (slot = latest()));
		return slot;
	}

	/**
	 * Refetches a section. `force` also applies the same revision (env and read-only changes
	 * don't move it) and says nothing about unchanged-looking updates.
	 */
	async function refetch(section: string, force = false): Promise<void> {
		try {
			const v = await slotFor(section).run((signal) => getSection(section, signal));
			if (destroyed) return;
			const s = sec(section);
			if (!s) return;
			if (inFlight?.has(section)) {
				// Our own save's response is authoritative; look again after it.
				held.set(section, Math.max(held.get(section) ?? 0, v.revision));
				return;
			}
			if (v.revision < s.revision || (v.revision === s.revision && !force)) return;
			mergeCurrent({ [section]: v }, force ? 'silent' : 'event');
		} catch {
			// Not shown: the next event or resync tries again.
		}
	}

	function received(e: SettingsEvent) {
		const s = sec(e.section);
		if (!s || e.revision <= s.revision) return;
		if (inFlight?.has(e.section)) {
			held.set(e.section, Math.max(held.get(e.section) ?? 0, e.revision));
			return;
		}
		void refetch(e.section);
	}

	async function resync() {
		await Promise.all(Object.keys(sections).map((s) => refetch(s, true)));
	}

	// --- misc --------------------------------------------------------------------------------

	async function retry() {
		if (status === 'load-error') return load();
		if (status !== 'offline' && status !== 'save-error') return;
		if (failedOp === 'save') {
			await save();
			return;
		}
		const r = await undo();
		if (!r.ok && r.reason === 'nothing') {
			// Undo's window closed while offline: there's nothing left to retry.
			status = 'ready';
			saveError = undefined;
		}
	}

	function destroy() {
		writeNow();
		destroyed = true;
		clearTimeout(undoTimer);
		clearTimeout(previewTimer);
		for (const t of instantTimers.values()) clearTimeout(t);
		for (const t of instantUndoTimers.values()) clearTimeout(t);
		previewSlot.cancel();
		for (const s of fetchSlots.values()) s.cancel();
	}

	void load();

	return {
		id: opts.id,
		mode,
		get specs() {
			return specs;
		},
		get status() {
			return status;
		},
		get loadError() {
			return loadError;
		},
		get saveError() {
			return saveError;
		},
		get dirtyCount() {
			return dirtyCount;
		},
		get errors() {
			const list: { key?: FieldKey; message: string }[] = formErrors.map((message) => ({
				message
			}));
			for (const s of specs) {
				const message = fieldErrors[s.key];
				if (message !== undefined) list.push({ key: s.key, message });
			}
			return list;
		},
		get conflicts() {
			return conflicts;
		},
		get restartPending() {
			return restartPending;
		},
		get undoUntil() {
			return undoUntil;
		},
		get undoSkipsSecrets() {
			return undoSkipsSecrets;
		},
		get draft() {
			return draftOffer;
		},
		get announcement() {
			return announcement;
		},
		href,
		field,
		set,
		reset,
		blur,
		save,
		discard,
		undo,
		resolve,
		restoreDraft,
		dropDraft,
		retry,
		removeSecret,
		onConfirm(code, handler) {
			confirms.set(code, handler);
			return () => {
				if (confirms.get(code) === handler) confirms.delete(code);
			};
		},
		gate(danger, handler) {
			gates.set(danger, handler);
			return () => {
				if (gates.get(danger) === handler) gates.delete(danger);
			};
		},
		received,
		resync,
		destroy
	};
}

function without<T>(record: Record<string, T>, key: string): Record<string, T> {
	const next = { ...record };
	delete next[key];
	return next;
}

function placeholderSpec(key: FieldKey): FieldSpec {
	const [section, name] = splitKey(key);
	return {
		key,
		section,
		name,
		labelKey: '',
		helpKey: '',
		kind: 'unknown',
		preview: false,
		group: '',
		order: 0,
		advanced: false,
		writeOnly: false,
		readOnly: true,
		restartRequired: false
	};
}
