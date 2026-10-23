<script lang="ts">
	// A screen made from the schema (§10.10.2, §10.10.8): the heading and its one-sentence purpose,
	// Show changed only, the groups, then anything the screen adds after them. This is the screen
	// that creates the form, so it destroys it when it unmounts (SettingsForm.svelte only borrows
	// it, and a re-mount must not lose its edits).
	import { page } from '$app/state';
	import { replaceState } from '$app/navigation';
	import { m } from '$lib/paraglide/messages.js';
	import Toggle from '$lib/ui/fields/Toggle.svelte';
	import { onMount, untrack, type Snippet } from 'svelte';
	import {
		createSettingsForm,
		loadSettingMessages,
		settingText,
		type FieldKey,
		type SettingsForm
	} from '../form';
	import SettingsFormView from '../form/SettingsForm.svelte';
	import type { ResolvedScreen } from '../shell/registry';
	import { useSettings } from '../shell/state.svelte';
	import ConfigDisclosure from './ConfigDisclosure.svelte';
	import SettingsGroups from './SettingsGroups.svelte';

	interface Props {
		resolved: ResolvedScreen;
		/** Extra content after a group (its id is the argument), and after all of them. */
		afterGroup?: Snippet<[group: string]>;
		footer?: Snippet;
		/** Called with the form once it exists (Network registers its lockout dialog); returns its cleanup. */
		oncreate?: (form: SettingsForm) => (() => void) | void;
	}

	let { resolved, afterGroup, footer, oncreate }: Props = $props();

	const settings = useSettings();
	const screen = untrack(() => resolved.screen);
	const mode = untrack(() => resolved.mode);

	const PURPOSE: Record<string, () => string> = {
		crawler: m.settings_ui_screen_crawler_purpose,
		network: m.settings_ui_screen_network_purpose,
		storage: m.settings_ui_screen_storage_purpose,
		search: m.settings_ui_screen_search_purpose,
		general: m.settings_ui_screen_general_purpose
	};
	const purpose = $derived.by(() => {
		const own = PURPOSE[screen.id];
		if (own) return own();
		const section = screen.sections?.[0];
		const help = settings.index?.sections.find((s) => s.section === section)?.helpKey;
		return help ? settingText(help) : undefined;
	});

	let form = $state<SettingsForm>();
	let groups = $state<SettingsGroups>();
	onMount(() => {
		const specs = settings.registry.fieldsFor(screen);
		let created: SettingsForm | undefined;
		let off: (() => void) | void;
		let gone = false;
		// The sections' text is in before the form mounts, so no label ever flashes as a field name.
		void loadSettingMessages(specs.map((s) => s.section)).then(() => {
			if (gone) return;
			created = createSettingsForm({ id: screen.id, fields: specs, mode });
			form = created;
			off = oncreate?.(created);
		});
		return () => {
			gone = true;
			off?.();
			created?.destroy();
		};
	});

	// Show changed only lives in the URL (`?changed=1`), so the Overview can link to it.
	// `replaceState` moves the address bar but not `page.url`, so the state is kept here and
	// follows `page.url` whenever a real navigation changes it.
	let changedOnly = $derived(page.url.searchParams.get('changed') === '1');
	function setChanged(on: boolean) {
		changedOnly = on;
		const url = new URL(page.url);
		if (on) url.searchParams.set('changed', '1');
		else url.searchParams.delete('changed');
		// eslint-disable-next-line svelte/no-navigation-without-resolve -- same page, no base path
		replaceState(url, page.state);
	}

	const reveal = (key: FieldKey) => groups?.reveal(key);
	const nothingChanged = $derived(
		changedOnly && form !== undefined && form.specs.every((s) => !form!.field(s.key).changed)
	);
</script>

<div class="flex flex-col gap-6" data-testid="settings-screen-{screen.id}">
	<header class="flex flex-col gap-2">
		<h1 tabindex="-1" class="text-display font-semibold outline-none">{screen.title()}</h1>
		{#if purpose}<p class="text-body text-muted-text">{purpose}</p>{/if}
	</header>
	{#if form}
		<Toggle
			label={m.settings_ui_changed_only()}
			checked={changedOnly}
			onchange={(e) => setChanged(e.currentTarget.checked)}
			data-testid="settings-changed-only"
		/>
		<SettingsFormView {form} onReveal={reveal} leaveGuard={form.mode === 'staged'}>
			{#if nothingChanged}
				<p data-testid="settings-nothing-changed">{m.settings_ui_nothing_changed()}</p>
			{/if}
			<SettingsGroups
				bind:this={groups}
				{form}
				screen={screen.id}
				{changedOnly}
				after={afterGroup}
			/>
			<ConfigDisclosure {form} specs={form.specs} />
			{@render footer?.()}
		</SettingsFormView>
	{:else}
		<p class="text-muted-text" aria-busy="true">{m.settings_ui_loading()}</p>
	{/if}
</div>
