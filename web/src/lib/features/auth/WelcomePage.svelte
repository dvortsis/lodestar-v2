<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import WizardPage from '../wizard/WizardPage.svelte';
	import { checkAuth } from './gate';
	import { currentSession } from './session.svelte';
	import { toLoginFrom, toSetup } from './nav';

	let ready = $state(false);
	let failed = $state(false);
	let container = $state<HTMLElement>();
	let live = false;
	const signedIn = $derived(ready && currentSession() === 'signed_in');

	function isIdle() {
		const active = document.activeElement;
		return !active || active === document.body || active.id === 'main';
	}

	async function check() {
		failed = false;
		ready = false;
		const status = await checkAuth();
		if (!live) return;
		if (!status) {
			failed = true;
			await tick();
			if (live && isIdle()) container?.querySelector<HTMLElement>('button')?.focus();
			return;
		}
		if (status.setup_required) return toSetup();
		if (!status.logged_in) return toLoginFrom('/welcome');
		ready = true;
		await tick();
		if (!live) return;
		// Async auth must not steal focus from a control the visitor already chose.
		if (isIdle()) {
			const heading = container?.querySelector('h1');
			heading?.setAttribute('tabindex', '-1');
			heading?.focus();
		}
	}

	onMount(() => {
		live = true;
		void check();
		return () => {
			live = false;
		};
	});
</script>

<div bind:this={container} class="welcome">
	{#if signedIn}
		<WizardPage />
	{:else}
		<div class="mx-auto flex max-w-xl flex-col gap-4 px-4 py-8">
			<h1 class="w-fit max-w-full text-display font-semibold text-base-content">
				{m.auth_checking()}
			</h1>
			{#if failed}
				<Banner kind="error">{m.auth_error_network()}</Banner>
				<Button onclick={() => void check()}>{m.auth_welcome_retry()}</Button>
			{/if}
		</div>
	{/if}
</div>

<style>
	/* The heading takes focus on load; the ring hugs its text, like `.ins-heading`. */
	.welcome :global(h1:focus-visible) {
		outline: var(--focus-ring-width) solid var(--color-primary);
		outline-offset: var(--focus-ring-offset);
	}
</style>
