// One form per screen or wizard step (§10.10.1, §10.10.5). A plain store: everything tied to a
// component's lifetime (the `settings` subscription, ⌘/Ctrl+S, the leave guard) is wired by
// `SettingsForm.svelte`, which calls `received`/`resync` and registers the default gate.
//
// This file is what a section's first view needs: loading, the field view, staging an edit,
// previews and events. The rest loads on demand, once per page: `saving.ts` (the save pipeline,
// Undo, the merge, drafts and validation) with the first edit, a stored draft, a `settings`
// event or a save; `instant.ts` with the first instant change. Both work on the `Engine`.
//
// The Maps and Sets here are bookkeeping that nothing renders; reactive state is `$state.raw`
// records replaced on every change.
/* eslint-disable svelte/prefer-svelte-reactivity */
import { ApiError, isAbort, latest } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import { draftOf, hasDraft, writeDraft, type Draft } from './draft-store';
import type { Instant } from './instant';
import { getSection, type Batch } from './requests';
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
	type SettingsEvent,
	type SettingsPreview,
	type SettingValue
} from './types';
import { fromValues, isDirty, isLocked, RESET, stage, type SectionState } from './values';

/** How long Undo is offered after a save (§10.10.5). */
export const UNDO_MS = 10_000;
/** Debounce for drafts, previews and instant-mode text and numbers. */
export const DEBOUNCE_MS = 300;

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
	/** The latest save's result, from any caller (the bar, ⌘/Ctrl+S, the wizard's Next). */
	readonly lastSave: { id: number; result: SaveResult } | null;
	href(key: FieldKey): string;
	field(key: FieldKey): FieldState;
	set(key: FieldKey, value: unknown): void;
	reset(key: FieldKey): void;
	blur(key: FieldKey): void;
	/** One atomic save. A save that resolves after `destroy()` returns `cancelled`, whatever the
	 * server did: nothing is left to show it (the next screen loads the saved values). */
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
	/** A fallback for `danger`, used only while no `gate` handler is registered for it. */
	defaultGate(danger: Danger, handler: GateHandler): () => void;
	/** A `settings` event (`SettingsForm.svelte` subscribes). */
	received(e: SettingsEvent): void;
	/** The events stream resynced: refetch every section. */
	resync(): Promise<void>;
	destroy(): void;
}

export type Sections = Record<string, SectionState>;

let savingMod: typeof import('./edit') | undefined;
let previewMod: typeof import('./preview') | undefined;
let instantMod: typeof import('./instant') | undefined;

/** A lazy import, loaded once; a failed load is tried again next time. */
function once<T>(load: () => Promise<T>, loaded: (mod: T) => void): () => Promise<T> {
	let p: Promise<T> | undefined;
	return () =>
		(p ??= load().then(
			(mod) => {
				loaded(mod);
				return mod;
			},
			(e: unknown) => {
				p = undefined;
				throw e;
			}
		));
}

const loadPreview = once(
	() => import('./preview'),
	(mod) => (previewMod = mod)
);
const loadSaving = once(
	() => import('./edit'),
	(mod) => (savingMod = mod)
);
// Instant mode sends through the save pipeline: it comes along.
const loadInstant = once(
	() => Promise.all([import('./instant'), loadSaving()]).then(([mod]) => mod),
	(mod) => (instantMod = mod)
);
const ignore = () => {};

/**
 * Loads now what the engine otherwise loads on demand: the edit chunk and previews, and with
 * `instant` instant mode. `SettingsForm.svelte` calls it once the browser is idle after the
 * first view.
 */
export async function preloadSettingsForm(instant = true): Promise<void> {
	await Promise.all([instant ? loadInstant() : loadSaving(), loadPreview()]);
}

/** A form's state and its first-view half. Internal: screens use `SettingsForm`. */
export class Engine {
	readonly id: string;
	readonly mode: 'staged' | 'instant';
	/** Drafts in `sessionStorage`: staged mode only. */
	readonly useDrafts: boolean;

	specs = $state.raw<FieldSpec[]>([]);
	byKey = new Map<string, FieldSpec>();
	sections = $state.raw<Sections>({});
	status = $state<FormStatus>('loading');
	loadError = $state.raw<ApiError | undefined>();
	saveError = $state.raw<ApiError | undefined>();
	fieldErrors = $state.raw<Record<string, string>>({});
	previewErrors = $state.raw<Record<string, string>>({});
	formErrors = $state.raw<string[]>([]);
	preview = $state.raw<SettingsPreview | null>(null);
	restartPending = $state.raw<FieldKey[]>([]);
	savedRecently = $state.raw<Record<string, 'live' | 'restart'>>({});
	undoUntil = $state<number | null>(null);
	undoSkipsSecrets = $state(false);
	instantUndo = $state.raw<Record<string, { until: number; prev: SettingValue }>>({});
	draftOffer = $state.raw<null | { fields: number; dropped: number }>(null);
	announcement = $state.raw<{ id: number; text: string } | null>(null);
	lastSave = $state.raw<{ id: number; result: SaveResult } | null>(null);

