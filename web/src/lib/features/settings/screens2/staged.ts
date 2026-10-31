// Reading the values on screen for a test. A secret is write-only: the form holds it only while
// it's being replaced, and a test that leaves it out uses the saved one (B-settings-sections
// part b), so nothing here ever asks the server for a secret.
import type { SettingsForm } from '../form';

type K = Parameters<SettingsForm['field']>[0];

/** A string field's value; '' when it isn't one. */
export const str = (form: SettingsForm, key: K): string => {
	const v = form.field(key).value;
	return typeof v === 'string' ? v : '';
};

/** `{ name: value }` when the string isn't empty, else nothing: an empty one means "the saved". */
export const given = (name: string, value: string) =>
	value.trim() === '' ? {} : { [name]: value };

export const flag = (form: SettingsForm, key: K): boolean => form.field(key).value === true;

export const num = (form: SettingsForm, key: K): number => {
	const v = form.field(key).value;
	return typeof v === 'number' && Number.isFinite(v) ? v : 0;
};

export const list = (form: SettingsForm, key: K): string[] => {
	const v = form.field(key).value;
	return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
};
