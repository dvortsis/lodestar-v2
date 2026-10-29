<script lang="ts">
	// Runs the load check, then either redirects or shows the page's form.
	import { onMount, type Snippet } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { checkAuth, redirectFor } from './gate';
	import { toLogin, toNext, toSetup } from './nav';

	interface Props {
		page: 'login' | 'setup';
		/** Where a signed-in visitor of the sign-in page goes. */
		next?: string;
		children: Snippet;
	}

	let { page, next = '/', children }: Props = $props();
	let ready = $state(false);

	onMount(() => {
		let live = true;
		void checkAuth().then(async (status) => {
			if (!live) return;
			const to = redirectFor(page, status, next);
			if (to === '/setup') await toSetup();
			else if (to === '/login') await toLogin();
			else if (to) await toNext(to);
			else ready = true;
		});
		return () => (live = false);
	});
</script>

<!-- Reserve the actual form's size while checking; no estimated heights that
     break with translated text. Hidden/inert controls cannot receive input. -->
<div class="relative">
	<div class:invisible={!ready} inert={!ready}>
		{@render children()}
	</div>
	{#if !ready}
		<p role="status" class="absolute top-0 left-0 text-body text-muted-text">
			{m.auth_checking()}
		</p>
	{/if}
</div>
