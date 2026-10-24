<script lang="ts">
	// The fallback when no copy method worked (typically a plain-HTTP LAN address with a locked-down
	// browser): the magnet in a selected read-only field, ready for Ctrl+C.
	import { tick } from 'svelte';
	import Button from '$lib/ui/controls/Button.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import { manual } from './manual.svelte';
	import './detail.css';

	let field = $state<HTMLInputElement>();
	const mac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform);

	$effect(() => {
		if (manual.magnet === null) return;
		void tick().then(() => {
			field?.focus();
			field?.select();
		});
	});
</script>

{#if manual.magnet !== null}
	<div class="ls-detail-manual ls-detail-magnet" role="group" aria-label={m.detail_magnet_label()}>
		<p>{m.detail_magnet_manual()} {mac ? m.detail_press_copy_mac() : m.detail_press_copy()}</p>
		<input
			bind:this={field}
			readonly
			value={manual.magnet}
			aria-label={m.detail_magnet_label()}
			data-testid="detail-magnet-field"
			class="input input-sm w-full border-field-border font-mono text-small"
		/>
		<div>
			<Button dense onclick={() => (manual.magnet = null)}>{m.common_dismiss()}</Button>
		</div>
	</div>
{/if}
