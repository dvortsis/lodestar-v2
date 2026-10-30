<script lang="ts">
	import { page } from '$app/state';
	import SettingsSection from '$lib/features/settings/shell/SettingsSection.svelte';
</script>

<SettingsSection section={page.params.section ?? ''} />
