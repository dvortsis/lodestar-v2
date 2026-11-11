<script lang="ts">
	// Settings → Companions (§10.10.8, §18.10): the companions the catalogue offers, each with its
	// health, version and the things you can do. It shows only what the server says; a failed or
	// refused request leaves the last list on screen with a sentence in plain words.
	import { onDestroy, onMount } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import CompanionRow from './CompanionRow.svelte';
	import { failureWords, type Detected } from './logic';
	import { CompanionsStore } from './store.svelte';

	const store = new CompanionsStore();
	onMount(() => store.start());
	onDestroy(() => store.stop());

	async function retry() {
		store.loading = true;
		await store.load();
	}
	let busy = $state(false);
	let error = $state('');
	let url = $state('');
	let target = $state('');

	async function detect() {
		busy = true;
		error = '';
		try {
			await store.detect();
		} catch (e) {
			error = failureWords(e);
		} finally {
			busy = false;
		}
	}
	async function addFound(d: Detected) {
		busy = true;
		error = '';
		try {
			await store.addAt(d.id, d.target);
		} catch (x) {
			error = failureWords(x);
		} finally {
			busy = false;
		}
	}
	async function add(e: Event) {
		e.preventDefault();
		if (!target || !url) {
			error = m.settings_companions_add_choose_first();
			return;
		}
		busy = true;
		error = '';
		try {
			await store.addAt(target, { kind: 'url', url });
			url = '';
		} catch (x) {
			error = failureWords(x);
		} finally {
			busy = false;
		}
	}
</script>

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
			{#if store.detected}
				{#if store.detected.length === 0}
					<p class="text-small text-muted-text">{m.settings_companions_detect_none()}</p>
				{:else}
					<ul class="text-small" data-detected>
						{#each store.detected as d (d.id)}
							<li class="flex flex-wrap items-center gap-2">
								<span>
									{m.settings_companions_detect_found({
										id:
											store.items.find((c) => c.id === d.id)?.name ??
											m.settings_companions_unknown_name(),
										version: d.version ?? ''
									})}
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
		{#if error}<Banner kind="error">{error}</Banner>{/if}
	{/if}
</section>
