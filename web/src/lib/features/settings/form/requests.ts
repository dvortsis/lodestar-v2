// Every request the form engine makes, in one place.
import { api } from '$lib/api/client';
import type { components } from '$lib/api/schema';

/**
 * A batch body, `{section: {field: value, if_revision?, confirm_adult?}}`.
 *
 * `SettingsBatch` is `Record<string, never>` in `schema.d.ts` (the Rust side types it as an
 * untyped object), so the typed client can't express a body. This is the one place it's cast.
 * Backend follow-up: give `SettingsBatch` and `SettingsPatch` `additionalProperties` in the
 * OpenAPI document, then drop the cast.
 */
export type Batch = Record<string, Record<string, unknown>>;
const body = (batch: Batch) => batch as unknown as components['schemas']['SettingsBatch'];

export const getIndex = () => api.get('/settings');

export const getSection = (section: string, signal?: AbortSignal) =>
	api.get('/settings/{section}', { path: { section }, signal });

export const patchBatch = (batch: Batch) => api.patch('/settings', { body: body(batch) });

export const previewBatch = (batch: Batch, signal?: AbortSignal) =>
	api.post('/settings/preview', { body: body(batch), signal });
