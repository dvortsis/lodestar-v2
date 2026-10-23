// Where the shell reads the event-stream status from. The stream client (`status.svelte.ts`) loads
// after first paint so it stays out of the first-load budget; until it has, the pill shows
// "Connecting" and the connection is taken to be fine.
import type { Component } from 'svelte';
import type { createShellStatus } from './status.svelte';
import type { PillState } from './status';

export const INITIAL_PILL: PillState = {
	kind: 'connecting',
	torrents: null,
	done: null,
	total: null
};

export const statusRef = $state<{
	current: ReturnType<typeof createShellStatus> | null;
	/** The pill component, loaded with the status module (it carries the icons and formatters). */
	Pill: Component<{ state: PillState }> | null;
	/** The connection states, fetched with the stream. */
	Connection: Component<{ kind: 'unreachable' | 'reconnecting' }> | null;
}>({ current: null, Pill: null, Connection: null });
