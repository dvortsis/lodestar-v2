<script lang="ts">
	// The full-page "Restarting…" state (§10.10.6): polls until the server is back, then reloads, or
	// moves to the address a change names. After two minutes it says what to check. It covers the
	// page, so nothing underneath can be used while the server is away.
	import { formatSpan } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import IconWait from '~icons/ph/spinner-gap';
	import IconWarn from '~icons/ph/warning';
	import { untrack } from 'svelte';
	import { flow, stopWaiting } from './flow.svelte';
	import { pollUntil, restartedHere, upAt } from './restart';
	import { acceptUrl, networkUrl } from './urls';

	/** What moves the page; tests replace them (jsdom can't navigate). */
	interface Props {
		reload?: () => void;
		assign?: (url: string) => void;
	}
	let { reload = () => location.reload(), assign = (url) => location.assign(url) }: Props =
		$props();

	const locale = $derived(currentLocale());
	const target = $derived(flow.to);
	const followable = $derived(target !== undefined && acceptUrl(target) !== null);
	let attempt = $state(0);
	let manual = $state(false);

	// Polls on mount, and again whenever Try again bumps `attempt`.
	$effect(() => {
		void attempt;
		const ctl = new AbortController();
		const to = untrack(() => flow.to);
		manual = false;
		flow.phase = 'waiting';
		if (to !== undefined && !acceptUrl(to)) {
			// A scheme or address that isn't followed is shown, not opened.
			manual = true;
			return;
		}
		const check = to !== undefined ? upAt(to) : restartedHere(untrack(() => flow.before));
		void pollUntil(check, { signal: ctl.signal }).then((outcome) => {
			if (outcome === 'aborted') return;
			if (outcome === 'timeout') {
				flow.phase = 'timeout';
				return;
			}
			if (to !== undefined) assign(networkUrl(to)!.href);
			else reload();
		});
		return () => ctl.abort();
	});
</script>

<div class="restarting" role="alertdialog" aria-modal="true" aria-labelledby="restarting-title">
	<div class="restarting-panel" data-testid="settings-restarting" data-phase={flow.phase}>
		{#if flow.phase === 'timeout'}
			<IconWarn class="restarting-icon" aria-hidden="true" />
			<h2 id="restarting-title" class="text-title font-semibold">
				{m.settings_ui_restarting_timeout_title()}
			</h2>
			<p>{m.settings_ui_restarting_timeout_body()}</p>
			<Button
				variant="primary"
				onclick={() => {
					flow.phase = 'waiting';
					attempt++;
				}}>{m.settings_ui_restarting_again()}</Button
			>
		{:else if manual}
			<h2 id="restarting-title" class="text-title font-semibold">
				{m.settings_ui_moving_title()}
			</h2>
			<p>{m.settings_ui_moving_manual()}</p>
			<p><Mono>{target}</Mono></p>
			<p class="text-small text-muted-text">{m.settings_ui_moving_open()}</p>
			<Button onclick={stopWaiting}>{m.settings_form_dialog_close()}</Button>
		{:else}
			<IconWait class="restarting-icon restarting-spin" aria-hidden="true" />
			<h2 id="restarting-title" class="text-title font-semibold">
				{followable ? m.settings_ui_moving_title() : m.settings_ui_restarting_title()}
			</h2>
			<p aria-live="polite">
				{#if followable}
					{m.settings_ui_moving_body({ url: target ?? '' })}
				{:else}
					{m.settings_ui_restarting_body({ time: formatSpan(flow.estimate, locale) })}
				{/if}
			</p>
		{/if}
	</div>
</div>

<style>
	.restarting {
		position: fixed;
		inset: 0;
		z-index: 60;
		display: flex;
		align-items: center;
		justify-content: center;
		padding: calc(var(--spacing) * 4);
		background: var(--color-base-100);
	}
	.restarting-panel {
		display: flex;
		flex-direction: column;
		align-items: flex-start;
		gap: calc(var(--spacing) * 3);
		max-width: var(--manuscript-width);
	}
	:global(.restarting-icon) {
		width: var(--icon-size-nav);
		height: var(--icon-size-nav);
	}
	@media (prefers-reduced-motion: no-preference) {
		:global(.restarting-spin) {
			animation: restarting-spin 1.2s linear infinite;
		}
	}
	@keyframes restarting-spin {
		to {
			transform: rotate(360deg);
		}
	}
</style>
