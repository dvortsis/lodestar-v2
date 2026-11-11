<script lang="ts">
	// "Needs X · Set up" for a source that waits for a companion (§18.10, CMP-10's state on the
	// Sources API). Set up opens Settings → Companions' own install dialog in place (`SetupFlow`,
	// loaded when it's pressed): the server's offer is shown, nothing is fetched until the user
	// confirms, and the request carries only that offer's version and SHA-256. The server decides;
	// this builds no request of its own.
	import { tick } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { companionName, type CompanionNeed } from './logic';

	interface Props {
		need: CompanionNeed;
		/** The install succeeded: the screen reads the source again, so no stale "Needs X" stays. */
		oninstalled?: () => void | Promise<void>;
		/** Where focus goes once the install is done and the source is read again (the chip is gone). */
		focusAfter?: () => HTMLElement | undefined;
	}
	let { need, oninstalled, focusAfter }: Props = $props();

	const name = $derived(companionName(need.companion));
	let started = $state(false);
	let loading = $state(false);
	let done = $state(false);
	/** A new press starts the flow afresh, so a refused offer can be tried again. */
	let attempt = $state(0);

	function setUp() {
		if (loading) return;
		started = loading = true;
		attempt++;
	}
</script>

<div class="flex flex-col gap-2 text-small" data-testid="companion-need">
	<div class="flex flex-wrap items-center gap-2">
		{#if need.state === 'missing'}
			<Badge variant="warning"
				><span data-testid="companion-need-text">{m.companions_need_missing({ name })}</span></Badge
			>
			<span aria-hidden="true">·</span>
			<Button dense {loading} onclick={setUp} data-testid="companion-need-setup">
				{m.companions_need_setup()}<span class="sr-only"> {name}</span>
			</Button>
		{:else}
			<Badge variant="warning"
				><span data-testid="companion-need-text">{m.companions_need_unhealthy({ name })}</span
				></Badge
			>
			<span class="text-muted-text">{m.companions_need_unhealthy_hint()}</span>
		{/if}
	</div>
	{#if started}
		{#key attempt}
			{#await import('./SetupFlow.svelte') then { default: SetupFlow }}
				<SetupFlow
					companion={need.companion}
					onsettled={() => (loading = false)}
					onclose={() => (started = false)}
					oninstalled={async () => {
						// The source is read again and this chip goes: take what's needed before it does.
						const refresh = oninstalled;
						const target = focusAfter;
						done = true;
						await refresh?.();
						await tick();
						target?.()?.focus();
					}}
				/>
			{/await}
		{/key}
	{/if}
	{#if done}<p class="sr-only" role="status">{m.companions_need_installed({ name })}</p>{/if}
</div>
