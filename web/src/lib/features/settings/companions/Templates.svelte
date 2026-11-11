<script lang="ts">
	// A compose service or an Unraid template for running a companion as its own container. The
	// server builds and checks it; the text is shown as text and copied on request.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Field from '$lib/ui/fields/Field.svelte';
	import Input from '$lib/ui/fields/Input.svelte';
	import { copyText } from '$lib/util/clipboard';
	import { failureWords, type Template } from './logic';
	import type { CompanionsStore } from './store.svelte';

	interface Props {
		store: CompanionsStore;
		id: string;
		kinds: ('compose' | 'unraid')[];
	}
	let { store, id, kinds }: Props = $props();
	let result = $state<Template | null>(null);
	let error = $state('');
	let copied = $state('');
	let busy = $state<string | null>(null);
	// The route is the server's `route`; `direct` is its default and leaves outside any VPN.
	let route = $state<'direct' | 'gluetun' | 'existing_gluetun'>('direct');
	let gluetun = $state('');
	let configDir = $state('');
	let downloads = $state('');

	async function build(kind: 'compose' | 'unraid') {
		busy = kind;
		error = '';
		copied = '';
		try {
			result = await store.template(id, {
				kind,
				route,
				...(route === 'existing_gluetun' && gluetun ? { gluetun_container: gluetun } : {}),
				...(configDir ? { config_dir: configDir } : {}),
				...(downloads ? { downloads } : {})
			});
		} catch (e) {
			result = null;
			error = failureWords(e);
		} finally {
			busy = null;
		}
	}
	async function copy(text: string, key: string) {
		copied = (await copyText(text)) === 'copied' ? key : '';
	}
	const blocks = $derived(
		result
			? [
					...(result.compose
						? [{ key: 'compose', name: 'compose.yaml', text: result.compose }]
						: []),
					...result.unraid.map((u) => ({ key: u.file_name, name: u.file_name, text: u.xml }))
				]
			: []
	);
</script>

<div class="flex flex-col gap-3" data-templates={id}>
	<p class="text-small text-muted-text">{m.settings_companions_template_help()}</p>
	<div class="flex flex-col gap-3">
		<Field label={m.settings_companions_template_route()}>
			{#snippet children(props)}
				<select {...props} bind:value={route} class="select w-full border-field-border">
					<option value="direct">{m.settings_companions_template_route_direct()}</option>
					<option value="gluetun">{m.settings_companions_template_route_gluetun()}</option>
					<option value="existing_gluetun">{m.settings_companions_template_route_existing()}</option
					>
				</select>
			{/snippet}
		</Field>
		{#if route === 'direct'}
			<div data-direct-warning>
				<Banner kind="warning">{m.settings_companions_template_direct_warning()}</Banner>
			</div>
		{/if}
		{#if route === 'existing_gluetun'}
			<Field label={m.settings_companions_template_gluetun_name()}>
				{#snippet children(props)}
					<Input {...props} bind:value={gluetun} spellcheck="false" />
				{/snippet}
			</Field>
		{/if}
		<Field
			label={m.settings_companions_template_config_dir()}
			help={m.settings_companions_template_folder_help()}
		>
			{#snippet children(props)}
				<Input {...props} bind:value={configDir} mono spellcheck="false" />
			{/snippet}
		</Field>
		<Field
			label={m.settings_companions_template_downloads()}
			help={m.settings_companions_template_folder_help()}
		>
			{#snippet children(props)}
				<Input {...props} bind:value={downloads} mono spellcheck="false" />
			{/snippet}
		</Field>
	</div>
	<div class="flex flex-wrap gap-2">
		{#each kinds as k (k)}
			<Button dense loading={busy === k} onclick={() => build(k)}>
				{k === 'compose'
					? m.settings_companions_template_compose()
					: m.settings_companions_template_unraid()}
			</Button>
		{/each}
	</div>
	{#if error}<Banner kind="error">{error}</Banner>{/if}
	{#if result?.unraid.length}
		{#each result.unraid as u (u.file_name)}
			<div class="flex flex-col gap-1 text-small" data-unraid-fields>
				<p>{m.settings_companions_template_unraid_path({ path: u.add_container_path })}</p>
				<dl class="grid grid-cols-1 gap-1 sm:grid-cols-2">
					{#each u.fields as f (f.label)}
						<dt class="text-muted-text">{f.label}</dt>
						<dd class="break-all font-mono text-caption">{f.value}</dd>
					{/each}
				</dl>
			</div>
		{/each}
	{/if}
	{#each blocks as b (b.key)}
		<div class="flex flex-col gap-2">
			<div class="flex flex-wrap items-center gap-2">
				<span class="font-mono text-small">{b.name}</span>
				<Button dense onclick={() => copy(b.text, b.key)}>
					{copied === b.key
						? m.settings_companions_template_copied()
						: m.settings_companions_template_copy()}
				</Button>
			</div>
			<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
			<pre
				class="max-h-(--notice-max-height) overflow-auto whitespace-pre-wrap break-all rounded-selector border border-field-border p-2 font-mono text-caption"
				tabindex="0"
				aria-label={b.name}>{b.text}</pre>
		</div>
	{/each}
</div>
