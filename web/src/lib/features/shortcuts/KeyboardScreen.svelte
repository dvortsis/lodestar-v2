<script lang="ts">
	// Settings → Preferences → Keyboard (§10.10.8): single-key shortcuts on or off, and remapping.
	// A preference (§10.10.1): each change applies at once and saves through the `preferences`
	// section with `if_revision`, then offers Undo, as Appearance does. Saves go one at a time. A
	// stale revision reloads and lays only this change over the newer values, so it never overwrites
	// what another tab or device saved.
	import { onMount } from 'svelte';
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
	let undo = $state<Partial<Prefs> | null>(null);
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

	/** Sends one change; a stale revision reloads and tries once more over the newer values. */
	async function send(before: Partial<Prefs>, after: Partial<Prefs>): Promise<boolean> {
		let merged = false;
		for (;;) {
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
				if (!take(res as never)) throw new Error('malformed');
				return merged;
			} catch (e) {
				if (merged || !(e instanceof ApiError && e.code === 'stale_revision')) throw e;
				if (!take(await fetchSection())) throw e;
				merged = true;
			}
		}
	}

	/** Applies a change at once, then saves it after any save already under way. */
	function change(patch: Partial<Prefs>, undoing = false): Promise<void> {
		const names = NAMES.filter((n) => n in patch && !locks[n]);
		if (!names.length) return queue;
		const now = copy(prefs);
		const before = Object.fromEntries(names.map((n) => [n, now[n]])) as Partial<Prefs>;
		const after = structuredClone(
			Object.fromEntries(names.map((n) => [n, $state.snapshot(patch[n])]))
		) as Partial<Prefs>;
		prefs = { ...now, ...after };
		applyPrefs(prefs);
		undo = null;
		stale = failed = locked = false;
		errors = NO_ERRORS();
		saving++;
		queue = queue.then(async () => {
			try {
				stale = await send(before, after);
				if (!undoing) undo = before;
			} catch (e) {
				// Nothing of this change was saved; the screen goes back to the server's values below.
				if (!(e instanceof ApiError)) failed = true;
				else if (e.code === 'invalid_setting') {
					const name: Name = e.field === 'shortcuts' ? 'shortcuts' : 'shortcut_keys';
					errors[name] = { [name === 'shortcuts' ? '' : rowOf(e.message)]: e.message };
				} else if (e.code !== 'stale_revision' && e.status === 409) {
					try {
						take(await fetchSection());
					} catch {
						/* the banner still says why */
					}
					locked = true;
				} else failed = e.status !== 401;
			} finally {
				if (--saving === 0) {
					prefs = copy(saved);
					applyPrefs(prefs);
				}
			}
		});
		return queue;
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
			{#if undo}
				{m.shortcuts_saved()}
				<Button
					variant="quiet"
					onclick={() => {
						const back = undo!;
						undo = null;
						void change(back, true);
					}}>{m.shortcuts_undo()}</Button
				>
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
