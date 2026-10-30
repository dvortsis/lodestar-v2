/** Focus a field's control, never the header's options menu. */
export function focusSettingField(key: string): void {
	const root = document.getElementById(key);
	const area = root?.querySelector<HTMLElement>('[data-control]');
	const label = root?.querySelector<HTMLLabelElement>('label[for]');
	const target = label?.htmlFor ? document.getElementById(label.htmlFor) : null;
	const selectors = [
		'input:checked:not(:disabled)',
		'[role="radio"][aria-checked="true"]:not(:disabled)',
		'input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [role="radio"]:not(:disabled), button:not(:disabled)'
	];
	const control =
		(target && !target.matches(':disabled') ? target : null) ??
		selectors.map((selector) => area?.querySelector<HTMLElement>(selector)).find(Boolean);
	root?.scrollIntoView?.({ block: 'center' });
	control?.focus({ preventScroll: true });
}
