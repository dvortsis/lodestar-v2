<script lang="ts">
	// Secure cookies, read-only (§10.10.8): `network.secure_cookies` can be set only in the file or
	// the environment, since turning it on over plain HTTP would lock everyone out.
	import { api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import IconOn from '~icons/ph/shield-check';
	import IconOff from '~icons/ph/shield';

	let state = $state<{ on: boolean; env: string | null } | 'failed'>();

	$effect(() => {
		let live = true;
		api.get('/settings/{section}', { path: { section: 'network' } }).then(
			(r) => {
				const f = r.fields.secure_cookies;
				if (live) state = f ? { on: f.value === true, env: f.env } : 'failed';
			},
			() => {
				if (live) state = 'failed';
			}
		);
		return () => {
			live = false;
		};
	});
</script>

<section class="flex flex-col gap-2" aria-labelledby="security-cookies">
	<h2 id="security-cookies" class="text-label font-semibold">
		{m.settings_security_cookies_heading()}
	</h2>
	<p class="text-muted-text">{m.settings_security_cookies_help()}</p>
	{#if state === 'failed'}
		<p data-testid="security-cookies">{m.settings_security_cookies_unknown()}</p>
	{:else if state}
		{@const Icon = state.on ? IconOn : IconOff}
		<p class="flex items-center gap-2 font-semibold" data-testid="security-cookies">
			<Icon aria-hidden="true" />
			{m.settings_security_cookies_status({
				state: state.on ? m.settings_security_cookies_on() : m.settings_security_cookies_off()
			})}
		</p>
		<p class="text-small text-muted-text">
			{state.env
				? m.settings_security_cookies_env({ env: state.env })
				: m.settings_security_cookies_file()}
		</p>
	{:else}
		<p aria-busy="true" role="status">{m.settings_security_loading()}</p>
	{/if}
</section>
