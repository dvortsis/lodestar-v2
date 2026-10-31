<script lang="ts">
	// Settings → Security (§10.10.8): change password, sessions and secure cookies. Every risky
	// change asks for the password in one shared dialog (PasswordDialog.svelte, loaded on use).
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import ChangePassword from './ChangePassword.svelte';
	import Cookies from './Cookies.svelte';
	import Sessions from './Sessions.svelte';
	import { dismissPasswordDialogs } from './confirm';

	// A confirmation left open (Back, a sign-out elsewhere) goes with the screen.
	onDestroy(dismissPasswordDialogs);
</script>

<h1 class="text-title font-semibold">{m.settings_security_title()}</h1>
<p class="text-muted-text">{m.settings_security_purpose()}</p>
<div class="flex flex-col gap-8">
	<ChangePassword />
	<Sessions />
	<Cookies />
</div>
