<script lang="ts">
	// The theme picker (§10.18): a radio group of preview cards, drawn with each theme's own tokens.
	// Loaded when "Browse themes" is pressed; it brings `themes.json`.
	import { onMount, type Component, type ComponentProps } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Select from '$lib/ui/fields/Select.svelte';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import type { Appearance } from './prefs';
	import type { ThemeEntry, ThemesFile } from './themes';
	import './appearance.css';

	interface Props {
		prefs: Appearance;
		/** Whether the dark variant shows now (mode and the system's scheme). */
		dark: boolean;
		/** The theme preferences are set by the environment or read-only. */
		locked?: boolean;
		/** The shared `themes.json` loader and the browser's custom themes (themes.ts). */
		themes: () => Promise<ThemesFile>;
		custom: () => ThemeEntry[];
		write: (list: ThemeEntry[]) => void;
		onpick: (patch: Partial<Appearance>) => void;
	}
	let { prefs, dark, locked = false, themes, custom, write, onpick }: Props = $props();

	interface Family {
		id: string;
		name: string;
		group: string;
		light?: ThemeEntry;
		dark?: ThemeEntry;
		contrast: 'AA' | 'AAA';
	}

	let file = $state<ThemesFile>();
	let Editor = $state<Component<ComponentProps<typeof import('./ThemeEditor.svelte').default>>>();
	let editing = $state<string>();
	let editorKey = $state(0);
	let editorFailed = $state(false);
	let keys = $state<string[]>([]);
	/** A custom theme built for this build's tokens (themes.ts `usable`, inlined to keep chunks apart). */
	const ok = (t: ThemeEntry) =>
		t.keys?.length === keys.length && t.keys.every((k, i) => k === keys[i]);
	let all = $state<ThemeEntry[]>([]);
	let failed = $state(false);
	let filter = $state('all');

	onMount(async () => {
		try {
			file = await themes();
			keys = file.keys;
			all = [...file.themes, ...custom().filter(ok)];
		} catch {
			failed = true;
		}
	});

	const RIVERSTONE: Family = {
		id: 'riverstone',
		name: 'Riverstone',
		group: 'official',
		contrast: 'AA'
	};
	const families = $derived.by(() => {
		const by: Record<string, Family> = { riverstone: RIVERSTONE };
		for (const t of all) {
			const id = t.id.replace(/-(light|dark)$/, '');
			const f = by[id] ?? {
				id,
				name: t.family,
				group: t.custom ? 'custom' : t.group,
				contrast: t.contrast
			};
			f[t.mode] = t;
			if (t.contrast === 'AA' && f.contrast === 'AAA') f.contrast = 'AA';
			by[id] = f;
		}
		return Object.values(by);
	});
	const shown = $derived(families.filter((f) => filter === 'all' || f.group === filter));
	const filters = $derived([
		{ value: 'all', label: m.appearance_filter_all() },
		{ value: 'accessibility', label: m.appearance_filter_accessibility() },
		{ value: 'original', label: m.appearance_filter_original() },
		{ value: 'community', label: m.appearance_filter_community() },
		{ value: 'custom', label: m.appearance_filter_custom() }
	]);

	/** The custom properties a preview sets on itself: the theme's own, or Riverstone's from the CSS. */
	function style(f: Family): string | undefined {
		const t = f[dark ? 'dark' : 'light'] ?? f.light ?? f.dark;
		return t ? keys.map((k, i) => `${k}:${t.values[i]}`).join(';') : undefined;
	}

	async function openEditor(id?: string) {
		editing = id;
		editorKey++;
		Editor = undefined;
		editorFailed = false;
		try {
			Editor = (await import('./ThemeEditor.svelte')).default;
		} catch {
			editorFailed = true;
		}
	}

	let different = $state(false);
	// Follows `theme_dark` as it changes (Undo included), not only on mount.
	$effect.pre(() => {
		if (prefs.theme_dark) different = true;
	});
	const darkOptions = $derived([
		{ value: '', label: m.appearance_dark_same() },
		...families.map((f) => ({ value: f.id, label: f.name }))
	]);
</script>

{#if failed}
	<Banner kind="error">{m.appearance_picker_data_failed()}</Banner>
{:else}
	<div class="ap-filter">
		<label for="ap-filter" class="ap-label">{m.appearance_filter_label()}</label>
		<Select id="ap-filter" options={filters} bind:value={filter} />
	</div>
	<div role="radiogroup" aria-label={m.appearance_picker_label()} class="ap-grid">
		{#each shown as f (f.id)}
			<label
				class="ap-tile"
				style={style(f)}
				data-theme={f.id === 'riverstone' ? `riverstone-${dark ? 'dark' : 'light'}` : undefined}
			>
				<span class="ap-name">
					<span class="ap-pick">
						<input
							type="radio"
							class="radio"
							name="ap-theme"
							value={f.id}
							checked={prefs.theme === f.id}
							disabled={locked}
							onchange={() => onpick({ theme: f.id })}
						/>
						{f.name}
					</span>
					{#if f.light?.warn || f.dark?.warn}
						<Badge variant="warning">{m.appearance_custom_warning()}</Badge>
					{:else}
						<Badge variant="neutral">{m.appearance_contrast({ level: f.contrast })}</Badge>
					{/if}
				</span>
				<span class="ap-row" aria-hidden="true">
					<span>{m.appearance_card_result()}</span>
					<span class="ap-chip">{m.appearance_card_chip()}</span>
				</span>
				<span class="ap-bar" aria-hidden="true"
					><span></span><span></span><span></span><span></span><span></span><span></span></span
				>
				<span class="ap-btn" aria-hidden="true">{m.appearance_card_button()}</span>
			</label>
		{/each}
	</div>
	<div class="ap-actions">
		<Button disabled={locked} onclick={() => void openEditor()}>{m.appearance_create()}</Button>
		{#if prefs.theme.startsWith('custom-')}
			<Button onclick={() => void openEditor(prefs.theme)}>{m.appearance_edit()}</Button>
		{/if}
	</div>
	{#if editorFailed}<Banner kind="error">{m.appearance_editor_failed()}</Banner>{/if}
	{#if Editor && file}
		{#key editorKey}
			<Editor
				{file}
				custom={custom()}
				{write}
				{editing}
				onsave={(id) => {
					all = [...file!.themes, ...custom().filter(ok)];
					Editor = undefined;
					onpick({ theme: id });
				}}
				onclose={() => (Editor = undefined)}
			/>
		{/key}
	{/if}
	<Toggle
		label={m.appearance_dark_other()}
		checked={different}
		onchange={(e) => {
			different = e.currentTarget.checked;
			if (!different && prefs.theme_dark) onpick({ theme_dark: '' });
		}}
	/>
	{#if different}
		<label for="ap-dark" class="ap-label">{m.appearance_dark_label()}</label>
		<Select
			id="ap-dark"
			options={darkOptions}
			value={prefs.theme_dark}
			onchange={(e) => onpick({ theme_dark: e.currentTarget.value })}
		/>
	{/if}
{/if}
