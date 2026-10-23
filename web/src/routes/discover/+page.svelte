<script lang="ts">
	import { page } from '$app/state';
	import DiscoverPage from '$lib/features/discover/DiscoverPage.svelte';
</script>

<DiscoverPage search={page.url.searchParams} />
