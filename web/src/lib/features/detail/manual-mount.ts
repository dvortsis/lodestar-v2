// Both the mount runtime and the fallback field are needed only after copying fails.
import { mount } from 'svelte';
import ManualMagnet from './ManualMagnet.svelte';

let mounted = false;

export function mountManual() {
	if (mounted) return;
	mount(ManualMagnet, { target: document.body });
	mounted = true;
}
