<script lang="ts">
	// Sessions (§10.10.8): this browser, and Sign out other sessions behind the password
	// confirmation (§10.10.7). `POST /sessions/revoke-others` rotates the session key and sets a new
	// cookie for this browser, so it stays signed in.
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';
	import { confirmWithPassword } from './confirm';

	let busy = $state(false);
	let done = $state('');

	async function revoke() {
		busy = true;
		done = '';
		try {
			const ok = await confirmWithPassword({
				title: m.settings_security_sessions_dialog_title(),
				body: [m.settings_security_sessions_dialog_body()],
				action: m.settings_security_sessions_dialog_action(),
				send: (password) => api.post('/sessions/revoke-others', { body: { password } })
			});
			if (ok) done = m.settings_security_sessions_done();
		} finally {
			busy = false;
		}
	}
</script>

<section class="flex flex-col gap-2" aria-labelledby="security-sessions">
	<h2 id="security-sessions" class="text-label font-semibold">
		{m.settings_security_sessions_heading()}
	</h2>
	<p>{m.settings_security_sessions_current()}</p>
	<p class="text-muted-text">{m.settings_security_sessions_help()}</p>
	<div class="flex flex-wrap items-center gap-3">
		<Button loading={busy} onclick={revoke} data-testid="security-revoke-others"
			>{m.settings_security_sessions_revoke()}</Button
		>
		<p role="status" class="text-small text-success" data-testid="security-sessions-result">
			{done}
		</p>
	</div>
</section>
