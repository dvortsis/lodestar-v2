import { afterEach, expect, it, vi } from 'vitest';
import { focusSettingField } from './focus';

afterEach(() => {
	document.body.replaceChildren();
	vi.restoreAllMocks();
});

it('skips disabled controls and the field header menu', () => {
	document.body.innerHTML =
		'<div id="network.port"><button>Options</button><label for="locked">Port</label><div data-control><input id="locked" disabled><input aria-label="editable"></div></div>';
	focusSettingField('network.port');
	expect(document.activeElement).toBe(document.querySelector('[aria-label="editable"]'));
});

it('does not focus unrelated controls when a field has no control area', () => {
	document.body.innerHTML = '<div id="storage.path"><button>Options</button></div>';
	focusSettingField('storage.path');
	expect(document.activeElement).toBe(document.body);
});
