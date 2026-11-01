<script lang="ts">
	// Settings → Preferences → Keyboard (§10.10.8): single-key shortcuts on or off, and remapping.
	// A preference (§10.10.1): each change applies at once and saves through the `preferences`
	// section with `if_revision`, then offers Undo, as Appearance does. Saves go one at a time. A
	// stale revision reloads and lays only this change over the newer values, so it never overwrites
	// what another tab or device saved.
	import { onMount, tick } from 'svelte';
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { applyPrefs, narrow, type Prefs } from './boot';
	import { ACTION_IDS } from './catalog';
	import type { KeyMap } from './capture';
	import { shown } from './keyname';
	import ShortcutsField from './ShortcutsField.svelte';
	import './shortcuts.css';

	interface SettingLike {
		value?: unknown;
		read_only?: boolean;
		env?: string | null;
		source?: string;
	}
	type Section = { fields: Record<string, SettingLike>; revision: number };
	type Name = keyof Prefs;
	const NAMES: Name[] = ['shortcuts', 'shortcut_keys'];
	const SECTION = { path: { section: 'preferences' } };
	const NO_ERRORS = () => ({ shortcuts: {}, shortcut_keys: {} });

	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let revision = 0;
	/** What the server last confirmed. */
	let saved = $state<Prefs>({ shortcuts: true, shortcut_keys: {} });
	/** What the screen shows and the registry runs: `saved` plus any change still saving. */
	let prefs = $state<Prefs>({ shortcuts: true, shortcut_keys: {} });
	let locks = $state<Record<Name, string | null>>({ shortcuts: null, shortcut_keys: null });
	let stale = $state(false);
	let failed = $state(false);
	let locked = $state(false);
	/** The last saved change, inverted: what Undo runs. */
	let undo = $state<Delta | null>(null);
	let errors = $state<Record<Name, Record<string, string>>>(NO_ERRORS());
	let queue: Promise<void> = Promise.resolve();
	let saving = 0;

	const copy = (p: Prefs): Prefs => structuredClone($state.snapshot(p)) as Prefs;

	/** The newer remaps with this change (adds, changes, removals from `old` to `mine`) laid over. */
	function rebase(base: KeyMap, old: KeyMap, mine: KeyMap): KeyMap {
		const out = { ...base };
		for (const id of new Set([...Object.keys(old), ...Object.keys(mine)])) {
			if (mine[id] === old[id]) continue;
			if (mine[id] === undefined) delete out[id];
			else out[id] = mine[id];
		}
		return out;
	}

	/** Takes the server's values and locks; false when they are malformed. */
	function take(res: Section): boolean {
		const next = narrow(res.fields.shortcuts?.value, res.fields.shortcut_keys?.value);
		if (!next) return false;
		saved = next;
		revision = res.revision;
		for (const n of NAMES) {
			const f = res.fields[n];
			locks[n] = f?.env
				? m.shortcuts_locked_env({ variable: f.env })
				: f?.read_only
					? m.shortcuts_locked_read_only()
					: null;
		}
		return true;
	}

	const fetchSection = async () => (await api.get('/settings/{section}', SECTION)) as never;

	async function load() {
		try {
			if (!take(await fetchSection())) throw new Error('malformed');
			prefs = copy(saved);
			status = 'ready';
		} catch {
			// A 401 is the shell's; anything else is shown.
			status = 'failed';
		}
	}

	/** The row a server message names: the first remappable action it mentions. */
	const rowOf = (message: string) =>
		ACTION_IDS.filter((id) => message.includes(id)).sort(
			(a, b) => message.indexOf(a) - message.indexOf(b)
		)[0] ?? '';

	/** One change: the fields it touches, as they were and as it makes them. */
	type Delta = { before: Partial<Prefs>; after: Partial<Prefs> };
	/** A 2xx reply that can't be read: the save may have landed, so re-read rather than say it failed. */
	class Unreadable extends Error {}

	/**
	 * Sends one change; a stale revision reloads and tries once more over the newer values. Remaps
	 * send only this change's own adds, changes and removals laid over the server's map. Resolves to
	 * whether it merged, and the switch as the server had it, for Undo.
	 */
	async function send({ before, after }: Delta): Promise<{ merged: boolean; was: boolean }> {
		let merged = false;
		for (;;) {
			const was = saved.shortcuts;
			const body: Record<string, unknown> = { if_revision: revision };
			if (after.shortcuts !== undefined) body.shortcuts = after.shortcuts;
			if (after.shortcut_keys)
				body.shortcut_keys = rebase(
					saved.shortcut_keys,
					before.shortcut_keys ?? {},
					after.shortcut_keys
				);
			try {
				const res = await api.patch('/settings/{section}', {
					...SECTION,
					body: body as unknown as Record<string, never>
				});
				if (!take(res as never)) throw new Unreadable();
				return { merged, was };
			} catch (e) {
				if (merged || !(e instanceof ApiError && e.code === 'stale_revision')) throw e;
				if (!take(await fetchSection())) throw e;
				merged = true;
			}
		}
	}

	/** A change made on the screen: from what it shows to `patch`. */
	const change = (patch: Partial<Prefs>) => {
		const now = copy(prefs);
		const names = NAMES.filter((n) => n in patch);
		return run({
			before: Object.fromEntries(names.map((n) => [n, now[n]])),
			after: structuredClone(Object.fromEntries(names.map((n) => [n, $state.snapshot(patch[n])])))
		});
	};

	/** Applies a change at once, then saves it after any save already under way. */
	function run(delta: Delta, undoing = false): Promise<void> {
		const names = NAMES.filter((n) => n in delta.after && !locks[n]);
		if (!names.length) return queue;
		const pick = (p: Partial<Prefs>) =>
			Object.fromEntries(names.map((n) => [n, p[n]])) as Partial<Prefs>;
		const d: Delta = { before: pick(delta.before), after: pick(delta.after) };
		const now = copy(prefs);
		if (d.after.shortcuts !== undefined) now.shortcuts = d.after.shortcuts;
		if (d.after.shortcut_keys)
			now.shortcut_keys = rebase(
				now.shortcut_keys,
				d.before.shortcut_keys ?? {},
				d.after.shortcut_keys
			);
		prefs = now;
		applyPrefs(prefs);
		undo = null;
		stale = failed = locked = false;
		errors = NO_ERRORS();
		saving++;
		queue = queue.then(async () => {
			try {
				const { merged, was } = await send(d);
				stale = merged;
				failed = locked = false;
				// Undo inverts this change only: never another device's, nor one that failed. It is
				// offered only once nothing later is queued, so it never belongs to an older change.
				if (!undoing && saving === 1)
					undo = {
						before: d.after,
						after: { ...d.before, ...('shortcuts' in d.after ? { shortcuts: was } : {}) }
					};
			} catch (e) {
				// Nothing of this change was saved. The screen goes back to the server's values, read
				// again so a change made elsewhere meanwhile isn't undone in the registry.
				if (e instanceof ApiError && e.status === 401) return;
				try {
					take(await fetchSection());
				} catch {
					/* the banner still says why */
				}
				if (e instanceof Unreadable) return;
				if (!(e instanceof ApiError)) failed = true;
				else if (e.code === 'invalid_setting') {
					const name: Name = e.field === 'shortcuts' ? 'shortcuts' : 'shortcut_keys';
					errors[name] = { [name === 'shortcuts' ? '' : rowOf(e.message)]: e.message };
				} else if (e.code !== 'stale_revision' && e.status === 409) locked = true;
				else failed = true;
			} finally {
				if (--saving === 0) {
					prefs = copy(saved);
					applyPrefs(prefs);
				}
			}
		});
		return queue;
	}

	/** Undo removes its own button: focus goes to the control the change was made with. */
	function undoLast() {
		const last = undo!;
		undo = null;
		void run(last, true);
		const { before, after } = last;
		const id = Object.keys({ ...before.shortcut_keys, ...after.shortcut_keys }).find(
			(k) => before.shortcut_keys?.[k] !== after.shortcut_keys?.[k]
		);
		void tick().then(() =>
			document
				.querySelector<HTMLElement>(
					id
						? `[data-testid="shortcut-change-${id}"]`
						: '[data-testid="keyboard-screen"] [role="switch"]'
				)
				?.focus()
		);
	}

	onMount(() => void load());
