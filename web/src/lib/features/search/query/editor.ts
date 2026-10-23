// The query bar's editor: a single-line `contenteditable="plaintext-only"` element whose text is
// re-rendered as token spans on every input (§10.4: tokens differ by weight, mono font and colour,
// which a native <input> with an overlay can't line up with its caret). Rebuilding the DOM breaks
// the browser's own undo stack, so undo and redo are kept here, with typing bursts coalesced.
// The spans are built as DOM nodes from `tokenize()`; no markup string is ever parsed.
import { tokenize, type TokenKind } from './tokenize';

export interface Range16 {
	from: number;
	to: number;
}

export interface EditorOptions {
	/** The user changed the text (typing, paste, undo, redo, Esc). */
	onchange: (text: string) => void;
	/** Enter. */
	onsubmit: () => void;
	/** Esc: return true when something else took it (an open popover or listbox closed). */
	onescape?: () => boolean;
	/** After every render, with the text shown (the bar shows its placeholder when empty). */
	onrender?: (text: string) => void;
	/**
	 * An IME composition already open when the editor attaches (typing started before it loaded):
	 * the first render then waits for its compositionend, so the composition survives.
	 */
	composing?: boolean;
}

export interface Editor {
	readonly text: string;
	/**
	 * Text from outside (the URL, the server's canonical text, a "Try"): one undo step. With
	 * `base`, it replaces the history instead (the URL's query when the editor starts, so undo
	 * never empties a pasted link).
	 */
	set(text: string, base?: boolean): void;
	/** Focus with the caret at the end. */
	focusEnd(): void;
	/** The range to mark as a parse error, in UTF-16 offsets, or null. */
	setError(range: Range16 | null): void;
	undo(): void;
	redo(): void;
	destroy(): void;
}

/** Class per token kind: the rules live in the bar's stylesheet (`Live.svelte`). */
const CLASS: Record<TokenKind, string> = {
	field: 'qt-field',
	value: 'qt-value',
	range: 'qt-range',
	operator: 'qt-operator',
	phrase: 'qt-phrase',
	text: ''
};

/** Consecutive edits of one kind this close together are one undo step. */
export const COALESCE_MS = 1000;
const HISTORY_MAX = 200;
/** Keeps the caret this far from the edge when scrolling a long query. */
const CARET_MARGIN = 16;

interface Snapshot {
	text: string;
	anchor: number;
	focus: number;
}

type EditKind = 'insert' | 'delete' | null;

const oneLine = (s: string) => s.replace(/\r\n|[\r\n]/g, ' ');

/** A caret measured in `raw` (the DOM), moved to `oneLine(raw)`: each CRLF before it is one char. */
function shrink(raw: string, sel: { anchor: number; focus: number } | null) {
	if (!sel) return null;
	const at = (n: number) => n - (raw.slice(0, n).match(/\r\n/g)?.length ?? 0);
	return { anchor: at(sel.anchor), focus: at(sel.focus) };
}

function editKind(inputType: string | undefined): EditKind {
	if (!inputType) return null;
	if (inputType === 'insertText' || inputType === 'insertCompositionText') return 'insert';
	if (inputType.startsWith('delete')) return 'delete';
	return null;
}

