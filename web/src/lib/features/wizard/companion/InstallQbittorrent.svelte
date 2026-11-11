<script lang="ts">
	// "Install qBittorrent for me" on the download-client step (§10.10.10, §18.10). Offered only
	// when the server says this build can install it; the confirmation shows the server's own
	// offer, and nothing is fetched until the user confirms it.
	import { onDestroy, onMount, tick } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { formatBytes } from '$lib/format';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { createInstall, type Failure } from './install.svelte';

	interface Props {
		/** The client was installed and added to Integrations: reload the step's field. */
		oninstalled: () => void;
	}
	let { oninstalled }: Props = $props();

	const install = createInstall(() => oninstalled());
	/** True while the install runs: the step must not move on and lose its result. */
	export const busy = () => install.phase === 'installing';
	let root = $state<HTMLElement>();
	let last = install.phase;
	// Each click replaces the button that had focus: move focus to what replaced it (never on load).
	$effect(() => {
		const now = install.phase;
		if (now === last) return;
		const was = last;
		last = now;
		if (was === 'checking') return;
		void tick().then(() => root?.querySelector<HTMLElement>('[data-cmp-focus]')?.focus());
	});
	onMount(() => void install.check());
	onDestroy(() => install.destroy());

	const reasons: Record<Failure, () => string> = {
		native_in_container: () => m.wizard_cmp_err_container(),
		unsupported_platform: () => m.wizard_cmp_err_platform(),
		awaiting_pin: () => m.wizard_cmp_err_pin(),
		no_build: () => m.wizard_cmp_err_build(),
		guided_only: () => m.wizard_cmp_err_guided(),
		already_current: () => m.wizard_cmp_err_current(),
		confirmation_mismatch: () => m.wizard_cmp_err_mismatch(),
		not_wired: () => m.wizard_cmp_err_not_wired(),
		busy: () => m.wizard_cmp_err_busy(),
		companions_unavailable: () => m.wizard_cmp_err_off(),
		other: () => m.wizard_cmp_err_other()
	};
	// Only a refusal that a fresh offer could change is worth retrying.
	const retryable = $derived(
		install.failure === 'busy' ||
			install.failure === 'confirmation_mismatch' ||
			install.failure === 'other'
	);
	const offer = $derived(install.offer);
</script>

{#if install.phase !== 'hidden' && install.phase !== 'checking'}
	<section
		class="flex flex-col gap-3 rounded-lg border border-field-border p-4"
		data-testid="wizard-cmp"
		aria-labelledby="wizard-cmp-title"
		bind:this={root}
	>
		<h3 id="wizard-cmp-title" class="text-body-lg font-semibold">{m.wizard_cmp_title()}</h3>

		{#if install.phase === 'ready' || install.phase === 'asking'}
			<p class="text-body">{m.wizard_cmp_lead()}</p>
			<div>
				<Button onclick={() => void install.ask()} loading={install.phase === 'asking'}
					>{m.wizard_cmp_install()}</Button
				>
			</div>
		{:else if install.phase === 'confirm' && offer}
			<p class="text-body" tabindex="-1" data-cmp-focus>{m.wizard_cmp_confirm_lead()}</p>
			<dl class="flex flex-col gap-2 text-body" data-testid="wizard-cmp-offer">
				<div class="flex flex-col">
					<dt class="text-muted-text">{m.wizard_cmp_program()}</dt>
					<dd>{offer.name}</dd>
				</div>
				<div class="flex flex-col">
					<dt class="text-muted-text">{m.wizard_cmp_version()}</dt>
					<dd>
						{offer.version}{#if offer.replaces}
							({m.wizard_cmp_replaces({ version: offer.replaces })}){/if}
					</dd>
				</div>
				<div class="flex flex-col">
					<dt class="text-muted-text">{m.wizard_cmp_licence()}</dt>
					<dd>{offer.licence}</dd>
				</div>
				<div class="flex flex-col">
					<dt class="text-muted-text">{m.wizard_cmp_size()}</dt>
					<dd>{formatBytes(offer.size_bytes)}</dd>
				</div>
				<div class="flex flex-col">
					<dt class="text-muted-text">{m.wizard_cmp_sha()}</dt>
					<dd class="break-all font-mono">{offer.sha256}</dd>
				</div>
				<div class="flex flex-col">
					<dt class="text-muted-text">{m.wizard_cmp_where()}</dt>
					<dd>{m.wizard_cmp_where_value()}</dd>
				</div>
			</dl>
			<div class="flex flex-wrap gap-2">
				<Button variant="primary" onclick={() => void install.confirm()}
					>{m.wizard_cmp_confirm()}</Button
				>
				<Button variant="quiet" onclick={() => install.decline()}>{m.wizard_cmp_decline()}</Button>
			</div>
		{:else if install.phase === 'installing'}
			<Banner kind="info" title={m.wizard_cmp_installing()}>{m.wizard_cmp_installing_note()}</Banner
			>
			<div>
				<Button loading>{m.wizard_cmp_installing()}</Button>
			</div>
		{:else if install.phase === 'done'}
			<div tabindex="-1" data-cmp-focus>
				<Banner kind="success" title={m.wizard_cmp_done()}>{m.wizard_cmp_done_note()}</Banner>
			</div>
		{:else if install.phase === 'failed' && install.failure}
			<div tabindex="-1" data-cmp-focus>
				<Banner kind="error" title={m.wizard_cmp_failed()}>{reasons[install.failure]()}</Banner>
			</div>
			{#if retryable}
				<div>
					<Button onclick={() => void install.ask()}>{m.wizard_cmp_retry()}</Button>
				</div>
			{/if}
		{/if}
	</section>
{/if}
