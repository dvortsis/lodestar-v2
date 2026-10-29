/** The magnet to show in a selected read-only field when no copy method worked (ManualMagnet). */
import { onDetailInvalidated } from './cache';

export const manual = $state<{ magnet: string | null }>({ magnet: null });

// A fallback can outlive the drawer. It shares the cache's visibility lifetime.
onDetailInvalidated(() => (manual.magnet = null));
