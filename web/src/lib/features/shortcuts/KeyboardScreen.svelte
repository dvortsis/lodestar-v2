<script lang="ts">
	// Settings → Preferences → Keyboard (§10.10.8): single-key shortcuts on or off, and remapping.
	// Saves with Save and Cancel, sending `if_revision` so an edit never overwrites someone else's.
	// Moves onto the settings form engine (`createSettingsForm`) when that lands.
	import { onMount } from 'svelte';
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
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
	type Fields = Record<string, SettingLike>;
	type Name = keyof Prefs;
	const NAMES: Name[] = ['shortcuts', 'shortcut_keys'];
	const SECTION = { path: { section: 'preferences' } };

	let status = $state<'loading' | 'ready' | 'failed'>('loading');
	let revision = $state<number>();
	let saved = $state<Prefs>({ shortcuts: true, shortcut_keys: {} });
	let draft = $state<Prefs>({ shortcuts: true, shortcut_keys: {} });
	let locks = $state<Record<Name, string | null>>({ shortcuts: null, shortcut_keys: null });
	let saving = $state(false);
	let stale = $state(false);
	let failed = $state(false);
	let locked = $state(false);
	let nothing = $state(false);
	let errors = $state<Record<Name, Record<string, string>>>({ shortcuts: {}, shortcut_keys: {} });

	const same = (a: Prefs[Name], b: Prefs[Name]) =>
		JSON.stringify(sorted(a)) === JSON.stringify(sorted(b));
	const sorted = (v: Prefs[Name]) =>
		typeof v === 'object' ? Object.fromEntries(Object.entries(v).sort()) : v;
	const changed = (name: Name) => !same(draft[name], saved[name]);
	const dirty = $derived(NAMES.some((n) => !locks[n] && changed(n)));

	/** The other device's remaps with this tab's edits (adds, changes, removals) laid over them. */
	function rebase(base: KeyMap, old: KeyMap, mine: KeyMap): KeyMap {
		const out = { ...base };
		for (const id of new Set([...Object.keys(old), ...Object.keys(mine)])) {
			if (mine[id] === old[id]) continue;
			if (mine[id] === undefined) delete out[id];
			else out[id] = mine[id];
		}
		return out;
	}

	/** Takes the server's values; `keep` lays edits made over an older revision back on top. */
	function take(res: { fields: Fields; revision: number }, keep: boolean) {
		const next = narrow(res.fields.shortcuts?.value, res.fields.shortcut_keys?.value);
		if (!next) {
			status = 'failed';
			return;
		}
		const mine = $state.snapshot(draft) as Prefs;
		const old = $state.snapshot(saved) as Prefs;
		saved = next;
		draft = keep
			? {
					shortcuts: mine.shortcuts === old.shortcuts ? next.shortcuts : mine.shortcuts,
					shortcut_keys: rebase(next.shortcut_keys, old.shortcut_keys, mine.shortcut_keys)
				}
			: structuredClone(next);
		revision = res.revision;
		for (const n of NAMES) {
			const f = res.fields[n];
			locks[n] = f?.env
				? m.shortcuts_locked_env({ variable: f.env })
				: f?.read_only
					? m.shortcuts_locked_read_only()
					: null;
		}
		status = 'ready';
	}

	async function load(keep = false) {
		try {
			take((await api.get('/settings/{section}', SECTION)) as never, keep);
		} catch {
			// A 401 is the shell's; anything else is shown, with the form left as it was.
			if (status === 'loading') status = 'failed';
		}
	}

	/** The row a server message names: the first remappable action it mentions. */
	const rowOf = (message: string) =>
		ACTION_IDS.filter((id) => message.includes(id)).sort(
			(a, b) => message.indexOf(a) - message.indexOf(b)
		)[0] ?? '';

	async function save() {
		const body: Record<string, unknown> = { if_revision: revision };
		for (const n of NAMES) if (!locks[n] && changed(n)) body[n] = draft[n];
		if (!dirty) {
			nothing = true;
			return;
		}
		saving = true;
		nothing = stale = failed = locked = false;
		errors = { shortcuts: {}, shortcut_keys: {} };
		try {
			const res = await api.patch('/settings/{section}', {
				...SECTION,
				body: body as unknown as Record<string, never>
			});
			take(res as never, false);
			applyPrefs(saved);
			toast.push({ kind: 'success', text: m.shortcuts_saved() });
		} catch (e) {
			if (!(e instanceof ApiError)) failed = true;
			else if (e.code === 'stale_revision') {
				await load(true);
				stale = true;
			} else if (e.code === 'invalid_setting') {
				const name: Name = e.field === 'shortcuts' ? 'shortcuts' : 'shortcut_keys';
				errors[name] = { [name === 'shortcuts' ? '' : rowOf(e.message)]: e.message };
			} else if (e.status === 409) {
				await load(true);
				locked = true;
			} else failed = e.status !== 401;
		} finally {
			saving = false;
		}
	}

	function cancel() {
		nothing = false;
		draft = $state.snapshot(saved) as Prefs;
		errors = { shortcuts: {}, shortcut_keys: {} };
		stale = failed = false;
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
		<section>
			<Toggle
				label={m.shortcuts_single_label()}
				bind:checked={draft.shortcuts}
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
				value={draft.shortcut_keys}
				onchange={(next) => (draft.shortcut_keys = next)}
				errors={errors.shortcut_keys}
				disabled={!!locks.shortcut_keys}
			/>
		</section>
		<div class="sc-buttons">
			<Button variant="primary" loading={saving} onclick={() => void save()}>
				{m.shortcuts_save()}
			</Button>
			<Button disabled={saving} onclick={cancel}>{m.shortcuts_cancel()}</Button>
		</div>
		<p class="sc-text" role="status">{nothing && !dirty ? m.shortcuts_nothing_to_save() : ''}</p>
	{/if}
</div>
