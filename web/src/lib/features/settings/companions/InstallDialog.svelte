<script lang="ts">
	// The install confirmation: everything the server's offer says, then Install. Nothing is fetched
	// until the button is pressed, and the request carries only the offer's version and SHA-256.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Dialog from '$lib/ui/overlays/Dialog.svelte';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { bytesText, type InstallOffer } from './logic';

	interface Props {
		offer: InstallOffer;
		busy: boolean;
		error: string;
		onconfirm: () => void;
		onclose: () => void;
	}
	let { offer, busy, error, onconfirm, onclose }: Props = $props();
	let open = $state(true);
	const size = $derived(bytesText(offer.size_bytes, currentLocale()));
	const rows = $derived([
		[m.settings_companions_offer_program(), offer.name],
		[
			m.settings_companions_offer_version(),
			offer.replaces
				? m.settings_companions_offer_update({ from: offer.replaces, to: offer.version })
				: offer.version
		],
		[m.settings_companions_offer_licence(), offer.licence],
		[m.settings_companions_offer_size(), size],
		[m.settings_companions_offer_runs(), m.settings_companions_offer_runs_value()]
	]);
</script>

<Dialog
	bind:open
	title={offer.replaces
		? m.settings_companions_update_title({ name: offer.name })
		: m.settings_companions_install_title({ name: offer.name })}
	closeLabel={m.settings_companions_close()}
	onOpenChange={(o) => {
		if (!o) onclose();
	}}
	initialFocus={() => document.querySelector<HTMLElement>('[data-install-cancel]')}
>
	<div class="flex flex-col gap-4">
		<p class="text-small">{m.settings_companions_offer_intro()}</p>
		<dl class="grid grid-cols-1 gap-2 text-small sm:grid-cols-2">
			{#each rows as [k, v], i (k)}
				<dt class="text-muted-text">{k}</dt>
				<dd class="break-words" data-offer-field={i} data-testid={`offer-field-${i}`}>{v}</dd>
			{/each}
			<dt class="text-muted-text">{m.settings_companions_offer_sha()}</dt>
			<dd class="break-all font-mono text-caption" data-offer-sha>{offer.sha256}</dd>
		</dl>
		{#if error}<Banner kind="error">{error}</Banner>{/if}
	</div>
	{#snippet footer()}
		<Button data-install-cancel onclick={() => (open = false)}
			>{m.settings_companions_cancel()}</Button
		>
		<Button variant="primary" loading={busy} onclick={onconfirm}>
			{offer.replaces ? m.settings_companions_update() : m.settings_companions_install()}
		</Button>
	{/snippet}
</Dialog>
