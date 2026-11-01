<script lang="ts" module>
	import type { Component } from 'svelte';
	import type { About } from './screens';
	// Loaded bodies, so a return visit renders at once instead of showing the loading line again.
	const bodies = new WeakMap<object, Component>();
	// Loaded purpose lines and notices (`Screen.about`), for the same reason.
	const abouts = new WeakMap<object, About>();
</script>

<script lang="ts">
	import { untrack } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { loadSettingsIndex, type SettingsIndexView } from '../form/schema';
	import { aboutOf, screens, screenFields, type Screen } from './screens';
	let { section }: { section: string } = $props();
	let index = $state<SettingsIndexView>();
	let failed = $state(false);
	let attempt = $state(0);
	// The form engine is a generated screen's: a hand-built screen's first view never loads it.
	let SectionForm =
		$state<Component<{ screen: Screen; fields: ReturnType<typeof screenFields> }>>();
	let formFailed = $state(false);
	// The screen's purpose line and notice: their own small chunk, loaded with the form, so the
	// registry carries no long strings (screens.ts). Keyed by screen, so another screen's never
	// shows; the purpose line shows once it's in, the notice with the form.
	let about = $state<{ id: string; value: About }>();
	const screen = $derived(screens.find((screen) => screen.id === section));
	const generated = $derived(!!screen?.sections?.length);
	const fields = $derived(screen && index ? screenFields(screen, index) : []);
	$effect(() => {
		void attempt;
		if (!generated) return;
		let live = true;
		failed = false;
		// Both start at once, so the form's code never waits on the index (or the other way round).
		// Read without tracking: the form arriving must not re-run this and fetch the index again.
		if (!untrack(() => SectionForm))
			void import('./SectionForm.svelte').then(
				(mod) => {
					SectionForm = mod.default;
				},
				() => {
					formFailed = true;
				}
			);
		void loadSettingsIndex().then(
			(value) => {
				if (live) index = value;
			},
			() => {
				if (live) failed = true;
			}
		);
		return () => {
			live = false;
		};
	});
	$effect(() => {
		const current = screen;
		if (!current?.sections?.length || untrack(() => about?.id) === current.id) return;
		const known = abouts.get(current);
		if (known) {
			about = { id: current.id, value: known };
			return;
		}
		let live = true;
		void aboutOf(current).then(
			(value) => {
				abouts.set(current, value);
				if (live) about = { id: current.id, value };
			},
			() => {
				if (live) formFailed = true;
			}
		);
		return () => {
			live = false;
		};
	});
	const loadBody = (screen: Screen) =>
		screen.load!().then((mod) => {
			bodies.set(screen, mod.default);
			return mod.default;
		});
</script>

{#snippet chunkFailed()}
	<Banner kind="error"
		>{m.settings_form_chunk_failed()}{#snippet action()}<Button
				dense
				onclick={() => location.reload()}>{m.settings_form_reload()}</Button
			>{/snippet}</Banner
	>
{/snippet}

<svelte:head><title>{screen?.title() ?? m.settings_shell_title()} · Lodestar</title></svelte:head>
{#if screen?.load}
	{@const Body = bodies.get(screen)}
	{#if Body}
		<Body />
	{:else}
		{#await loadBody(screen)}
			<h1 class="text-title font-semibold">{screen.title()}</h1>
			<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
		{:then Loaded}
			<Loaded />
		{:catch}
			<h1 class="text-title font-semibold">{screen.title()}</h1>
			{@render chunkFailed()}
		{/await}
	{/if}
{:else}
	<h1 class="text-title font-semibold">{screen?.title() ?? m.settings_shell_title()}</h1>
	{#if !screen}
		<p>{m.settings_shell_unavailable()}</p>
	{:else}
		{@const text = about?.id === screen.id ? about.value : undefined}
		<!-- The purpose arrives with its chunk: its line is kept free meanwhile, so nothing shifts. -->
		{#if !text || text.purpose}<p class={text ? 'text-muted-text' : 'h-6'}>
				{text?.purpose?.()}
			</p>{/if}
		{#if failed}
			<Banner kind="error"
				>{m.settings_form_load_error()}{#snippet action()}<Button dense onclick={() => attempt++}
						>{m.settings_form_retry()}</Button
					>{/snippet}</Banner
			>
		{:else if formFailed}
			{@render chunkFailed()}
		{:else if !index || !SectionForm || !text}
			<p aria-busy="true" role="status">{m.settings_form_loading()}</p>
		{:else if !fields.length}
			<p>{m.settings_shell_server_unavailable()}</p>
		{:else}
			{#if text.notice}<Banner>{text.notice()}</Banner>{/if}
			{#if fields.some((field) => field.danger && field.danger !== 'disruptive')}<Banner
					>{m.settings_shell_guarded_readonly()}</Banner
				>{/if}
			{#key screen.id}<SectionForm {screen} {fields} />{/key}
			{#if screen.extra}
				{#await screen.extra() then extra}
					<extra.default />
				{:catch}
					{@render chunkFailed()}
				{/await}
			{/if}
		{/if}
	{/if}
{/if}
