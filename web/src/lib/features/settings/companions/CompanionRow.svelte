<script lang="ts">
	// One companion: health, version, update notice, why it can't be set up here (in words), and
	// Set up / Update / Remove behind their confirmations. The server decides every one: a
	// refusal shows as a sentence and changes nothing.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Badge from '$lib/ui/labels/Badge.svelte';
	import { failureWords, hasRoute, isHealth, prowlarrFamily, wordsFor } from './logic';
	import type { Companion, InstallOffer, RemovalPlan } from './logic';
	import type { CompanionsStore } from './store.svelte';

	let { store, c }: { store: CompanionsStore; c: Companion } = $props();

	let panel = $state<'logs' | 'credentials' | 'template' | null>(null);
	let offer = $state<InstallOffer | null>(null);
	let plan = $state<RemovalPlan | null>(null);
	let busy = $state<string | null>(null);
	let error = $state('');
	let dialogError = $state('');
	let note = $state('');

	const health = $derived(isHealth(c.health) ? c.health : null);
	const installed = $derived(c.installed !== null);
	const pasteOffered = $derived(!installed && c.refusal === 'guided_only');
	const hasCredentials = $derived(c.credentials.length > 0 || pasteOffered);
	/** Clears `what` after the exit animation, unless a newer one has replaced it by then. */
	const later = (clear: () => void, what: unknown) =>
		setTimeout(() => {
			if (what === offer || what === plan) clear();
		}, 400);
	const toggle = (p: typeof panel) => (panel = panel === p ? null : p);

	async function openOffer() {
		busy = 'offer';
		error = note = dialogError = '';
		try {
			offer = await store.offer(c.id);
		} catch (e) {
			error = failureWords(e);
		} finally {
			busy = null;
		}
	}
	async function confirmInstall() {
		if (!offer) return;
		busy = 'install';
		dialogError = '';
		try {
			await store.install(offer);
			offer = null;
		} catch (e) {
			dialogError = failureWords(e);
		} finally {
			busy = null;
		}
	}
	async function openRemoval() {
		busy = 'plan';
		error = note = dialogError = '';
		try {
			plan = await store.removalPlan(c.id);
		} catch (e) {
			error = failureWords(e);
		} finally {
			busy = null;
		}
	}
	async function confirmRemove() {
		busy = 'remove';
		dialogError = '';
		try {
			await store.remove(c.id);
			plan = null;
			panel = null;
		} catch (e) {
			dialogError = failureWords(e);
		} finally {
			busy = null;
		}
	}
	async function restart() {
		busy = 'restart';
		error = note = '';
		try {
			await store.restart(c.id);
			note = m.settings_companions_restarting();
		} catch (e) {
			error = failureWords(e);
		} finally {
			busy = null;
		}
	}
</script>

<li class="flex flex-col gap-3 border-b border-field-border py-4" data-companion={c.id}>
	<div class="flex flex-wrap items-center gap-2">
		<h3 class="text-small font-semibold">{c.name}</h3>
		{#if health === 'healthy'}
			<Badge variant="success">{m.settings_companions_health_healthy()}</Badge>
		{:else if health === 'unhealthy'}
			<Badge variant="error">{m.settings_companions_health_unhealthy()}</Badge>
		{:else if health === 'unknown'}
			<Badge>{m.settings_companions_health_unknown()}</Badge>
		{:else}
			<Badge>{m.settings_companions_health_none()}</Badge>
		{/if}
		{#if c.opt_in}<Badge>{m.settings_companions_opt_in()}</Badge>{/if}
	</div>
	<p class="text-small text-muted-text">
		{installed
			? m.settings_companions_installed({ version: c.installed ?? '' })
			: m.settings_companions_not_installed()}
		· {m.settings_companions_licence({ licence: c.licence })}
	</p>
	{#if c.available}
		<p class="text-small" data-update-notice>
			{installed
				? m.settings_companions_update_available({ version: c.available })
				: m.settings_companions_install_available({ version: c.available })}
		</p>
	{/if}
	{#if c.refusal}
		<p class="text-small text-muted-text" data-refusal>{wordsFor(c.refusal)}</p>
	{/if}

	{#if hasRoute(c.id)}
		<p class="text-small text-muted-text" data-route-note>{m.settings_companions_route_note()}</p>
	{/if}
	{#if prowlarrFamily(c.id)}
		<Banner kind="warning" title={m.settings_companions_socks_title()}>
			<ul class="list-disc ps-4" data-socks-warnings>
				<li>{m.settings_companions_socks_email()}</li>
				<li>{m.settings_companions_socks_flaresolverr()}</li>
				<li>{m.settings_companions_socks_indexer_proxies()}</li>
			</ul>
		</Banner>
	{/if}

	<div class="flex flex-wrap gap-2">
		{#if c.available}
			<Button variant="primary" dense loading={busy === 'offer'} onclick={openOffer}>
				{installed ? m.settings_companions_update() : m.settings_companions_setup()}
			</Button>
		{/if}
		{#if installed}
			<Button dense loading={busy === 'restart'} onclick={restart}
				>{m.settings_companions_restart()}</Button
			>
			<Button dense aria-expanded={panel === 'logs'} onclick={() => toggle('logs')}
				>{m.settings_companions_logs()}</Button
			>
			<Button dense variant="danger" loading={busy === 'plan'} onclick={openRemoval}
				>{m.settings_companions_remove()}</Button
			>
		{/if}
		{#if hasCredentials}
			<Button dense aria-expanded={panel === 'credentials'} onclick={() => toggle('credentials')}>
				{m.settings_companions_credentials()}
			</Button>
		{/if}
		{#if c.templates.length > 0}
			<Button dense aria-expanded={panel === 'template'} onclick={() => toggle('template')}>
				{m.settings_companions_templates()}
			</Button>
		{/if}
	</div>
	{#if error}<Banner kind="error">{error}</Banner>{/if}
	{#if note}<Banner kind="info">{note}</Banner>{/if}

	{#if panel === 'logs'}{#await import('./Logs.svelte') then { default: Logs }}<Logs
				{store}
				id={c.id}
			/>{/await}{/if}
	{#if panel === 'credentials'}
		{#await import('./Credentials.svelte') then { default: Credentials }}
			<Credentials {store} id={c.id} fields={c.credentials} {pasteOffered} />
		{/await}
	{/if}
	{#if panel === 'template'}{#await import('./Templates.svelte') then { default: Templates }}<Templates
				{store}
				id={c.id}
				kinds={c.templates}
			/>{/await}{/if}
</li>

<!-- The dialogs load on first use (their own budget line). A closed one stays mounted for its
     exit animation, then goes (as `ask()` does). -->
{#key offer}
	{#if offer}
		{#await import('./InstallDialog.svelte') then { default: InstallDialog }}
			<InstallDialog
				{offer}
				busy={busy === 'install'}
				error={dialogError}
				onconfirm={confirmInstall}
				onclose={() => later(() => (offer = null), offer)}
			/>
		{/await}
	{/if}
{/key}
{#key plan}
	{#if plan}
		{#await import('./RemoveDialog.svelte') then { default: RemoveDialog }}
			<RemoveDialog
				name={c.name}
				{plan}
				busy={busy === 'remove'}
				error={dialogError}
				onconfirm={confirmRemove}
				onclose={() => later(() => (plan = null), plan)}
			/>
		{/await}
	{/if}
{/key}
