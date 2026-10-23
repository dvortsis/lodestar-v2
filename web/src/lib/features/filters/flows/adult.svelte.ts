// "Show adult content" (§20.9, B17): a preference, not a password. Turning it on asks for the 18+
// confirmation first and sends `confirm_adult: true` with it; turning it off asks nothing.
import { api, ApiError } from '$lib/api/client';
import type { components } from '$lib/api/schema';

export interface AdultConfirm {
	/** Opens the 18+ dialog; resolves true when the user confirms, false on Cancel or Esc. */
	ask(): Promise<boolean>;
	/** For `AdultConfirmDialog`: whether it's open. */
	readonly open: boolean;
	/** For `AdultConfirmDialog`: the user's answer. */
	answer(ok: boolean): void;
}

export function createAdultConfirm(): AdultConfirm {
	let pending = $state.raw<((ok: boolean) => void) | null>(null);
	return {
		ask() {
			// A second ask while one is open answers the first with "no".
			pending?.(false);
			return new Promise<boolean>((resolve) => (pending = resolve));
		},
		get open() {
			return pending !== null;
		},
		answer(ok) {
			const resolve = pending;
			pending = null;
			resolve?.(ok);
		}
	};
}

/** The variable that sets `content.show_adult` from outside the app (B17). */
export const SHOW_ADULT_ENV = 'LODESTAR_SHOW_ADULT';

export type ShowAdultResult = { ok: true } | { ok: false; error?: ApiError };

export async function setShowAdult(on: boolean, adult: AdultConfirm): Promise<ShowAdultResult> {
	if (on && !(await adult.ask())) return { ok: false };
	// `SettingsPatch` is `Record<string, never>` in schema.d.ts: the settings DTO doesn't describe its
	// fields yet (backend follow-up: type each section's patch in the OpenAPI document).
	const body = (on
		? { show_adult: true, confirm_adult: true }
		: { show_adult: false }) as unknown as components['schemas']['SettingsPatch'];
	try {
		await api.patch('/settings/{section}', { path: { section: 'content' }, body });
		return { ok: true };
	} catch (e) {
		const error =
			e instanceof ApiError ? e : new ApiError(0, 'network', e instanceof Error ? e.message : '');
		if (error.code === 'adult_confirmation_required' && import.meta.env.DEV) {
			console.error('setShowAdult: the server wanted the 18+ confirmation that was sent');
		}
		return { ok: false, error };
	}
}
