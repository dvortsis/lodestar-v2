// The filters API's shapes as the flows use them (schema.d.ts, generated from the server).
import type { components } from '$lib/api/schema';

type Schemas = components['schemas'];

export type Change = Schemas['Change'];
export type ChangeOp = Change['op'];
export type ChangeBody = Schemas['ChangeBody'];
export type Assessment = Schemas['Assessment'];
export type Preview = Schemas['Preview'];
export type ApplyResponse = Schemas['ApplyResponse'];
export type JobStatus = Schemas['JobStatus'];
export type JobProgress = Schemas['JobProgress'];
export type MatchInput = Schemas['MatchInput'];

/** One member of `Change` by its `op`. */
export type ChangeOf<O extends ChangeOp> = Extract<Change, { op: O }>;
