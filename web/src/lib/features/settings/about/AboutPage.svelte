<script lang="ts">
	// Settings → About on v0.5.1: only its Legal section, which shows the Legal Notice this build
	// carries (owner decision §2.38). The rest of About arrives with the Settings screens.
	import { resolve } from '$app/paths';
	import NoticeBox from '$lib/features/auth/NoticeBox.svelte';
	import { m } from '$lib/paraglide/messages.js';
</script>

<div class="flex max-w-(--manuscript-width) flex-col gap-6 p-6" data-testid="about-page">
	<h1 class="text-title font-semibold">{m.shell_more_about()}</h1>
	<section aria-labelledby="about-legal" class="flex flex-col gap-3">
		<h2 id="about-legal" class="text-body font-semibold">{m.settings_about_legal()}</h2>
		<p class="text-body text-muted-text">{m.settings_about_legal_lead()}</p>
		<NoticeBox scroll={false} />
	</section>
	<a class="ls-focus text-primary underline" href={resolve('/search')}>{m.shell_nav_search()}</a>
</div>
