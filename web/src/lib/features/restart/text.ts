import { m } from '$lib/paraglide/messages.js';
import type { Pending } from './logic';

/**
 * "3 changes apply after a restart". The count is a small whole number, so it goes in as is, not
 * through `$lib/format`.
 */
export const countText = (pending: Pending | null): string =>
	pending
		? (pending.fields.length === 1 ? m.restart_banner_one : m.restart_banner)({
				count: pending.fields.length
			})
		: '';
