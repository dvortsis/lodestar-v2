<script lang="ts">
	// "Keep this address?" (§10.10.6, B2c): after signing in at the address a change moved Lodestar
	// to, the old address comes back by itself unless someone confirms. The countdown ticks once a
	// second in a fixed-width slot and is hidden from screen readers (the banner is announced once,
	// with a sentence that doesn't change).
	import { ApiError } from '$lib/api/client';
	import { formatSpan } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import { keepAddress, revertAddress } from './restart-flow';
	import type { AddressChange, SystemInfo } from './system';

	interface Props {
		system: SystemInfo;
		change: AddressChange;
		/** Refetches `/system` (after Keep, or when the change is gone). */
		refresh: () => void | Promise<void>;
	}
	let { system, change, refresh }: Props = $props();

	let now = $state(Date.now());
	$effect(() => {
		const timer = setInterval(() => (now = Date.now()), 1000);
		return () => clearInterval(timer);
	});
	const left = $derived(
		change.deadline === null ? null : Math.max(0, change.deadline - now / 1000)
	);
	let busy = $state<'keep' | 'back'>();

	async function act(kind: 'keep' | 'back') {
		busy = kind;
		try {
			if (kind === 'keep') await keepAddress();
			else await revertAddress(system);
			await refresh();
		} catch (e) {
			if (e instanceof ApiError && e.code === 'no_address_change') await refresh();
			else toast.push({ kind: 'error', text: m.settings_ui_address_failed() });
		} finally {
			busy = undefined;
		}
	}
</script>

<div data-testid="settings-address-banner">
	<Banner kind="info" title={m.settings_ui_address_title()}>
		<span class="sr-only">{m.settings_ui_address_body_sr({ previous: change.previous_url })}</span>
		<span aria-hidden="true" data-testid="settings-address-text">
			{m.settings_ui_address_body({
				previous: change.previous_url,
				time: left === null ? '—' : formatSpan(Math.ceil(left), currentLocale())
			})}
		</span>
		{#if !change.stored}
			<br /><span data-testid="settings-address-unstored"
				>{m.settings_ui_address_unprotected()}</span
			>
		{/if}
		{#snippet action()}
			<div class="flex flex-wrap items-center gap-2">
				<Button
					dense
					variant="primary"
					loading={busy === 'keep'}
					onclick={() => void act('keep')}
					data-testid="settings-address-keep">{m.settings_ui_address_keep()}</Button
				>
				<Button
					dense
					loading={busy === 'back'}
					onclick={() => void act('back')}
					data-testid="settings-address-back">{m.settings_ui_address_back()}</Button
				>
			</div>
		{/snippet}
	</Banner>
</div>
