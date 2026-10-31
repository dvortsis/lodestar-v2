// How the screen's lazy parts reach the catalog, which is created once `parts.ts` loads. Types
// only: a shared value here would make the lazy parts import the screen's own chunk.
import type { LoggingCatalog } from './catalog.svelte';

/** The context key: a string, for the reason above. */
export type CatalogKey = 'logging-catalog';
export interface CatalogHolder {
	readonly catalog: LoggingCatalog | undefined;
}
