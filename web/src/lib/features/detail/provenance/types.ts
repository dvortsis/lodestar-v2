// The provenance shape SRC-17a plans for `TorrentDetail.provenance`. Replaced by
// `components['schemas']['...']` once SRC-17a's types land; keep field names in step.
export interface ProvenanceSource {
	/** The source's display name ("DHT", "Nyaa"). Null for a private source seen by a non-admin. */
	name: string | null;
	/** Unix seconds. */
	first_seen_at: number;
}
export interface ClaimDifference {
	field: 'size' | 'files' | 'name';
	claimed: string;
	verified: string;
}
export interface Provenance {
	sources: ProvenanceSource[];
	verified: boolean;
	differences: ClaimDifference[];
}
