// Test doubles for the results specs: a reactive stand-in for the shell's status (its rebuild
// snapshot), so the panel's banner follows it the way it does in the app.
import type { RebuildSnapshot } from '$lib/shell/status';

let rebuild = $state.raw<RebuildSnapshot | null>(null);
let pillKind = $state('unknown');

export const testStatus = {
	get rebuild() {
		return rebuild;
	},
	get pill() {
		return { kind: pillKind };
	}
};

export function setPill(kind: string) {
	pillKind = kind;
}

export function setRebuild(next: RebuildSnapshot | null) {
	rebuild = next;
}
