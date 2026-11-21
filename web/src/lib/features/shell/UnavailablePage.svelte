<script lang="ts">
	import { PAGE_BODY_MANUSCRIPT, PAGE_FRAME, PAGE_H1 } from '$lib/ui/layout/classes';
	import { resolve } from '$app/paths';
	import { page } from '$app/state';
	import { m } from '$lib/paraglide/messages.js';
	let { kind }: { kind: 'settings' | 'stage' } = $props();
</script>

<div class={PAGE_FRAME} data-page-frame="">
	<div class={PAGE_BODY_MANUSCRIPT}>
		<div class="contents" data-testid="unavailable-page">
			<h1 class={PAGE_H1} tabindex="-1">
				{kind === 'settings' ? m.shell_nav_settings() : m.shell_stage_title()}
			</h1>
			<p>{kind === 'settings' ? m.shell_settings_unavailable() : m.shell_stage_unavailable()}</p>
			<a
				class="ls-focus text-primary underline"
				href={resolve(('/insights' + (kind === 'stage' ? page.url.search : '')) as '/insights')}
				>{m.shell_return_insights()}</a
			>
			<a class="ls-focus text-primary underline" href={resolve('/search')}>{m.shell_nav_search()}</a
			>
		</div>
	</div>
</div>
