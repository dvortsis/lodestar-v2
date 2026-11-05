<script lang="ts">
	// Settings → About → Legal (owner §2.38, docs/decisions/2026-11-04-X-legal-docs.md): the Legal
	// Notice this build shows, with its version, loaded when the section is opened. It's the same
	// notice setup showed, from GET /legal/notice, rendered node by node (never as HTML).
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';
	import NoticeBox from '../../auth/NoticeBox.svelte';
	import type { LegalNotice } from '../../auth/notice';

	let notice = $state<LegalNotice | null>(null);
</script>

{#if notice}
	<p class="text-muted-text" data-testid="about-notice-version">
		{m.settings_about_legal_version({ version: formatCount(notice.version, currentLocale()) })}
	</p>
{/if}
<!-- On About the notice flows with the page instead of scrolling in a box. -->
<NoticeBox bind:notice scroll={false} level={3} />
