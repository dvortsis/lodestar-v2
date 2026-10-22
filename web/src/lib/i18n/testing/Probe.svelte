<script lang="ts">
	import { m } from '$lib/paraglide/messages.js';

	let { relative }: { relative: string } = $props();
</script>

<p data-testid="probe-language">{m.common_language_name()}</p>
<p data-testid="probe-age">{m.scrape_age({ relative })}</p>
