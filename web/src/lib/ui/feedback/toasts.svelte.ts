// Toast queue. Features call `toast.push(...)`; <Toaster> (mounted once in the shell) renders it.
export type ToastKind = 'info' | 'success' | 'warning' | 'error';

export interface ToastInput {
	kind?: ToastKind;
	text: string;
	/** Auto-dismiss after this many ms; 0 keeps it until dismissed. Default: 6000 (0 for errors). */
	timeout?: number;
}

export interface ToastItem extends Required<Pick<ToastInput, 'kind' | 'text' | 'timeout'>> {
	id: number;
}

const MAX_VISIBLE = 4;
let nextId = 1;

export const toasts = $state<{ items: ToastItem[] }>({ items: [] });

export const toast = {
	push(input: ToastInput): number {
		const kind = input.kind ?? 'info';
		const item: ToastItem = {
			id: nextId++,
			kind,
			text: input.text,
			timeout: input.timeout ?? (kind === 'error' ? 0 : 6000)
		};
		// Oldest drop first once the queue is full; errors are kept as long as possible.
		const items = [...toasts.items, item];
		while (items.length > MAX_VISIBLE) {
			const drop = items.findIndex((t) => t.kind !== 'error');
			items.splice(drop === -1 ? 0 : drop, 1);
		}
		toasts.items = items;
		return item.id;
	},
	dismiss(id: number) {
		toasts.items = toasts.items.filter((t) => t.id !== id);
	},
	clear() {
		toasts.items = [];
	}
};
