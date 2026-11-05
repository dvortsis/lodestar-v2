<script lang="ts">
	// The wizard's first step (§10.10.10, owner §2.38): the Legal Notice, rendered from the server's
	// built-in copy, and the required "I have read the Legal Notice" box, unticked by default. It runs
	// in /setup before the session exists, so it saves nothing itself: /setup sends `notice.version`
	// with the password (POST /setup), and the server refuses any other version (409
	// notice_required), which brings this step back with the new text.
	import { tick } from 'svelte';
	import IconWarning from '~icons/ph/warning-circle';
	import Checkbox from '$lib/ui/fields/Checkbox.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import NoticeBox from '../auth/NoticeBox.svelte';
	import type { LegalNotice } from '../auth/notice';

	interface Props {
		/** The notice shown; `null` until it loads, and again to load a newer one. */
		notice?: LegalNotice | null;
		/** The box. */
		read?: boolean;
		/** Why Continue refused: not ticked, or the notice changed. */
		error?: 'unchecked' | 'changed';
		/** Move focus to the notice once it has loaded, so a keyboard user can scroll it. */
		focusOnLoad?: boolean;
		onchange?: () => void;
	}

	let {
		notice = $bindable(null),
		read = $bindable(false),
		error,
		focusOnLoad = false,
		onchange
	}: Props = $props();

	let root = $state<HTMLDivElement>();
	let focused = false;
	$effect(() => {
		if (!notice || !focusOnLoad || focused) return;
		focused = true;
		void tick().then(() =>
			root?.querySelector<HTMLElement>('[data-testid="legal-notice"]')?.focus()
		);
	});

	const message = $derived(
		error === 'unchecked'
			? m.wizard_legal_unchecked()
			: error === 'changed'
				? m.wizard_legal_changed()
				: undefined
	);
</script>

<div bind:this={root} class="flex flex-col gap-4" data-testid="legal-step">
	<p class="text-body text-muted-text">{m.wizard_legal_intro()}</p>
	<NoticeBox bind:notice level={3} />
	<!-- Always drawn, so the notice arriving moves nothing; it can't be ticked before then. -->
	<div class="flex flex-col gap-1">
		<Checkbox
			bind:checked={read}
			name="notice"
			label={m.wizard_legal_confirm()}
			required
			disabled={!notice}
			aria-invalid={message ? true : undefined}
			aria-describedby={message ? 'notice-error' : undefined}
			onchange={() => onchange?.()}
		/>
		{#if message}
			<p
				id="notice-error"
				class="flex items-start gap-1 text-small text-base-content"
				data-testid="field-error"
			>
				<IconWarning class="mt-0.5 size-4 shrink-0 text-error" aria-hidden="true" />
				<span>{message}</span>
			</p>
		{/if}
	</div>
</div>
