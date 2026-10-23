// Playwright only: `/system`, the network check, and the restart and address calls answered
// through the OpenAPI mock, with state a test can change: which fields wait for a restart, an
// address change under way, and a restart that moves `started_at`.
import { checkBody, systemInfo } from './golden';
import type { AddressChange, SystemInfo } from './system';

export interface MockLike {
	on(operationId: never, override: never): unknown;
}

type Reply = { status: number; body?: unknown };

export class SystemBackend {
	system: SystemInfo;
	check: Record<string, unknown> = checkBody();
	readonly posts: string[] = [];
	/** What the next `POST /network/check` answers instead of the check (a 429, say). */
	nextCheck: Reply[] = [];

	constructor(over: Partial<SystemInfo> = {}) {
		this.system = systemInfo(over);
	}

	attach(mock: MockLike): this {
		const on = mock.on.bind(mock) as (id: string, f: unknown) => unknown;
		on('getSystem', () => ({ status: 200, body: structuredClone(this.system) }));
		on('getNetworkCheck', () => ({ status: 200, body: this.check }));
		on('restartNetworkCheck', () => this.nextCheck.shift() ?? { status: 200, body: this.check });
		on('restartSystem', () => {
			this.posts.push('restart');
			// The "new process": a later start, nothing waiting any more, any address change done.
			this.system = {
				...this.system,
				started_at: this.system.started_at + 60,
				restart_required: [],
				address_change:
					this.system.address_change?.state === 'awaiting_restart'
						? { ...this.system.address_change, state: 'awaiting_confirmation' }
						: null
			};
			return { status: 202, body: { estimated_seconds: 20, restarting: true } };
		});
		on('keepAddress', () => {
			this.posts.push('keep');
			this.system = { ...this.system, address_change: null };
			return { status: 200, body: { address_change: null, restarting: false } };
		});
		on('revertAddress', () => {
			this.posts.push('revert');
			this.system = { ...this.system, address_change: null };
			return { status: 200, body: { address_change: null, restarting: true } };
		});
		return this;
	}

	change(change: AddressChange | null) {
		this.system = { ...this.system, address_change: change };
	}
}