export function attach(el: HTMLElement, opts: EditorOptions): Editor {
	let text = oneLine(el.textContent ?? '');
	let error: Range16 | null = null;
	let composing = opts.composing ?? false;
	/** Enter already submitted from keydown: the beforeinput that may follow must not again. */
	let submitted = false;

	const history: Snapshot[] = [{ text, anchor: text.length, focus: text.length }];
	let at = 0;
	let lastKind: EditKind = null;
	let lastTime = 0;
	let breakNext = false;

	// ---- caret, in UTF-16 offsets over the text ----

	function offsetOf(node: Node, offset: number): number {
		const r = document.createRange();
		r.selectNodeContents(el);
		r.setEnd(node, offset);
		return r.toString().length;
	}

	function caret(): { anchor: number; focus: number } | null {
		const sel = document.getSelection();
		if (!sel || !sel.rangeCount || !sel.anchorNode || !el.contains(sel.anchorNode)) return null;
		if (!sel.focusNode || !el.contains(sel.focusNode)) return null;
		return {
			anchor: offsetOf(sel.anchorNode, sel.anchorOffset),
			focus: offsetOf(sel.focusNode, sel.focusOffset)
		};
	}

	function point(offset: number): [Node, number] {
		const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
		let seen = 0;
		let last: Text | null = null;
		for (let n = walk.nextNode() as Text | null; n; n = walk.nextNode() as Text | null) {
			const len = n.data.length;
			if (offset <= seen + len) return [n, offset - seen];
			seen += len;
			last = n;
		}
		return last ? [last, last.data.length] : [el, 0];
	}

	function place(anchor: number, focus: number) {
		const sel = document.getSelection();
		if (!sel) return;
		const [an, ao] = point(Math.min(anchor, text.length));
		const [fn, fo] = point(Math.min(focus, text.length));
		sel.setBaseAndExtent(an, ao, fn, fo);
		reveal(sel);
	}

	/** Keeps the caret in view in a long, horizontally scrolling query. */
	function reveal(sel: Selection) {
		const range = sel.rangeCount ? sel.getRangeAt(0) : null;
		if (!range?.getBoundingClientRect) return; // no layout (jsdom)
		const r = range.getBoundingClientRect();
		const box = el.getBoundingClientRect();
		if (!r.width && !r.height && !r.left) return; // nothing laid out (tests, hidden)
		if (r.right > box.right - CARET_MARGIN) el.scrollLeft += r.right - box.right + CARET_MARGIN;
		else if (r.left < box.left + CARET_MARGIN) el.scrollLeft -= box.left + CARET_MARGIN - r.left;
	}

	// ---- rendering ----

	function span(cls: string, from: number, to: number, out: Node[]) {
		if (to <= from) return;
		const t = document.createTextNode(text.slice(from, to));
		if (!cls) {
			out.push(t);
			return;
		}
		const s = document.createElement('span');
		s.className = cls;
		s.append(t);
		out.push(s);
	}

	/** A piece of a token, split where the error range starts and ends. */
	function piece(cls: string, from: number, to: number, out: Node[]) {
		if (!error || error.to <= from || error.from >= to) return span(cls, from, to, out);
		const a = Math.max(from, error.from);
		const b = Math.min(to, error.to);
		const err = cls ? `${cls} qt-error` : 'qt-error';
		span(cls, from, a, out);
		span(err, a, b, out);
		span(cls, b, to, out);
	}

	function render(restore: { anchor: number; focus: number } | null) {
		const out: Node[] = [];
		for (const tok of tokenize(text)) {
			const cls = CLASS[tok.kind];
			if (tok.kind !== 'phrase') {
				piece(cls, tok.from, tok.to, out);
				continue;
			}
			// Quotes in muted text, the phrase itself in medium weight.
			const closed = tok.to - tok.from > 1 && text[tok.to - 1] === '"';
			const inner = closed ? tok.to - 1 : tok.to;
			piece(`${cls} qt-quote`, tok.from, tok.from + 1, out);
			piece(cls, tok.from + 1, inner, out);
			if (closed) piece(`${cls} qt-quote`, inner, tok.to, out);
		}
		// An error at the very end marks the end itself (an empty span, drawn by CSS).
		if (error && error.from >= text.length && text) {
			const mark = document.createElement('span');
			mark.className = 'qt-error-end';
			out.push(mark);
		}
		el.replaceChildren(...out);
		if (restore && document.activeElement === el) place(restore.anchor, restore.focus);
		opts.onrender?.(text);
	}

	// ---- history ----

	function record(next: string, sel: { anchor: number; focus: number }, kind: EditKind) {
		const now = Date.now();
		const snap = { text: next, ...sel };
		const coalesce =
			kind !== null && kind === lastKind && !breakNext && now - lastTime < COALESCE_MS && at > 0;
		if (coalesce) history[at] = snap;
		else {
			history.splice(at + 1, Infinity, snap);
			if (history.length > HISTORY_MAX) history.shift();
			at = history.length - 1;
		}
		lastKind = kind;
		lastTime = now;
	}

	function apply(snap: Snapshot) {
		text = snap.text;
		lastKind = null;
		render(snap);
		opts.onchange(text);
	}

	function edit(next: string, sel: { anchor: number; focus: number }, kind: EditKind) {
		if (next === text) {
			render(sel); // the DOM may still hold the browser's own nodes
			return;
		}
		record(next, sel, kind);
		text = next;
		render(sel);
		opts.onchange(text);
	}

	// ---- events ----

	function oninput(e: Event) {
		const ev = e as InputEvent;
		if (composing || ev.isComposing) return; // re-rendered at compositionend
		const raw = el.textContent ?? '';
		const next = oneLine(raw);
		const sel = shrink(raw, caret()) ?? { anchor: next.length, focus: next.length };
		const kind = editKind(ev.inputType);
		edit(next, sel, kind);
		// A space ends a typing burst: the next word is its own undo step.
		breakNext = kind === 'insert' && /\s/.test(ev.data ?? '');
	}

	function insert(s: string) {
		const sel = caret() ?? { anchor: text.length, focus: text.length };
		const lo = Math.min(sel.anchor, sel.focus);
		const hi = Math.max(sel.anchor, sel.focus);
		const end = lo + s.length;
		edit(text.slice(0, lo) + s + text.slice(hi), { anchor: end, focus: end }, null);
	}

	function onpaste(e: Event) {
		e.preventDefault(); // plain text only, on one line
		insert(oneLine((e as ClipboardEvent).clipboardData?.getData('text/plain') ?? ''));
	}

	function onbeforeinput(e: Event) {
		const type = (e as InputEvent).inputType;
		if (type === 'historyUndo' || type === 'historyRedo') {
			e.preventDefault();
			if (type === 'historyUndo') undo();
			else redo();
		} else if (type === 'insertParagraph' || type === 'insertLineBreak') {
			// One line. Enter submits from keydown; a soft keyboard's "search" key may arrive only as
			// this (Android sends keyCode 229 first), so it submits here, once, when not composing.
			e.preventDefault();
			if (!composing && !(e as InputEvent).isComposing && !submitted) opts.onsubmit();
		}
		submitted = false;
	}

	function onkeydown(event: Event) {
		const e = event as KeyboardEvent;
		// A new key: an Enter submitted from an earlier keydown no longer silences insertParagraph
		// (a hardware Enter sends none when its keydown is prevented).
		submitted = false;
		if (e.isComposing || composing || e.keyCode === 229) return; // the IME owns these keys
		const mod = (e.ctrlKey || e.metaKey) && !e.altKey;
		const key = e.key.toLowerCase();
		if (mod && key === 'z') {
			e.preventDefault();
			if (e.shiftKey) redo();
			else undo();
		} else if (mod && key === 'y' && !e.shiftKey) {
			e.preventDefault();
			redo();
		} else if (e.key === 'Enter') {
			e.preventDefault();
			submitted = true; // the browser may still send insertParagraph for this key
			opts.onsubmit();
		} else if (e.key === 'Escape') {
			if (opts.onescape?.()) {
				e.preventDefault();
				return;
			}
			if (!text) return;
			e.preventDefault();
			edit('', { anchor: 0, focus: 0 }, null); // undo brings it back
		}
	}

	function oncompositionstart() {
		composing = true;
	}

	function oncompositionend() {
		composing = false;
		const raw = el.textContent ?? '';
		const next = oneLine(raw);
		edit(next, shrink(raw, caret()) ?? { anchor: next.length, focus: next.length }, null);
	}

	function undo() {
		const snap = history[at - 1];
		if (!snap) return;
		at--;
		apply(snap);
	}

	function redo() {
		const snap = history[at + 1];
		if (!snap) return;
		at++;
		apply(snap);
	}

	const listeners: Record<string, (e: Event) => void> = {
		input: oninput,
		beforeinput: onbeforeinput,
		paste: onpaste,
		keydown: onkeydown,
		compositionstart: oncompositionstart,
		compositionend: oncompositionend
	};
	for (const [type, fn] of Object.entries(listeners)) el.addEventListener(type, fn);

	if (!composing) render(caret());

	return {
		get text() {
			return text;
		},
		set(next: string, base = false) {
			next = oneLine(next);
			// Never under an IME composition: rebuilding the DOM would destroy it. The composed
			// text reaches the state at compositionend and comes back here as itself.
			if (next === text || composing) return;
			if (base)
				history.splice(0, Infinity, { text: next, anchor: next.length, focus: next.length });
			else record(next, { anchor: next.length, focus: next.length }, null);
			if (base) at = 0;
			text = next;
			render({ anchor: next.length, focus: next.length });
		},
		focusEnd() {
			el.focus();
			place(text.length, text.length);
			el.scrollLeft = el.scrollWidth;
		},
		setError(range: Range16 | null) {
			if (range?.from === error?.from && range?.to === error?.to) return;
			error = range;
			// Kept for compositionend's render while an IME is composing (a parse can answer then).
			if (!composing) render(caret());
		},
		undo,
		redo,
		destroy() {
			for (const [type, fn] of Object.entries(listeners)) el.removeEventListener(type, fn);
		}
	};
}
