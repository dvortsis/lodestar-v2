/** A magnet link is the only URL the UI takes from torrent data, and only with this scheme. */
const MAGNET = /^magnet:\?/;

export function isMagnet(value: unknown): value is string {
	return typeof value === 'string' && MAGNET.test(value);
}
