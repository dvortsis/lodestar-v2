<script lang="ts">
	// Step 3: is the DHT port reachable from outside (§10.10.10, §18.7)? Live from
	// `GET /network/check`, read until the server has an answer; Retest restarts the measurement.
	import { onDestroy, onMount } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { formatCount } from '$lib/format';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { createNetworkCheck } from './network.svelte';

	const net = createNetworkCheck();
	onMount(() => net.start());
	onDestroy(() => net.destroy());

	const check = $derived(net.check);
	const off = $derived(check?.reason === 'dht_off');
	// One small polite region: it changes only when the verdict does, not on each progress tick.
	const verdict = $derived(
		!check || off
			? ''
			: check.reachable === true
				? m.wizard_net_reachable()
				: check.reachable === false
					? m.wizard_net_unreachable()
					: ''
	);
	const reasons = {
		dht_off: () => m.wizard_net_reason_dht_off(),
		bootstrapping: () => m.wizard_net_reason_bootstrapping(),
		measuring: () => m.wizard_net_reason_measuring(),
		unsolicited_queries: () => m.wizard_net_reason_ok(),
		few_unsolicited_sources: () => m.wizard_net_reason_few(),
		no_unsolicited_queries: () => m.wizard_net_reason_none()
	};
</script>

<p class="text-body">{m.wizard_net_lead()}</p>
<div class="sr-only" role="status" aria-live="polite">{verdict}</div>

<div class="flex flex-col gap-3" data-testid="wizard-net">
	<div>
		<Button onclick={() => net.retestNow()} loading={net.retest === 'busy'}>
			{net.retest === 'busy' ? m.wizard_net_retesting() : m.wizard_net_retest()}
		</Button>
	</div>
	{#if net.failed && !check}
		<Banner kind="error">{m.wizard_net_error()}</Banner>
	{:else if !check}
		<Banner kind="info" title={m.wizard_net_loading()}>&nbsp;</Banner>
		<p class="text-body" aria-hidden="true">&nbsp;</p>
	{:else}
		{#if off}
			<Banner kind="info" title={m.wizard_net_off()}>{reasons[check.reason]()}</Banner>
		{:else if check.reachable === true}
			<Banner kind="success" title={m.wizard_net_reachable()}>{reasons[check.reason]()}</Banner>
		{:else if check.reachable === false}
			<Banner kind="warning" title={m.wizard_net_unreachable()}>{reasons[check.reason]()}</Banner>
		{:else}
			<Banner kind="info" title={m.wizard_net_measuring()}>
				{reasons[check.reason]()}
				<span data-testid="wizard-net-progress"
					>{m.wizard_net_progress({
						seconds: formatCount(check.measured_s),
						total: formatCount(check.min_window_s)
					})}</span
				>
			</Banner>
		{/if}
		{#if check.external.ip && check.external.port !== null}
			<p class="text-body text-muted-text" data-testid="wizard-net-address">
				{m.wizard_net_address({ ip: check.external.ip, port: String(check.external.port) })}
			</p>
		{/if}
		{#if net.failed}<Banner kind="warning">{m.wizard_net_error()}</Banner>{/if}
		{#if net.stopped}<p class="text-muted-text">{m.wizard_net_stopped()}</p>{/if}
	{/if}
	{#if net.retest === 'limited'}
		<Banner kind="info">
			<span data-testid="wizard-net-limited"
				>{net.retryAfter === undefined
					? m.wizard_net_limited()
					: m.wizard_net_limited_in({ seconds: formatCount(net.retryAfter) })}</span
			>
		</Banner>
	{:else if net.retest === 'error'}
		<Banner kind="error">{m.wizard_net_retest_error()}</Banner>
	{/if}
</div>

{#if check && check.reachable !== true && !off}
	<section class="flex flex-col gap-2">
		<h3 class="text-body-lg font-semibold">{m.wizard_net_help_title()}</h3>
		<ul class="list-disc ps-4 text-body">
			<li>{m.wizard_net_help_forward()}</li>
			<li>{m.wizard_net_help_vpn()}</li>
			<li>{m.wizard_net_help_still()}</li>
		</ul>
	</section>
{/if}
