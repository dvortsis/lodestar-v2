<script lang="ts">
	// The privacy banners on the Overview (§10.10.7 Privacy, §10.10.12), loaded (through door.ts)
	// only while the overview's `privacy` summary is set: one while the log writes data kinds that
	// identify torrents or people, with the time left and Turn off now (the same request as
	// Settings → Logging's and /logs'), and one while searches are saved, linking to Settings →
	// Insights. Neither can be dismissed except by turning the thing off. Deadlines are on the
	// server's clock, kept as an offset from the response's `updated_at`.
	import { resolve } from '$app/paths';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	// Through rules.ts too, as Settings → Logging reads them, so the two modules stay one chunk.
	import { UNTIL_OFF } from '../../settings/logging/rules';
	import { ahead, timeLeft } from '../../settings/logging/state';
	import type { BannerProps } from './door';

	let { overview, reload }: BannerProps = $props();
	// Mounted only while it's set; null (all off) shows nothing.
	const privacy = $derived(overview.privacy ?? { identifying_until: null, query_logging: false });
	const serverAt = $derived(overview.updated_at);

	/** How often the time left is redrawn. */
	const TICK_MS = 15_000;
	const offset = $derived(serverAt - Date.now() / 1000);
	let clock = $state(Date.now());
	$effect(() => {
		const t = setInterval(() => (clock = Date.now()), TICK_MS);
		return () => clearInterval(t);
	});
	const now = $derived(clock / 1000 + offset);
	const left = $derived(timeLeft(privacy.identifying_until ?? 0, now));
	// Once the time runs out the banner goes; the server is asked again so the summary follows.
	let reloadedFor: number | null = null;
	$effect(() => {
		const until = privacy.identifying_until;
		if (until === null || until === UNTIL_OFF || ahead(until, now) || reloadedFor === until) return;
		reloadedFor = until;
		reload();
	});

	let busy = $state(false);
	let error = $state('');
	async function turnOff() {
		busy = true;
		error = '';
		try {
			const [{ LoggingCatalog }, { turnOffAll }] = await Promise.all([
				import('../../settings/logging/catalog.svelte'),
				import('../../settings/logging/privacy')
			]);
			await turnOffAll({ catalog: new LoggingCatalog() });
		} catch (e) {
			error = m.shell_privacy_failed_off({ message: (e as Error).message });
		} finally {
			busy = false;
			reload();
		}
	}
</script>

{#if left || error || privacy.query_logging}
	<div class="flex flex-col gap-2" data-testid="privacy-banners">
		{#if left}
			<Banner kind="warning" title={m.shell_privacy_identifying_title()}>
				<span data-testid="privacy-identifying">{left}</span>
				{#snippet action()}
					<Button variant="secondary" disabled={busy} onclick={turnOff}
						>{m.shell_privacy_turn_off()}</Button
					>
				{/snippet}
			</Banner>
		{/if}
		{#if error}<p role="alert" class="text-small text-error">{error}</p>{/if}
		{#if privacy.query_logging}
			<Banner kind="warning" title={m.shell_privacy_queries_title()}>
				<span data-testid="privacy-queries">{m.shell_privacy_queries_body()}</span>
				<a class="underline" href={resolve('/settings/[section]', { section: 'insights' })}
					>{m.shell_privacy_queries_manage()}</a
				>
			</Banner>
		{/if}
	</div>
{/if}
