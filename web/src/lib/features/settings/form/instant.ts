// Instant mode (Preferences screens): each change saves on its own, with its own Undo. Loaded
// by `form.svelte.ts` when the first instant field changes, so a section's first view never
// carries it. It sends through the save pipeline (`saving.ts`).
import { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import { DEBOUNCE_MS, UNDO_MS, without, type Engine } from './form.svelte';
import { sectionValues } from './merge';
import { announce, applyResponse, holding, runGates, send } from './saving';
import { validate } from './validate';
import { getSection, type Batch } from './requests';
import type { FieldKey, FieldSpec, SaveResult, SectionValues, SettingValue } from './types';
import { wire } from './batch';
import { isDirty, stage } from './values';

/** Kinds whose change is sent at once rather than debounced. */
const NOW = new Set(['boolean', 'enum']);

export interface Instant {
	/** A changed field (never a secret): sent at once (toggles, choices) or debounced. */
	schedule(spec: FieldSpec): void;
	/** The explicit commit: the staged secrets, each as its own change. */
	saveSecrets(): Promise<SaveResult>;
	undo(key: FieldKey): Promise<SaveResult>;
	destroy(): void;
}

export function createInstant(e: Engine): Instant {
	let chain: Promise<unknown> = Promise.resolve();
	const timers = new Map<string, ReturnType<typeof setTimeout>>();
	const undoTimers = new Map<string, ReturnType<typeof setTimeout>>();

	function queue<T>(f: () => Promise<T>): Promise<T> {
		const run = chain.then(f);
		chain = run;
		return run;
	}

	/** Puts the saved value back. */
	function revert(spec: FieldSpec) {
		const now = e.sections[spec.section];
		if (now) e.setSection(stage(now, spec.name, undefined, spec));
	}

	function applied(batch: Batch, values: Record<string, SectionValues>) {
		applyResponse(e, batch, values);
		e.savedRecently = {};
	}

	/** Runs an instant-mode send with its section's events held, like a staged save's. */
	const sendOne = (spec: FieldSpec) =>
		holding(e, new Set([spec.section]), [spec.key], () => sendNow(spec));

	async function sendNow(spec: FieldSpec, retried = false): Promise<SaveResult> {
		const { section, name, key } = spec;
		const s = e.sections[section];
		const f = s?.fields[name];
		if (e.destroyed || !s || !f || !isDirty(f)) return { ok: false, reason: 'nothing' };
		const message = validate(spec, f.mine);
		if (message !== undefined) {
			e.setFieldError(key, message);
			return { ok: false, reason: 'invalid' };
		}
		const prev = f.saved;
		const batch: Batch = { [section]: { if_revision: s.revision, [name]: wire(f.mine) } };
		// Danger classes gate instant changes too, and fail closed the same way.
		const gated = await runGates(e, batch, [key]);
		if (gated) {
			revert(spec);
			return gated;
		}
		const r = await send(e, batch);
		if (e.destroyed) return { ok: false, reason: 'cancelled' };
		if (r.ok) {
			applied(r.batch, r.values);
			if (!spec.writeOnly) offerUndo(key, prev); // a secret's old value is never known
			announce(e, m.settings_form_announce_changed());
			return { ok: true, restart: [] };
		}
		const err = 'error' in r ? r.error : undefined;
		if (!retried && err instanceof ApiError && err.code === 'stale_revision') {
			// The user just acted: take the new revision and try once more.
			const v =
				sectionValues(err.current)[section] ?? (await getSection(section).catch(() => undefined));
			const cur = e.sections[section];
			if (v && cur) {
				e.setSection({ ...cur, revision: v.revision, restart: v.restart_required });
				return sendNow(spec, true);
			}
		}
		// Put the old value back, with the error inline.
		revert(spec);
		const text =
			err instanceof ApiError
				? err.status === 0
					? m.settings_form_offline()
					: err.message
				: m.settings_form_error_server();
		if (!('cancelled' in r)) e.setFieldError(key, text);
		return { ok: false, reason: 'cancelled' in r ? 'cancelled' : 'refused' };
	}

	function offerUndo(key: FieldKey, prev: SettingValue) {
		clearTimeout(undoTimers.get(key));
		e.instantUndo = { ...e.instantUndo, [key]: { until: Date.now() + UNDO_MS, prev } };
		undoTimers.set(
			key,
			setTimeout(() => {
				e.instantUndo = without(e.instantUndo, key);
				undoTimers.delete(key);
			}, UNDO_MS)
		);
	}

	async function undoNow(key: FieldKey): Promise<SaveResult> {
		const u = e.instantUndo[key];
		const spec = e.byKey.get(key);
		if (!u || !spec || Date.now() > u.until) return { ok: false, reason: 'nothing' };
		clearTimeout(undoTimers.get(key));
		e.instantUndo = without(e.instantUndo, key);
		const s = e.sections[spec.section];
		if (!s) return { ok: false, reason: 'nothing' };
		const batch: Batch = {
			[spec.section]: {
				if_revision: s.revision,
				[spec.name]: u.prev.source === 'db' ? u.prev.value : null
			}
		};
		const r = await send(e, batch);
		if (!r.ok) {
			if ('cancelled' in r) return { ok: false, reason: 'cancelled' };
			const err = r.error;
			if (err instanceof ApiError && err.code === 'stale_revision') {
				announce(e, m.settings_form_announce_undo_stale());
				void e.refetch(spec.section, true);
				return { ok: false, reason: 'conflict' };
			}
			e.setFieldError(key, err instanceof ApiError ? err.message : m.settings_form_error_server());
			return { ok: false, reason: 'refused' };
		}
		applied(r.batch, r.values);
		announce(e, m.settings_form_announce_undone());
		return { ok: true, restart: [] };
	}

	return {
		schedule(spec) {
			clearTimeout(timers.get(spec.key));
			const run = () => {
				timers.delete(spec.key);
				void queue(() => sendOne(spec));
			};
			if (NOW.has(spec.kind)) run();
			else timers.set(spec.key, setTimeout(run, DEBOUNCE_MS));
		},
		async saveSecrets() {
			const staged = e.specs.filter((s) => {
				const f = e.sections[s.section]?.fields[s.name];
				return s.writeOnly && f !== undefined && isDirty(f);
			});
			let last: SaveResult = { ok: false, reason: 'nothing' };
			for (const spec of staged) {
				last = await queue(() => sendOne(spec));
				// Never kept once tried (§10.10.4).
				if (e.sections[spec.section]?.fields[spec.name]?.mine !== undefined) revert(spec);
				if (!last.ok) return last;
			}
			return last;
		},
		undo(key) {
			return queue(() => {
				const spec = e.byKey.get(key);
				return spec
					? holding(e, new Set([spec.section]), undefined, () => undoNow(key))
					: undoNow(key);
			});
		},
		destroy() {
			for (const t of timers.values()) clearTimeout(t);
			for (const t of undoTimers.values()) clearTimeout(t);
		}
	};
}
