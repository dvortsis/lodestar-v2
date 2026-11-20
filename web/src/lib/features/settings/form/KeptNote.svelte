<script lang="ts">
	// After a sign-in that kept the draft (X-session-draft-keep): one line, the changes are back
	// but not saved yet. Loaded with `kept-restore.ts`; `DraftBanner` shows it.
	import Banner from '$lib/ui/feedback/Banner.svelte';
	import { m } from '$lib/paraglide/messages.js';
</script>

<Banner kind="info">
	<span data-testid="draft-kept">{m.settings_form_kept_body()}</span>
</Banner>
