import type { Change } from '../flows/types';
import type { Pending } from './model.svelte';

/** Starts a change through the dialog (the screen loads it on first use). */
export type Ask = (change: Change, pending: Pending) => void | Promise<void>;
