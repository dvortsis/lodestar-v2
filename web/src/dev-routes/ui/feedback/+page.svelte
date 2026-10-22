<script lang="ts">
	import Button from '$lib/ui/controls/Button.svelte';
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import Skeleton from '$lib/ui/feedback/Skeleton.svelte';
	import EmptyState from '$lib/ui/feedback/EmptyState.svelte';
	import Toaster from '$lib/ui/feedback/Toaster.svelte';
	import { toast } from '$lib/ui/feedback/toasts.svelte';

	const kinds = ['info', 'success', 'warning', 'error'] as const;
</script>

<div
	class="mx-auto flex max-w-(--manuscript-width) flex-col gap-5 bg-base-200 p-6 text-base-content"
>
	<h1 class="text-display font-semibold">Feedback</h1>

	{#each kinds as kind (kind)}
		<Banner {kind} title="{kind} banner">
			Something worth knowing.
			{#snippet action()}<Button dense variant="quiet">Details</Button>{/snippet}
		</Banner>
	{/each}

	<section class="flex flex-wrap gap-2">
		{#each kinds as kind (kind)}
			<Button dense onclick={() => toast.push({ kind, text: `A ${kind} toast` })}>
				Toast {kind}
			</Button>
		{/each}
	</section>

	<section class="flex flex-col gap-3">
		<Skeleton lines={3} />
		<Skeleton class="h-10 w-1/2" />
	</section>

	<EmptyState title="No saved searches" text="Save a search to be told when new results match.">
		{#snippet action()}<Button variant="primary">Save search</Button>{/snippet}
	</EmptyState>
</div>

<Toaster />
