<script lang="ts">
	// Settings → Security (§10.10.8): change password, sessions, API keys and feed tokens, trusted
	// proxy sign-in and secure cookies. Every risky change asks for the password in one shared
	// dialog (PasswordDialog.svelte, loaded on use). The key manager and the proxy section load right
	// after the first paint (`.size-limit.mjs` gates them apart from it).
	import { onDestroy } from 'svelte';
	import { m } from '$lib/paraglide/messages.js';
	import ChangePassword from './ChangePassword.svelte';
	import Cookies from './Cookies.svelte';
	import Later from './Later.svelte';
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
	<Later
		id="security-keys-heading"
		heading={m.settings_security_keys_heading()}
		load={() => import('./Keys.svelte')}
	/>
	<Later
		id="security-proxy"
		heading={m.settings_security_proxy_heading()}
		load={() => import('./Proxy.svelte')}
	/>
	<Cookies />
</div>
