<script lang="ts">
	// The header's one primary action. Send to qBittorrent waits for a download-client endpoint.
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { isMagnet } from '$lib/util/magnet';
	import { detailGeneration, onDetailInvalidated, type TorrentDetail } from './cache';

	interface Props {
		detail: TorrentDetail | null;
	}
	let { detail }: Props = $props();
	const magnet = $derived(detail && isMagnet(detail.magnet) ? detail.magnet : null);
	let gone = false;
	onDestroy(() => (gone = true));
	async function copy() {
		const value = magnet;
		const generation = detailGeneration();
		if (!value) return;
		const { copyMagnet } = await import('./magnet');
		if (!gone && generation === detailGeneration() && value === magnet)
			await copyMagnet({ magnet: value });
	}

	// "Block this…" (§20.2) through the flows' one door (S-filters-2c): it only ever adds a rule,
	// previewed first, for the torrent shown when it was pressed (its hash and name), and only
	// while that torrent is still the one shown.
	type Target = { hash: string; hash_v2: string | null; name: string };
	let Block = $state.raw<typeof import('$lib/features/filters/flows-root').BlockThisDialog>();
	let target = $state.raw<Target>();
	let blocking = $state(false);
	// Visibility changed (e.g. "Show adult content" turned off): the dialog goes with what it showed.
	onDestroy(onDetailInvalidated(() => (target = undefined)));
	async function block() {
		if (!detail) return;
		const { hash, hash_v2, name } = detail;
		const generation = detailGeneration();
		const mod = await import('$lib/features/filters/flows-door').then(
			(door) => door.load(),
			() => null
		);
		if (!mod) return; // offline or a stale build: the button stays, a press tries again
		if (gone || generation !== detailGeneration() || hash !== detail?.hash) return;
		Block = mod.BlockThisDialog;
		target = { hash, hash_v2, name };
		blocking = true;
	}
</script>

<Button variant="primary" dense disabled={!magnet} data-testid="detail-copy-magnet" onclick={copy}>
	{m.detail_copy_magnet()}
</Button>
<!-- Shown once there is a torrent, never disabled first: an outline button fading from its
     disabled colours reads as low contrast mid-transition (axe, rose-pine-dark). -->
{#if detail}
	<Button dense data-testid="detail-block" onclick={block}>{m.detail_block()}</Button>
{/if}
{#if Block && target && target.hash === detail?.hash}
	<Block torrent={target} bind:open={blocking} />
{/if}
