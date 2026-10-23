<script lang="ts">
	// `/settings/<id>`: finds the screen in the registry and renders it. A hand-built screen
	// (`load`) is loaded on open and renders alone; any other is generated from the schema, with
	// the extras a few screens add (Network's reachability panel). Opening a screen moves focus to
	// its `<h1>`, unless the URL hash names where to go (§10.10.11).
	import { afterNavigate } from '$app/navigation';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import { tick, type Component } from 'svelte';
	import GeneratedScreen from '../screens/GeneratedScreen.svelte';
	import { focusHeading } from './focus';
	import { useSettings } from './state.svelte';

	let { id }: { id: string } = $props();

	const settings = useSettings();
	const resolved = $derived(settings.registry.get(id));
	const load = $derived(resolved?.screen.load);

	/** Generated screens that add something of their own, loaded with the screen. */
	type Extra = Component<{ resolved: NonNullable<typeof resolved> }>;
	const EXTRAS: Record<string, () => Promise<{ default: Component<never> }>> = {
		network: () => import('../screens/NetworkScreen.svelte'),
		general: () => import('../screens/GeneralScreen.svelte')
	};
	let Extras = $state.raw<Extra>();
	$effect(() => {
		const loader = Object.hasOwn(EXTRAS, id) ? EXTRAS[id] : undefined;
		Extras = undefined;
		if (!loader) return;
		let live = true;
		void loader().then((mod) => live && (Extras = mod.default as unknown as Extra));
		return () => (live = false);
	});
	const wantsExtras = $derived(Object.hasOwn(EXTRAS, id));

	let Loaded = $state.raw<Component>();
	let failed = $state(false);
	let attempt = $state(0);
	$effect(() => {
		void attempt;
		const fn = load;
		Loaded = undefined;
		failed = false;
		if (!fn) return;
		let live = true;
		fn().then(
			(mod) => live && (Loaded = mod.default),
			() => live && (failed = true)
		);
		return () => (live = false);
	});

	// Focus waits for the heading to exist: at once for a generated screen, after the load for a
	// hand-built one.
	let pending = false;
	afterNavigate(() => {
		pending = location.hash === '';
		void settle();
	});
	const present = $derived(
		resolved !== undefined &&
			(load
				? Loaded !== undefined
				: settings.status === 'ready' && (!wantsExtras || Extras !== undefined))
	);
	$effect(() => {
		if (present) void settle();
	});
	async function settle() {
		if (!pending || !present) return;
		await tick();
		if (pending && focusHeading()) pending = false;
	}
</script>

{#if resolved}
	{#if load}
		{#if Loaded}
			<Loaded />
		{:else if failed}
			<Banner kind="error">
				{m.settings_ui_screen_failed()}
				{#snippet action()}
					<Button variant="secondary" onclick={() => attempt++}>{m.settings_ui_retry()}</Button>
				{/snippet}
			</Banner>
		{:else}
			<Skeleton lines={3} />
		{/if}
	{:else if settings.status !== 'ready'}
		<!-- A generated screen's fields come from the schema: wait for it (the layout offers Retry). -->
		<div aria-busy={settings.status === 'loading'}><Skeleton lines={3} /></div>
	{:else if wantsExtras}
		{#if Extras}<Extras {resolved} />{:else}<Skeleton lines={3} />{/if}
	{:else}
		<GeneratedScreen {resolved} />
	{/if}
{:else if settings.status === 'loading'}
	<div aria-busy="true"><Skeleton lines={3} /></div>
{:else if settings.status === 'ready'}
	<p data-testid="settings-unknown">{m.settings_ui_screen_unknown()}</p>
{/if}
