<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import PageFrame from '$lib/ui/layout/PageFrame.svelte';
	import PageHeader from '$lib/ui/layout/PageHeader.svelte';
	import { m } from '$lib/paraglide/messages.js';
	let { kind }: { kind: 'settings' | 'stage' } = $props();
</script>

<PageFrame size="manuscript">
	<div class="contents" data-testid="unavailable-page">
		<PageHeader title={kind === 'settings' ? m.shell_nav_settings() : m.shell_stage_title()} />
		<p>{kind === 'settings' ? m.shell_settings_unavailable() : m.shell_stage_unavailable()}</p>
		<a
			class="ls-focus text-primary underline"
			href={resolve(('/insights' + (kind === 'stage' ? page.url.search : '')) as '/insights')}
			>{m.shell_return_insights()}</a
		>
		<a class="ls-focus text-primary underline" href={resolve('/search')}>{m.shell_nav_search()}</a>
	</div>
</PageFrame>
