import { m } from '$lib/paraglide/messages.js';
import { usableUrl } from './flow';
import type { Pending, System } from './logic';

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

/** What the watch's live region says: the address waiting to be kept, else the pending count. */
export const announceText = (pending: Pending | null, system: System | undefined): string =>
	system?.address_change?.state === 'awaiting_confirmation'
		? m.restart_keep_announce({
				url: usableUrl(system.address_change.url, location.hostname)
			})
		: countText(pending);
