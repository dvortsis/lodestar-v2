<script lang="ts">
	// The header's one primary action. Send to qBittorrent waits for a download-client endpoint.
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { isMagnet } from '$lib/util/magnet';
	import { detailGeneration, type TorrentDetail } from './cache';

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
</script>

<Button variant="primary" dense disabled={!magnet} data-testid="detail-copy-magnet" onclick={copy}>
	{m.detail_copy_magnet()}
</Button>
