<script lang="ts">
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	let { kind }: { kind: 'settings' | 'stage' } = $props();
</script>

<div class="flex max-w-(--prose-measure) flex-col gap-4 p-6" data-testid="unavailable-page">
	<h1 class="text-title font-semibold">
		{kind === 'settings' ? m.shell_nav_settings() : m.shell_stage_title()}
	</h1>
	<p>{kind === 'settings' ? m.shell_settings_unavailable() : m.shell_stage_unavailable()}</p>
	<a
		class="ls-focus text-primary underline"
		href={resolve(('/insights' + (kind === 'stage' ? page.url.search : '')) as '/insights')}
		>{m.shell_return_insights()}</a
	>
	<a class="ls-focus text-primary underline" href={resolve('/search')}>{m.shell_nav_search()}</a>
</div>