</script>

<div class="sc-screen" data-testid="keyboard-screen" aria-busy={status === 'loading'}>
	<h1 class="sc-h1">{m.shortcuts_screen_title()}</h1>
	<p class="sc-text">{m.shortcuts_screen_lead()}</p>
	{#if status === 'failed'}
		<Banner kind="error">{m.shortcuts_load_failed()}</Banner>
	{:else if status === 'ready'}
		{#if stale}<Banner kind="info">{m.shortcuts_stale()}</Banner>{/if}
		{#if locked}<Banner kind="warning">{m.shortcuts_read_only_banner()}</Banner>{/if}
		{#if failed}<Banner kind="error">{m.shortcuts_save_failed()}</Banner>{/if}
		<div role="status" class="sc-status">
			{#if undo && !NAMES.some((n) => n in undo!.after && locks[n])}
				{m.shortcuts_saved()}
				<Button variant="quiet" onclick={undoLast}>{m.shortcuts_undo()}</Button>
			{/if}
		</div>
		<section>
			<Toggle
				label={m.shortcuts_single_label()}
				checked={prefs.shortcuts}
				onchange={(e) => void change({ shortcuts: e.currentTarget.checked })}
				disabled={!!locks.shortcuts}
				aria-describedby="shortcuts-single-help"
			/>
			<p class="sc-text sc-help" id="shortcuts-single-help">
				{m.shortcuts_single_help({ palette: shown('mod+k') })}
			</p>
			{#if locks.shortcuts}<p class="sc-text sc-help">{locks.shortcuts}</p>{/if}
			{#each Object.values(errors.shortcuts) as message (message)}
				<p class="sc-text sc-help" role="alert">{message}</p>
			{/each}
		</section>
		<section aria-labelledby="shortcuts-keys-heading">
			<h2 class="sc-h2" id="shortcuts-keys-heading">{m.shortcuts_field_label()}</h2>
			{#if locks.shortcut_keys}<p class="sc-text sc-help">{locks.shortcut_keys}</p>{/if}
			<ShortcutsField
				value={prefs.shortcut_keys}
				onchange={(next) => void change({ shortcut_keys: next })}
				errors={errors.shortcut_keys}
				disabled={!!locks.shortcut_keys}
			/>
		</section>
	{/if}
</div>
