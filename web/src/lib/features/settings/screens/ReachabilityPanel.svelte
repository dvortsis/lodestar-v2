<script module lang="ts">
	/** How often the answer is asked for again while the screen is open and visible. */
	export const REFRESH_MS = 30_000;
</script>

<script lang="ts">
	// "Reachable from the internet" (§10.10.8, B14): the network check as plain words, the address
	// other nodes see, the evidence behind the answer and Check again. It refreshes every 30 s while
	// the screen is open and visible. Every number is the server's; an unknown one is "—".
	import { onMount } from 'svelte';
	import { ApiError, api, isAbort } from '$lib/api/client';
	import { formatCount, formatSpan, DASH } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import IconOk from '~icons/ph/check-circle';
	import IconBad from '~icons/ph/warning';
	import IconWait from '~icons/ph/spinner-gap';
	import type { components } from '$lib/api/schema';

	type Check = components['schemas']['NetworkCheck'];

	let check = $state.raw<Check>();
	let failed = $state(false);
	let checking = $state(false);
	let wait = $state<{ seconds?: number }>();

	const locale = $derived(currentLocale());
	const outcome = $derived(
		check?.reachable === true ? 'yes' : check?.reachable === false ? 'no' : 'maybe'
	);

	async function refresh(signal?: AbortSignal) {
		try {
			check = await api.get('/network/check', { signal });
			failed = false;
		} catch (e) {
			if (!isAbort(e)) failed = true;
		}
	}

	async function again() {
		checking = true;
		wait = undefined;
		try {
			check = await api.post('/network/check');
			failed = false;
		} catch (e) {
			if (e instanceof ApiError && e.code === 'rate_limited') wait = { seconds: e.retryAfter };
			else failed = true;
		} finally {
			checking = false;
		}
	}

	onMount(() => {
		const ctl = new AbortController();
		void refresh(ctl.signal);
		const timer = setInterval(() => {
			if (document.visibilityState === 'visible') void refresh(ctl.signal);
		}, REFRESH_MS);
		const onVisible = () => {
			if (document.visibilityState === 'visible') void refresh(ctl.signal);
		};
		document.addEventListener('visibilitychange', onVisible);
		return () => {
			ctl.abort();
			clearInterval(timer);
			document.removeEventListener('visibilitychange', onVisible);
		};
	});

	function reason(c: Check): string {
		switch (c.reason) {
			case 'dht_off':
				return m.settings_ui_reach_dht_off();
			case 'bootstrapping':
				return m.settings_ui_reach_bootstrapping();
			case 'measuring':
				return m.settings_ui_reach_measuring({
					time: formatSpan(Math.max(0, c.min_window_s - c.measured_s), locale)
				});
			case 'unsolicited_queries':
				return m.settings_ui_reach_unsolicited_queries();
			case 'few_unsolicited_sources':
				return m.settings_ui_reach_few_sources();
			case 'no_unsolicited_queries':
				return m.settings_ui_reach_none();
			default:
				return '';
		}
	}

	/** `ip:port`, with an IPv6 address in brackets; null while there is no consensus. */
	function address(c: Check): string | null {
		const { ip, port } = c.external;
		if (ip === null || port === null) return null;
		return ip.includes(':') ? `[${ip}]:${port}` : `${ip}:${port}`;
	}
</script>

<section
	class="flex flex-col gap-3 rounded-box border border-base-300 p-4"
	aria-labelledby="reach-title"
	data-testid="settings-reach"
>
	<h3 id="reach-title" class="text-body-lg font-semibold">{m.settings_ui_reach_title()}</h3>
	{#if check}
		<p
			class="flex items-center gap-2 font-medium"
			data-testid="settings-reach-status"
			data-state={outcome}
		>
			{#if outcome === 'yes'}
				<IconOk class="size-(--icon-size-row) shrink-0 text-success" aria-hidden="true" />
				{m.settings_ui_reach_yes()}
			{:else if outcome === 'no'}
				<IconBad class="size-(--icon-size-row) shrink-0 text-error" aria-hidden="true" />
				{m.settings_ui_reach_no()}
			{:else}
				<IconWait class="size-(--icon-size-row) shrink-0" aria-hidden="true" />
				{m.settings_ui_reach_checking()}
			{/if}
		</p>
		<p data-testid="settings-reach-reason">{reason(check)}</p>
		<dl class="reach-facts text-small">
			<dt>{m.settings_ui_reach_address()}</dt>
			<dd data-testid="settings-reach-address">
				{#if address(check)}<Mono>{address(check)}</Mono>{:else}<span
						aria-label={m.settings_ui_reach_unknown()}>{DASH}</span
					>{/if}
			</dd>
			<dt>{m.settings_ui_reach_inbound()}</dt>
			<dd data-testid="settings-reach-inbound">
				{#if check.inbound_queries === null}<span aria-label={m.settings_ui_reach_unknown()}
						>{DASH}</span
					>{:else}{formatCount(check.inbound_queries, locale)}{/if}
			</dd>
			<dt>{m.settings_ui_reach_unsolicited()}</dt>
			<dd data-testid="settings-reach-unsolicited">
				{#if check.unsolicited_queries === null}<span aria-label={m.settings_ui_reach_unknown()}
						>{DASH}</span
					>{:else}{formatCount(check.unsolicited_queries, locale)}{/if}
			</dd>
			<dt>{m.settings_ui_reach_sources()}</dt>
			<dd data-testid="settings-reach-sources">
				{#if check.unsolicited_sources === null}<span aria-label={m.settings_ui_reach_unknown()}
						>{DASH}</span
					>{:else}{formatCount(check.unsolicited_sources, locale)}{/if}
			</dd>
		</dl>
	{:else if failed}
		<p role="alert" data-testid="settings-reach-failed">{m.settings_ui_reach_failed()}</p>
	{:else}
		<p aria-busy="true" class="flex items-center gap-2">
			<IconWait
				class="size-(--icon-size-row) shrink-0"
				aria-hidden="true"
			/>{m.settings_ui_reach_checking()}
		</p>
	{/if}
	<div class="flex flex-wrap items-center gap-3">
		<Button loading={checking} onclick={() => void again()} data-testid="settings-reach-again"
			>{m.settings_ui_reach_again()}</Button
		>
		{#if wait}
			<p role="status" class="text-small" data-testid="settings-reach-wait">
				{wait.seconds === undefined
					? m.settings_ui_reach_wait()
					: m.settings_ui_reach_wait_for({ time: formatSpan(wait.seconds, locale) })}
			</p>
		{/if}
	</div>
</section>

<style>
	.reach-facts {
		display: grid;
		grid-template-columns: auto 1fr;
		gap: calc(var(--spacing) * 1) calc(var(--spacing) * 4);
	}
</style>
