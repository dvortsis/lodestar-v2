<script lang="ts">
	// The sign-in gate (owner decision §2.38): signed in, but this instance hasn't accepted the
	// current Legal Notice (set up before it existed, or its version was raised). Every other
	// request answers 409 notice_required until Accept records it; then on to `next`.
	import { onDestroy, tick } from 'svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { createAuthForm } from './form.svelte';
	import FormAlert from './FormAlert.svelte';
	import NoticeBox from './NoticeBox.svelte';
	import { acceptNotice, resetNoticeCache, type LegalNotice } from './notice';
	import { toNext } from './nav';

	interface Props {
		next: string;
		/** Move focus to the notice once it has loaded: after sign-in replaced the form. */
		focusOnLoad?: boolean;
	}

	let { next, focusOnLoad = false }: Props = $props();

	const form = createAuthForm();
	onDestroy(form.destroy);
	let notice = $state<LegalNotice | null>(null);
	let root = $state<HTMLFormElement>();
	let focused = false;
	$effect(() => {
		if (!notice || !focusOnLoad || focused) return;
		focused = true;
		void tick().then(() =>
			root?.querySelector<HTMLElement>('[data-testid="legal-notice"]')?.focus()
		);
	});

	async function onsubmit(event: SubmitEvent) {
		event.preventDefault();
		if (!notice || form.busy) return;
		const version = notice.version;
		if (await form.submit(() => acceptNotice(version))) {
			await toNext(next);
		} else if (form.state.name === 'error' && form.state.failure.kind === 'notice_required') {
			// A newer build is serving: show its notice instead.
			resetNoticeCache();
			notice = null;
		}
	}
</script>

<form
	bind:this={root}
	method="post"
	class="flex flex-col gap-4"
	novalidate
	{onsubmit}
	data-testid="notice-gate"
>
	<NoticeBox bind:notice />
	<FormAlert {form} />
	{#if form.state.name === 'error' && form.state.failure.kind === 'notice_required'}
		<p class="text-small text-base-content">{m.auth_notice_gate_changed()}</p>
	{/if}
	<Button
		type="submit"
		variant="primary"
		loading={form.busy}
		aria-disabled={!notice || form.busy || undefined}
	>
		{m.auth_notice_accept()}
	</Button>
</form>
