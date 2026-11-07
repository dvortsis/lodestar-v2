// Worker names for an admin (`GET /workers` is admin-only). Anyone else, or any failure, gets an
// empty map and the id fallback. Never keeps an address or token: only id -> name.
import { api } from '$lib/api/client';

export async function loadNames(signal: AbortSignal): Promise<Map<number, string>> {
	try {
		const r = await api.get('/workers', { signal });
		const out = new Map<number, string>();
		for (const w of r.workers) if (w.name) out.set(w.id, w.name);
		return out;
	} catch {
		return new Map();
	}
}
