<script lang="ts">
	// The body of "View as config.toml" (lazy): this screen's values that aren't at their default as
	// `config.toml` lines and as `LODESTAR_*` lines, each block with Copy. Saved values only, never a
	// secret.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';
	import type { FieldSpec, SettingsForm } from '../form';
	import { configToml, envLines, type ConfigEntry } from './toml';

	let { form, specs }: { form: SettingsForm; specs: FieldSpec[] } = $props();

	const entries = $derived.by((): ConfigEntry[] =>
		specs
			.map((spec) => ({ spec, f: form.field(spec.key) }))
			.filter(({ spec, f }) => !spec.writeOnly && f.source !== 'default')
			.map(({ spec, f }) => ({ config: spec.config, env: spec.env, value: f.saved }))
	);
	const toml = $derived(configToml(entries));
	const env = $derived(envLines(entries));

	async function copy(text: string) {
		try {
			await navigator.clipboard.writeText(text);
			toast.push({ kind: 'success', text: m.settings_ui_config_copied(), timeout: 2000 });
		} catch {
			toast.push({ kind: 'error', text: m.settings_ui_config_copy_failed() });
		}
	}
</script>

<div class="flex flex-col gap-4" data-testid="settings-config">
	{#if !toml && !env}
		<p data-testid="settings-config-none">{m.settings_ui_config_none()}</p>
	{:else}
		{#each [{ id: 'toml', title: m.settings_ui_config_toml(), text: toml }, { id: 'env', title: m.settings_ui_config_env(), text: env }] as block (block.id)}
			{#if block.text}
				<section class="flex flex-col gap-2" aria-labelledby="config-{block.id}">
					<div class="flex items-center justify-between gap-3">
						<h3 id="config-{block.id}" class="text-body font-semibold">{block.title}</h3>
						<Button
							dense
							onclick={() => void copy(block.text)}
							data-testid="settings-config-copy-{block.id}">{m.settings_ui_config_copy()}</Button
						>
					</div>
					<!-- A scrollable block must be reachable by keyboard (axe: scrollable-region-focusable). -->
					<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
					<pre
						class="config-block"
						data-testid="settings-config-{block.id}"
						tabindex="0">{block.text}</pre>
				</section>
			{/if}
		{/each}
		<p class="text-small text-muted-text">{m.settings_ui_config_note()}</p>
	{/if}
</div>

<style>
	.config-block {
		overflow-x: auto;
		padding: calc(var(--spacing) * 3);
		border: var(--border) solid var(--color-base-300);
		border-radius: var(--radius-field);
		background: var(--color-base-200);
		font-family: var(--font-mono);
		font-size: var(--text-small);
	}
</style>
