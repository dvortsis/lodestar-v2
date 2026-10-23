// What a refused change says. The server's own sentence is shown as text when it sent one (it says
// what happened in the user's terms); a network failure gets a fixed message.
import type { ApiError } from '$lib/api/client';
import { m } from '$lib/paraglide/messages.js';
import { SHOW_ADULT_ENV } from './adult.svelte';

export function errorText(e: ApiError): string {
	if (e.status === 0) return m.filters_flows_error_network();
	return e.message.trim() || m.filters_flows_error_code({ code: e.code });
}

/** The 18+ setting's refusals: `set_by_environment` names the variable to change. */
export function adultErrorText(e: ApiError): string {
	if (e.code === 'set_by_environment')
		return m.filters_flows_adult_env({ variable: SHOW_ADULT_ENV });
	if (e.code === 'adult_confirmation_required') return m.filters_flows_adult_refused();
	return errorText(e);
}