	// Not rendered: plain fields.
	saveSeq = 0;
	undoBatch: Batch | null = null;
	undoTimer: ReturnType<typeof setTimeout> | undefined;
	draftTimer: ReturnType<typeof setTimeout> | undefined;
	previewTimer: ReturnType<typeof setTimeout> | undefined;
	draftData: Draft | null = null;
	inFlight: Set<string> | null = null; // sections of the save in flight
	sending = new Set<string>(); // its fields: an edit to one is kept until the response
	failedOp: 'save' | 'undo' = 'save'; // what Retry re-runs
	held = new Map<string, number>(); // events held during it
	destroyed = false;
	announceSeq = 0;
	instant: Instant | undefined;
	readonly confirms = new Map<string, ConfirmHandler>();
	readonly gates = new Map<Danger, GateHandler>();
	readonly fallbackGates = new Map<Danger, GateHandler>();
	readonly fetchSlots = new Map<string, ReturnType<typeof latest>>();
	readonly previewSlot = latest();

	constructor(private readonly opts: SettingsFormOptions) {
		this.id = opts.id;
		this.mode = opts.mode ?? 'staged';
		this.useDrafts = this.mode === 'staged' && (opts.drafts ?? true);
	}

	dirtyCount = $derived.by(() => {
		let n = 0;
		for (const s of this.specs) {
			const f = this.sections[s.section]?.fields[s.name];
			if (f && (isDirty(f) || f.conflict)) n++;
		}
		return n;
	});

	conflicts = $derived(
		this.specs.filter((s) => this.sections[s.section]?.fields[s.name]?.conflict).map((s) => s.key)
	);

	gateFor = (d: Danger) => this.gates.get(d) ?? this.fallbackGates.get(d);
	specOf = (section: string, name: string) => this.byKey.get(`${section}.${name}`);
	/** A section by a name from outside (an event, a caller's key): own properties only. */
	sec = (section: string): SectionState | undefined =>
		Object.hasOwn(this.sections, section) ? this.sections[section] : undefined;
	inForm = (section: string, name: string) => this.byKey.has(`${section}.${name}`);

	announce(text: string) {
		this.announcement = { id: ++this.announceSeq, text };
	}

	setSection(s: SectionState) {
		this.sections = { ...this.sections, [s.section]: s };
	}

	setFieldError(key: string, message: string | undefined) {
		if (this.fieldErrors[key] === message) return;
		const next = { ...this.fieldErrors };
		if (message === undefined) delete next[key];
		else next[key] = message;
		this.fieldErrors = next;
	}

	dirtyKeys(): FieldKey[] {
		return this.specs
			.filter((s) => {
				const f = this.sections[s.section]?.fields[s.name];
				return f !== undefined && (isDirty(f) || f.conflict !== undefined);
			})
			.map((s) => s.key);
	}

	// --- loading ---------------------------------------------------------------------------

	async load() {
		this.status = 'loading';
		this.loadError = undefined;
		try {
			let list: FieldSpec[];
			const fields = this.opts.fields;
			if (fields.length && typeof fields[0] === 'string') {
				const index = await loadSettingsIndex();
				list = [];
				for (const key of fields as FieldKey[]) {
					const spec = index.spec(key);
					if (spec) list.push(spec);
					else if (import.meta.env.DEV) devWarn(`${this.id}: no setting ${key}`);
				}
			} else list = fields as FieldSpec[];
			this.byKey = new Map(list.map((s) => [s.key, s]));
			this.specs = list;
			const names = [...new Set(list.map((s) => s.section))];
			// A stored draft is offered with the first view: its code loads with the values.
			const offer = this.useDrafts && hasDraft(this.id) ? loadSaving().catch(ignore) : undefined;
			const values = await Promise.all(names.map((s) => getSection(s)));
			await offer;
			if (this.destroyed) return;
			const next: Sections = {};
			for (const v of values) next[v.section] = fromValues(v);
			this.sections = next;
			this.status = 'ready';
			if (this.useDrafts) savingMod?.offerDraft(this);
			this.schedulePreview();
		} catch (e) {
			if (this.destroyed || isAbort(e)) return;
			this.loadError = e instanceof ApiError ? e : new ApiError(0, 'network', String(e));
			this.status = 'load-error';
		}
	}

