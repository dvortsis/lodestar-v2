// What Settings → Logging loads once its first view shows, in one chunk: the catalog of what the
// server has on, the banners and the data kinds. Both wait on `GET /logs/catalog` anyway, so this
// loads alongside that request.
export { LoggingCatalog } from './catalog.svelte';
export { default as Banners } from './Banners.svelte';
export { default as DataKinds } from './DataKinds.svelte';
