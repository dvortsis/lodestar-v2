<script lang="ts">
	// Test-only showcase (M-settings-form): the form engine over whatever `GET /settings` returns,
	// laid out the way S-settings-1 will lay out a screen. Playwright drives it over a fixture
	// schema (the OpenAPI mock) or a real `serve --fixture`.
	//   ?sections=crawler,network   which sections (default: all)
	//   ?mode=instant               Preferences-style instant saves
	//   ?id=…                       the draft key (default: showcase)
	import { page } from '$app/state';
	import {
		createSettingsForm,
		groupFields,
		loadSettingsIndex,
		settingText,
		type FieldKey,
		type SettingsForm
	} from '$lib/features/settings/form';
	import SettingField from '$lib/features/settings/form/SettingField.svelte';
	import SettingsFormView from '$lib/features/settings/form/SettingsForm.svelte';
	import { onMount } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';

	let form = $state<SettingsForm>();
	let failed = $state(false);
	const open = new SvelteSet<string>();

	onMount(() => {
		const params = page.url.searchParams;
		loadSettingsIndex().then(
			(index) => {
				const wanted = params.get('sections')?.split(',').filter(Boolean);
				const sections = wanted ?? index.sections.map((s) => s.section);
				form = createSettingsForm({
					id: params.get('id') ?? 'showcase',
					fields: index.fieldsOf(sections),
					mode: params.get('mode') === 'instant' ? 'instant' : 'staged'
				});
			},
			() => (failed = true)
		);
	});

	const groups = $derived(form ? groupFields(form.specs) : []);

	function reveal(key: FieldKey) {
		const g = groups.find((g) => g.advanced.some((s) => s.key === key));
		if (g) open.add(g.group);
	}
</script>

<div class="mx-auto flex max-w-(--manuscript-width) flex-col gap-6 p-4 text-base-content">
	<h1 class="text-display font-semibold">Settings form</h1>
	<!-- eslint-disable-next-line svelte/no-navigation-without-resolve -- no base path (svelte.config.js) -->
	<p><a class="link" href="/dev/ui/controls">Another page</a></p>
	{#if failed}
		<p role="alert">Couldn't load the settings index.</p>
	{:else if form}
		<SettingsFormView {form} onReveal={reveal} leaveGuard={form.mode === 'staged'}>
			{#each groups as g (g.group)}
				<section class="flex flex-col gap-4" aria-labelledby="group-{g.group}">
					<h2 id="group-{g.group}" class="text-title font-semibold">
						{settingText(g.labelKey) ?? g.group}
					</h2>
					{#each g.essentials as s (s.key)}
						<SettingField {form} key={s.key} />
					{/each}
					{#if g.advanced.length}
						<details
							open={open.has(g.group)}
							ontoggle={(e) => {
								if (e.currentTarget.open) open.add(g.group);
								else open.delete(g.group);
							}}
						>
							<summary class="cursor-pointer py-2">Advanced ({g.advanced.length})</summary>
							<div class="mt-2 flex flex-col gap-4">
								{#each g.advanced as s (s.key)}
									<SettingField {form} key={s.key} />
								{/each}
							</div>
						</details>
					{/if}
				</section>
			{/each}
		</SettingsFormView>
	{/if}
</div>
