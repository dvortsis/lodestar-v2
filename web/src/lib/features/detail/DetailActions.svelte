<script lang="ts">
	// The header's one primary action. Send to qBittorrent waits for a download-client endpoint.
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { isMagnet } from '$lib/util/magnet';
	import { copyMagnet } from './magnet';
	import type { TorrentDetail } from './cache';

	interface Props {
		detail: TorrentDetail | null;
	}
	let { detail }: Props = $props();
	const magnet = $derived(detail && isMagnet(detail.magnet) ? detail.magnet : null);
</script>

<Button
	variant="primary"
	dense
	disabled={!magnet}
	data-testid="detail-copy-magnet"
	onclick={() => magnet && copyMagnet({ magnet })}
>
	{m.detail_copy_magnet()}
</Button>