	// --- field view --------------------------------------------------------------------------

	field(key: FieldKey): FieldState {
		const spec = this.byKey.get(key);
		const [section, name] = splitKey(key);
		const f = spec ? this.sec(section)?.fields[name] : undefined;
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
		const p = this.preview?.sections[section]?.fields[name];
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
		const error = this.fieldErrors[key] ?? this.previewErrors[key];
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
		if (this.savedRecently[key]) state.savedRecently = this.savedRecently[key];
		if (this.instantUndo[key]) state.undoUntil = this.instantUndo[key].until;
		return state;
	}

	// --- editing -----------------------------------------------------------------------------

	set(key: FieldKey, value: unknown) {
		const spec = this.byKey.get(key);
		const [section, name] = splitKey(key);
		const s = spec ? this.sec(section) : undefined;
		if (!spec || !s) return;
		const next = stage(s, name, value, spec, this.sending.has(key));
		if (next === s) return;
		this.setSection(next);
		// A server error no longer applies once the value changes; a client one is re-checked.
		// Errors come from the save pipeline, so it has loaded, except instant mode's "couldn't
		// load", which stays until a change gets through.
		if (this.fieldErrors[key] !== undefined) savingMod?.recheck(this, key);
		if (this.previewErrors[key] !== undefined) {
			this.previewErrors = without(this.previewErrors, key);
		}
		if (this.mode === 'instant') return this.scheduleInstant(spec);
		if (!savingMod) void loadSaving().catch(ignore);
		this.scheduleDraft();
		this.schedulePreview();
	}

	blur(key: FieldKey) {
		const spec = this.byKey.get(key);
		const [section, name] = splitKey(key);
		const f = spec ? this.sec(section)?.fields[name] : undefined;
		if (!spec || !f) return;
		if (!isDirty(f)) this.setFieldError(key, undefined);
		// A dirty field was edited, which loads the pipeline; it may still be on its way.
		else if (savingMod) savingMod.recheck(this, key);
		else void loadSaving().then(() => this.blur(key), ignore);
	}

	// --- drafts ------------------------------------------------------------------------------

	writeNow() {
		clearTimeout(this.draftTimer);
		this.draftTimer = undefined;
		// An offered draft isn't overwritten before the user restores or discards it.
		if (!this.useDrafts || this.draftOffer || this.status === 'loading') return;
		writeDraft(this.id, draftOf(this.sections, this.specOf, Date.now()));
	}

	scheduleDraft() {
		if (!this.useDrafts) return;
		clearTimeout(this.draftTimer);
		this.draftTimer = setTimeout(() => this.writeNow(), DEBOUNCE_MS);
	}

	// --- preview -----------------------------------------------------------------------------

	schedulePreview() {
		if (this.destroyed || !this.specs.some((s) => s.preview)) return;
		clearTimeout(this.previewTimer);
		// Its code loads during the debounce.
		if (!previewMod) void loadPreview().catch(ignore);
		this.previewTimer = setTimeout(() => {
			if (previewMod) void previewMod.runPreview(this);
			else void loadPreview().then((mod) => mod.runPreview(this), ignore);
		}, DEBOUNCE_MS);
	}

	// --- instant mode ------------------------------------------------------------------------

	async instantMode(): Promise<Instant> {
		if (!instantMod) await loadInstant();
		return (this.instant ??= instantMod!.createInstant(this));
	}

	scheduleInstant(spec: FieldSpec) {
		// A secret is sent only when the user commits it (`save()`), never half-typed.
		if (spec.writeOnly) return;
		if (instantMod) {
			(this.instant ??= instantMod.createInstant(this)).schedule(spec);
			return;
		}
		this.instantMode().then(
			(i) => {
				if (!this.destroyed) i.schedule(spec);
			},
			() => {
				// Its code didn't load: put the old value back, as a failed send does.
				const s = this.sec(spec.section);
				if (s) this.setSection(stage(s, spec.name, undefined, spec));
				this.setFieldError(spec.key, m.settings_form_offline());
			}
		);
	}

	// --- events ------------------------------------------------------------------------------

	slotFor(section: string) {
		let slot = this.fetchSlots.get(section);
		if (!slot) this.fetchSlots.set(section, (slot = latest()));
		return slot;
	}

