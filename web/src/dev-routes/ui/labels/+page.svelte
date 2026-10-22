<script lang="ts">
	import Badge from '$lib/ui/labels/Badge.svelte';
	import Chip from '$lib/ui/labels/Chip.svelte';
	import Kbd from '$lib/ui/labels/Kbd.svelte';
	import Mono from '$lib/ui/labels/Mono.svelte';
	import RelativeTime from '$lib/ui/labels/RelativeTime.svelte';

	let chips = $state(['video', 'audio', 'archive']);
	const now = Math.floor(Date.now() / 1000);
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-5 bg-base-200 p-6 text-base-content"
>
	<h1 class="text-display font-semibold">Labels</h1>

	<section class="flex flex-wrap items-center gap-2" data-testid="badges">
		<Badge>Neutral</Badge>
		<Badge variant="accent">Accent</Badge>
		<Badge variant="info">Info</Badge>
		<Badge variant="success">Verified</Badge>
		<Badge variant="warning">Suspect seeders</Badge>
		<Badge variant="error">Failed</Badge>
	</section>

	<section class="flex flex-wrap items-center gap-2" data-testid="chips">
		{#each chips as chip (chip)}
			<Chip
				removeLabel="Remove filter: {chip}"
				onremove={() => (chips = chips.filter((c) => c !== chip))}>{chip}</Chip
			>
		{/each}
		<Chip selected>Selected</Chip>
		<Chip>Read-only</Chip>
	</section>

	<p class="text-body">
		Press <Kbd>/</Kbd> to search, <Kbd>j</Kbd> / <Kbd>k</Kbd> to move.
	</p>
	<p class="text-body"><Mono wrap>3b245504cf5f11bbdbe1201cea6a6bf45aee1bc0</Mono></p>
	<p class="text-body">
		Last seen <RelativeTime seconds={now - 3 * 3600} />, discovered
		<RelativeTime seconds={now - 5} />, never <RelativeTime seconds={null} />.
	</p>
</div>
