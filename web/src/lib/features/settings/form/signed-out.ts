// The question when a save finds the session ended (X-session-draft-keep, UI review L2). Its own
// on-demand chunk, loaded only then, so neither a first view nor the edit chunk carries it. The
// dialog comes from the edit chunk (`ask`), which the form loads alongside.
import { m } from '$lib/paraglide/messages.js';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import type { Question } from './dialogs';
import { KEPT_KEY, storage } from './draft-scope';
import { neverDrafted } from './draft-store';
import type { SettingsForm } from './form.svelte';

type Ask = (q: Question, signal?: AbortSignal) => Promise<string | null>;

/**
 * The session ended (or the Legal Notice is due) with unsaved changes, and the app is going to
 * sign in. Keeps the draft first, so the question says exactly what survives: Sign in goes on,
 * and the sign-in returns here and restores it; Keep editing (or Esc) stays, and the next save
 * that finds the session ended asks again.
 */
export async function askSignedOut(
	ask: Ask,
	form: SettingsForm,
	to: URL,
	reason: 'expired' | 'notice',
	signal: AbortSignal,
	navigate: (href: string) => Promise<void>,
	stay: () => void
): Promise<void> {
	const count = formatCount(form.dirtyCount, currentLocale());
	const kept = form.keepForSignIn() && markKept(form.id, location.pathname, Date.now());
	const secrets = form.specs.some((spec) => neverDrafted(spec) && form.field(spec.key).dirty);
	const body = [
		kept ? m.settings_form_signed_out_body({ count }) : m.settings_form_signed_out_lost({ count })
	];
	if (kept && secrets) body.push(m.settings_form_secrets_not_kept());
	const answer = await ask(
		{
			title:
				reason === 'notice' ? m.settings_form_notice_title() : m.settings_form_signed_out_title(),
			body,
			actions: [
				{ id: 'keep', label: m.settings_form_keep_editing() },
				{
					id: 'signin',
					label: reason === 'notice' ? m.settings_form_continue() : m.settings_form_sign_in(),
					variant: 'primary'
				}
			],
			initial: 'signin'
		},
		signal
	);
	if (answer !== 'signin') {
		// Staying: the marker goes, so a later visit offers the draft as usual instead.
		dropKept();
		stay();
		return;
	}
	await navigate(to.href);
}

/** Marks `id`'s draft as kept for the sign-in that returns to `path`. False when refused. */
export function markKept(id: string, path: string, at: number): boolean {
	const store = storage();
	if (!store) return false;
	try {
		store.setItem(KEPT_KEY, JSON.stringify({ id, path, at }));
		return true;
	} catch {
		return false;
	}
}

/** Removes the kept marker (the user stayed on the page instead of signing in). */
export function dropKept(): void {
	try {
		storage()?.removeItem(KEPT_KEY);
	} catch {
		// blocked
	}
}
