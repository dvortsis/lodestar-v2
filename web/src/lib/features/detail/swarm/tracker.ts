// A tracker's host for display. Tracker URLs can carry a passkey in the path or query (private
// trackers), so only the host is ever shown: never the path, query, credentials or port.
export function trackerHost(url: string): string | null {
	try {
		const u = new URL(url);
		return u.hostname || null;
	} catch {
		return null;
	}
}
