// Everything after a save finds the session ended (X-session-draft-keep, UI review L2), on demand:
// the form's leave guard loads this only then, so no first view carries it. It keeps the draft
// for the sign-in, asks, and re-arms the shell's redirect if the user stays; or, for an instance
// that needs its first-run setup again, discards what was staged and goes on.
import { m } from '$lib/paraglide/messages.js';
import { formatCount } from '$lib/format';
import { currentLocale } from '$lib/i18n/locale.svelte';
import { KEPT_KEY, REARM_EVENT, SIGN_IN_KEY, storage } from './draft-scope';
import { hasDraft, neverDrafted } from './draft-store';
import { loadEdit, type Engine, type SettingsForm } from './form.svelte';

/** Marks `id`'s draft as kept for the sign-in that returns to `path`. False when refused. */
export function markKept(id: string, path: string, at: number): boolean {
	try {
		const store = storage();
		if (!store) return false;
		store.setItem(KEPT_KEY, JSON.stringify({ id, path, at }));
		return true;
	} catch {
		return false;
	}
}

/** Removes the kept mark (the user stayed on the page instead of signing in). */
export function dropKept(): void {
	try {
		storage()?.removeItem(KEPT_KEY);
	} catch {
		// blocked
	}
}

/** Writes the draft now, over any still on offer (what's on the page is newer). */
export function keepDraft(form: SettingsForm): boolean {
	const e = form as Engine;
	if (!e.useDrafts || e.draftUnavailable || e.status === 'loading') return false;
	e.draft = null;
	e.draftData = null;
	e.writeNow();
	return hasDraft(e.id) && markKept(e.id, location.pathname, Date.now());
}

const notice = () => {
	try {
		return storage()?.getItem(SIGN_IN_KEY) === 'notice';
	} catch {
		return false;
	}
};

/**
 * The leave guard's `signin` or `setup` decision (`leave.ts`). `navigate` continues past the
 * guard; `fallback` is the ordinary leave question, used if the dialog's code can't load.
 */
export async function onSessionEnd(
	kind: 'signin' | 'setup',
	form: SettingsForm,
	to: URL,
	signal: AbortSignal,
	navigate: (href: string) => Promise<void>,
	fallback: () => void
): Promise<void> {
	if (kind === 'setup') {
		// Signed out, or a re-claimed instance: nothing staged is kept. The shell has dropped the
		// drafts and blocks new ones (`draftsBlocked`); the staged values go with the page.
		await navigate(to.href);
		return;
	}
	let ask: Awaited<ReturnType<typeof loadEdit>>['ask'];
	try {
		ask = (await loadEdit()).ask;
	} catch {
		fallback();
		return;
	}
	const isNotice = notice();
	const count = formatCount(form.dirtyCount, currentLocale());
	// Kept before asking, so the question says exactly what survives the sign-in.
	const kept = keepDraft(form);
	const secrets = form.specs.some((spec) => neverDrafted(spec) && form.field(spec.key).dirty);
	const body = [
		kept ? m.settings_form_signed_out_body({ count }) : m.settings_form_signed_out_lost({ count })
	];
	if (kept && secrets) body.push(m.settings_form_secrets_not_kept());
	const answer = await ask(
		{
			title: isNotice ? m.settings_form_notice_title() : m.settings_form_signed_out_title(),
			body,
			actions: [
				{ id: 'keep', label: m.settings_form_keep_editing() },
				{
					id: 'signin',
					label: isNotice ? m.settings_form_continue() : m.settings_form_sign_in(),
					variant: 'primary'
				}
			],
			initial: 'signin'
		},
		signal
	);
	if (answer !== 'signin') {
		// Staying: the mark goes (a later visit offers the draft as usual), and the shell
		// redirects again on the next save that finds the session ended.
		dropKept();
		globalThis.dispatchEvent?.(new Event(REARM_EVENT));
		return;
	}
	await navigate(to.href);
}
