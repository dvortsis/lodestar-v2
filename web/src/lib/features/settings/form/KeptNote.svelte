<script lang="ts">
	// After a sign-in that kept the draft (X-session-draft-keep): one line, the changes are back
	// but not saved yet, and how many no longer apply. Loaded with `kept-restore.ts`; `DraftBanner`
	// shows it.
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { formatCount } from '$lib/format';
	import { currentLocale } from '$lib/i18n/locale.svelte';
	import { m } from '$lib/paraglide/messages.js';

	let { dropped = 0 }: { dropped?: number } = $props();
</script>

<Banner kind="info">
	<span data-testid="draft-kept"
		>{m.settings_form_kept_body()}{#if dropped}
			{(dropped === 1 ? m.settings_form_kept_dropped_one : m.settings_form_kept_dropped_many)({
				count: formatCount(dropped, currentLocale())
			})}{/if}</span
	>
</Banner>
