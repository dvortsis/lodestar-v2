<script lang="ts">
	// Settings → Companions (§10.10.8, §18.10): the companions the catalogue offers, each with its
	// health, version and the things you can do. It shows only what the server says; a failed or
	// refused request leaves the last list on screen with a sentence in plain words.
	import { onDestroy, onMount } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import PageHeader from '$lib/ui/layout/PageHeader.svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import CompanionRow from './CompanionRow.svelte';
	import { failureWords, isStarting, type Detected } from './logic';
	import { CompanionsStore } from './store.svelte';

	const store = new CompanionsStore();
	onMount(() => store.start());
	onDestroy(() => store.stop());

	const nameOf = (id: string) =>
		store.items.find((c) => c.id === id)?.name ?? m.settings_companions_unknown_name();
	async function retry() {
		store.loading = true;
		await store.load();
	}
	let busy = $state(false);
	let error = $state('');
	/** Set when the last request was refused because the companion is still starting: Try again
	 * runs it once more. */
	let again = $state<(() => Promise<void>) | null>(null);
	/** Runs `step`, showing its refusal in words; one refused as still starting can be tried again. */
	async function run(step: () => Promise<void>) {
		busy = true;
		error = '';
		again = null;
		try {
			await step();
		} catch (x) {
			error = failureWords(x);
			if (isStarting(x)) again = () => run(step);
		} finally {
			busy = false;
		}
	}
	let url = $state('');
	let target = $state('');

	const detect = () => run(() => store.detect());
	const addFound = (d: Detected) => run(() => store.addAt(d.id, d.target));
	async function add(e: Event) {
		e.preventDefault();
		if (!target || !url) {
			error = m.settings_companions_add_choose_first();
			return;
		}
		const [id, at] = [target, url];
		await run(async () => {
			await store.addAt(id, { kind: 'url', url: at });
			url = '';
		});
	}
</script>

<PageHeader title={m.settings_companions_title()} />
<section class="flex flex-col gap-4" aria-labelledby="companions-heading">
	<h2 id="companions-heading" class="text-small font-semibold">
		{m.settings_companions_heading()}
	</h2>
	<p class="text-small text-muted-text">{m.settings_companions_intro()}</p>

	{#if store.loading}
		<Skeleton class="h-12 w-full" />
	{:else if store.unavailable}
		<Banner kind="info">{m.settings_companions_off()}</Banner>
	{:else if store.loadFailed && store.items.length === 0}
		<Banner kind="error">
			{m.settings_companions_load_failed()}
			{#snippet action()}<Button dense loading={store.loading} onclick={retry}
					>{m.settings_companions_retry()}</Button
				>{/snippet}
		</Banner>
	{:else}
		{#if store.loadFailed}<Banner kind="warning">{m.settings_companions_stale()}</Banner>{/if}
		<ul class="flex flex-col" aria-label={m.settings_companions_heading()}>
			{#each store.items as c (c.id)}<CompanionRow {store} {c} />{/each}
		</ul>

		<div class="flex flex-col gap-3" data-add>
			<h3 class="text-small font-semibold">{m.settings_companions_add_heading()}</h3>
			<p class="text-small text-muted-text">{m.settings_companions_add_help()}</p>
			<div>
				<Button dense loading={busy} onclick={detect}>{m.settings_companions_detect()}</Button>
			</div>
			{#if store.starting.length > 0}
				<ul class="text-small" data-starting>
					{#each store.starting as id (id)}
						<li>{m.settings_companions_detect_starting({ name: nameOf(id) })}</li>
					{/each}
				</ul>
				<div>
					<Button dense loading={busy} onclick={detect}>{m.settings_companions_retry()}</Button>
				</div>
			{/if}
			{#if store.detected}
				{#if store.detected.length === 0 && store.starting.length === 0}
					<p class="text-small text-muted-text">{m.settings_companions_detect_none()}</p>
				{:else}
					<ul class="text-small" data-detected>
						{#each store.detected as d (d.id)}
							<li class="flex flex-wrap items-center gap-2">
								<span>
									{d.version
										? m.settings_companions_detect_found_version({
												id: nameOf(d.id),
												version: d.version
											})
										: m.settings_companions_detect_found({ id: nameOf(d.id) })}
								</span>
								<Button dense loading={busy} onclick={() => addFound(d)}>
									{m.settings_companions_detect_add({
										name:
											store.items.find((c) => c.id === d.id)?.name ??
											m.settings_companions_unknown_name()
									})}
								</Button>
							</li>
						{/each}
					</ul>
				{/if}
			{/if}
			<form class="flex flex-col gap-2" onsubmit={add}>
				<Field label={m.settings_companions_add_which()}>
					{#snippet children(props)}
						<select {...props} bind:value={target} class="select w-full border-field-border">
							<option value="" disabled>{m.settings_companions_add_choose()}</option>
							{#each store.items as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
						</select>
					{/snippet}
				</Field>
				<Field label={m.settings_companions_add_url()} help={m.settings_companions_add_url_help()}>
					{#snippet children(props)}
						<Input {...props} bind:value={url} type="url" inputmode="url" spellcheck="false" />
					{/snippet}
				</Field>
				<div><Button type="submit" loading={busy}>{m.settings_companions_add()}</Button></div>
			</form>
		</div>
		{#if error && again}
			{@const retryStep = again}
			<Banner kind="info">
				{error}
				{#snippet action()}<Button dense loading={busy} onclick={() => void retryStep()}
						>{m.settings_companions_retry()}</Button
					>{/snippet}
			</Banner>
		{:else if error}<Banner kind="error">{error}</Banner>{/if}
	{/if}
</section>
