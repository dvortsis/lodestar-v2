<script lang="ts">
	import AlertDetailPage from '$lib/features/alerts/AlertDetailPage.svelte';

	let { data }: { data: { id: number } } = $props();
</script>

<AlertDetailPage id={data.id} />
