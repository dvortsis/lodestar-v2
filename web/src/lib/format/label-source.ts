// Discovery sources (X-labels), for "Found by" and the Source facet, where the server sends each
// source's name with its plugin id. Insights sends ids alone: `label-source-names.ts` adds the
// plugins' names there, so the detail's first view doesn't carry them.
import { m } from '$lib/paraglide/messages.js';
import { word } from './label-shared';

// The two importers have no manifest, so no name from the server.
const IMPORTER: Record<string, () => string> = {
	bitmagnet: m.label_source_import_bitmagnet,
	magnetico: m.label_source_import_magnetico
};

/**
 * A source by plugin id. "DHT" for the DHT, as the glossary writes it; else the server's own name
 * for it, when it sent one that isn't just the id; else an importer's name; else the id.
 */
export function sourceLabel(id: string, name?: string | null): string {
	if (id === 'dht' || name === 'Mainline DHT') return 'DHT';
	if (name && name !== id) return name;
	return word(IMPORTER, id);
}
