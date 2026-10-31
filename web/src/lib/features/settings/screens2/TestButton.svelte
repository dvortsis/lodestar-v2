<script lang="ts" module>
	import type { components } from '$lib/api/schema';
	export type TestBody = components['schemas']['SettingsTest'];
</script>

<script lang="ts">
	// A test button with its result beside it (§10.10.8): `POST /settings/test` with the values on
	// screen, saving nothing. The result is plain text: what we make from `ok` and `version`, or
	// the server's own sentence. Never markup, never a URL or a credential.
	import { ApiError, api } from '$lib/api/client';
	import { m } from '$lib/paraglide/messages.js';
	import Button from '$lib/ui/controls/Button.svelte';

	interface Props {
		label: string;
		/** The body from the values on screen, or null while one the test needs is missing. */
		build: () => TestBody | null;
		/** What a success says; `version` is the server's, when it has one. */
		success: (version: string | null) => string;
		/** Says what's missing when `build` returns null. */
		missing: string;
		/** Only the test button is named by its card, so two cards' buttons differ. */
		'aria-label'?: string;
		disabled?: boolean;
	}
	let { label, build, success, missing, disabled = false, ...rest }: Props = $props();

	let busy = $state(false);
	let result = $state<{ ok: boolean; text: string }>();
	let run = 0;

	async function test() {
		const body = build();
		if (!body) {
			result = { ok: false, text: missing };
			return;
		}
		const mine = ++run;
		busy = true;
		result = undefined;
		let next: { ok: boolean; text: string };
		try {
			const r = await api.post('/settings/test', { body });
			next = r.ok
				? { ok: true, text: success(r.version ?? null) }
				: { ok: false, text: r.error?.message ?? m.settings_screen_test_failed() };
		} catch (e) {
			next = {
				ok: false,
				text: e instanceof ApiError && e.message ? e.message : m.settings_screen_test_unreachable()
			};
		}
		if (mine !== run) return;
		busy = false;
		result = next;
	}
</script>

<div class="flex flex-wrap items-center gap-3">
	<Button onclick={test} {disabled} loading={busy} aria-label={rest['aria-label']}>{label}</Button>
	<!-- Always in the page, so a result is announced when it arrives. -->
	<p
		role="status"
		class="text-small {result?.ok ? 'text-success' : 'text-error'}"
		data-testid="test-result"
	>
		{result?.text ?? ''}
	</p>
</div>