	/**
	 * Refetches a section. `force` also applies the same revision (env and read-only changes
	 * don't move it) and says nothing about unchanged-looking updates.
	 */
	async refetch(section: string, force = false): Promise<void> {
		try {
			const v = await this.slotFor(section).run((signal) => getSection(section, signal));
			const pipeline = savingMod ?? (await loadSaving()); // the merge
			if (this.destroyed) return;
			const s = this.sec(section);
			if (!s) return;
			if (this.inFlight?.has(section)) {
				// Our own save's response is authoritative; look again after it.
				this.held.set(section, Math.max(this.held.get(section) ?? 0, v.revision));
				return;
			}
			if (v.revision < s.revision || (v.revision === s.revision && !force)) return;
			pipeline.mergeCurrent(this, { [section]: v }, force ? 'silent' : 'event');
		} catch {
			// Not shown: the next event or resync tries again.
		}
	}

	received(e: SettingsEvent) {
		const s = this.sec(e.section);
		if (!s || e.revision <= s.revision) return;
		if (this.inFlight?.has(e.section)) {
			this.held.set(e.section, Math.max(this.held.get(e.section) ?? 0, e.revision));
			return;
		}
		void this.refetch(e.section);
	}

	destroy() {
		this.writeNow();
		this.destroyed = true;
		clearTimeout(this.undoTimer);
		clearTimeout(this.previewTimer);
		this.instant?.destroy();
		this.previewSlot.cancel();
		for (const s of this.fetchSlots.values()) s.cancel();
	}
}

export function createSettingsForm(opts: SettingsFormOptions): SettingsForm {
	const e = new Engine(opts);
	const href = opts.href ?? ((key: FieldKey) => `${globalThis.location?.pathname ?? ''}#${key}`);
	const nothing: SaveResult = { ok: false, reason: 'nothing' };

	void e.load();

	return {
		id: e.id,
		mode: e.mode,
		get specs() {
			return e.specs;
		},
		get status() {
			return e.status;
		},
		get loadError() {
			return e.loadError;
		},
		get saveError() {
			return e.saveError;
		},
		get dirtyCount() {
			return e.dirtyCount;
		},
		get errors() {
			const list: { key?: FieldKey; message: string }[] = e.formErrors.map((message) => ({
				message
			}));
			for (const s of e.specs) {
				const message = e.fieldErrors[s.key];
				if (message !== undefined) list.push({ key: s.key, message });
			}
			return list;
		},
		get conflicts() {
			return e.conflicts;
		},
		get restartPending() {
			return e.restartPending;
		},
		get undoUntil() {
			return e.undoUntil;
		},
		get undoSkipsSecrets() {
			return e.undoSkipsSecrets;
		},
		get draft() {
			return e.draftOffer;
		},
		get announcement() {
			return e.announcement;
		},
		get lastSave() {
			return e.lastSave;
		},
		href,
		field: (key) => e.field(key),
		set: (key, value) => e.set(key, value),
		reset: (key) => e.set(key, RESET),
		blur: (key) => e.blur(key),
		async save() {
			let pipeline = savingMod;
			try {
				pipeline ??= await loadSaving();
			} catch {
				return { ok: false, reason: 'network' };
			}
			return pipeline.save(e);
		},
		// Something to discard was staged, which loaded the pipeline.
		discard: () => savingMod?.discard(e),
		async undo(key) {
			// An Undo is offered only after a save or an instant change: its code has loaded.
			if (e.mode === 'instant') return key && e.instant ? e.instant.undo(key) : nothing;
			return savingMod ? savingMod.undo(e) : nothing;
		},
		// A conflict and an offered draft come from the pipeline, so it has loaded.
		resolve: (key, pick) => savingMod?.resolve(e, key, pick),
		restoreDraft: () => savingMod?.restoreDraft(e),
		dropDraft: () => savingMod?.dropDraft(e),
		async retry() {
			if (e.status === 'load-error') return e.load();
			// Offline and save errors come from the pipeline.
			await savingMod?.retry(e);
		},
		async removeSecret(key) {
			let pipeline = savingMod;
			try {
				pipeline ??= await loadSaving();
			} catch {
				return { ok: false, reason: 'network' };
			}
			return pipeline.removeSecret(e, key);
		},
		onConfirm(code, handler) {
			e.confirms.set(code, handler);
			return () => {
				if (e.confirms.get(code) === handler) e.confirms.delete(code);
			};
		},
		gate(danger, handler) {
			e.gates.set(danger, handler);
			return () => {
				if (e.gates.get(danger) === handler) e.gates.delete(danger);
			};
		},
		defaultGate(danger, handler) {
			e.fallbackGates.set(danger, handler);
			return () => {
				if (e.fallbackGates.get(danger) === handler) e.fallbackGates.delete(danger);
			};
		},
		received: (ev) => e.received(ev),
		async resync() {
			await Promise.all(Object.keys(e.sections).map((s) => e.refetch(s, true)));
		},
		destroy: () => e.destroy()
	};
}

export function without<T>(record: Record<string, T>, key: string): Record<string, T> {
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
