import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { attach, COALESCE_MS, type Editor } from './editor';

let el: HTMLDivElement;
let editor: Editor;
const onchange = vi.fn();
const onsubmit = vi.fn();
const onescape = vi.fn(() => false);
const onrender = vi.fn();

beforeEach(() => {
	vi.useFakeTimers();
	el = document.createElement('div');
	el.tabIndex = 0;
	document.body.append(el);
	el.focus();
	editor = attach(el, { onchange, onsubmit, onescape, onrender });
});
afterEach(() => {
	editor.destroy();
	el.remove();
	vi.useRealTimers();
	vi.clearAllMocks();
});

/** UTF-16 offset of the caret inside `el`. */
function caretAt(): number {
	const sel = document.getSelection()!;
	const r = document.createRange();
	r.selectNodeContents(el);
	r.setEnd(sel.focusNode!, sel.focusOffset);
	return r.toString().length;
}

function putCaret(offset: number) {
	const walk = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
	let seen = 0;
	for (let n = walk.nextNode() as Text | null; n; n = walk.nextNode() as Text | null) {
		if (offset <= seen + n.data.length) {
			document.getSelection()!.collapse(n, offset - seen);
			return;
		}
		seen += n.data.length;
	}
	document.getSelection()!.collapse(el, el.childNodes.length);
}

/** What the browser does on a keystroke: change the DOM, move the caret, fire `input`. */
function type(text: string, caret = text.length, data = text.slice(-1), inputType = 'insertText') {
	el.textContent = text;
	putCaret(caret);
	el.dispatchEvent(new InputEvent('input', { inputType, data, bubbles: true }));
}

function key(k: string, init: KeyboardEventInit = {}) {
	const e = new KeyboardEvent('keydown', { key: k, bubbles: true, cancelable: true, ...init });
	el.dispatchEvent(e);
	return e;
}

const spans = () => [...el.querySelectorAll('span')].map((s) => [s.className, s.textContent]);

describe('the editor', () => {
	it('re-renders tokens as spans on input and reports the text', () => {
		type('size:>4GB "big buck" -cam');
		expect(onchange).toHaveBeenLastCalledWith('size:>4GB "big buck" -cam');
		expect(el.textContent).toBe('size:>4GB "big buck" -cam');
		expect(spans()).toEqual([
			['qt-field', 'size:'],
			['qt-range', '>4GB'],
			['qt-phrase qt-quote', '"'],
			['qt-phrase', 'big buck'],
			['qt-phrase qt-quote', '"'],
			['qt-operator', '-']
		]);
		expect(onrender).toHaveBeenLastCalledWith('size:>4GB "big buck" -cam');
	});

	it('keeps the caret where it was after the re-render', () => {
		type('res:1080p bunny');
		type('res:1080p xbunny', 11);
		expect(caretAt()).toBe(11);
		type('res:1080p xbunny', 4);
		expect(caretAt()).toBe(4);
	});

	it('does not re-render while an IME is composing, then does at compositionend', () => {
		el.dispatchEvent(new CompositionEvent('compositionstart'));
		el.textContent = 'type:电';
		el.dispatchEvent(
			new InputEvent('input', { inputType: 'insertCompositionText', isComposing: true })
		);
		expect(el.querySelector('span')).toBeNull(); // the IME's own text node is left alone
		expect(onchange).not.toHaveBeenCalled();
		// Enter confirms the composition: not a submit.
		key('Enter', { isComposing: true });
		expect(onsubmit).not.toHaveBeenCalled();
		el.textContent = 'type:电影';
		putCaret(7);
		el.dispatchEvent(new CompositionEvent('compositionend', { data: '电影' }));
		expect(onchange).toHaveBeenLastCalledWith('type:电影');
		expect(spans()).toEqual([
			['qt-field', 'type:'],
			['qt-value', '电影']
		]);
		expect(caretAt()).toBe(7);
	});

	it('a parse answer or outside text during a composition leaves the composition alone', () => {
		type('name:');
		el.dispatchEvent(new CompositionEvent('compositionstart'));
		const composed = document.createTextNode('电');
		el.append(composed); // the IME's own node, mid-composition
		el.dispatchEvent(
			new InputEvent('input', { inputType: 'insertCompositionText', isComposing: true })
		);
		editor.setError({ from: 0, to: 5 }); // /parse answered for "name:"
		editor.set('name:'); // the state's text, from that parse
		expect(composed.isConnected).toBe(true);
		expect(el.textContent).toBe('name:电');
		expect(el.querySelector('.qt-error')).toBeNull();
		el.dispatchEvent(new CompositionEvent('compositionend', { data: '电' }));
		expect(onchange).toHaveBeenLastCalledWith('name:电');
		// The error that arrived meanwhile is drawn now, over the current text.
		expect(el.querySelector('.qt-error')?.textContent).toBe('name:');
	});

	it('pastes plain text only, on one line', () => {
		type('a  b', 2);
		const paste = new Event('paste', { bubbles: true, cancelable: true });
		Object.defineProperty(paste, 'clipboardData', {
			value: {
				getData: (t: string) =>
					t === 'text/plain' ? 'big\r\nbuck\nbunny' : '<b>big</b><script>x</script>'
			}
		});
		el.dispatchEvent(paste);
		expect(paste.defaultPrevented).toBe(true);
		expect(onchange).toHaveBeenLastCalledWith('a big buck bunny b');
		expect(el.querySelector('b, script')).toBeNull();
		expect(caretAt()).toBe(16);
	});

	it('turns newlines from a drop or the browser into spaces', () => {
		type('a\nb');
		expect(onchange).toHaveBeenLastCalledWith('a b');
		expect(el.textContent).toBe('a b');
		// A CRLF is one space, and the caret after it stays after it.
		type('x\r\ny\r\nz', 6);
		expect(onchange).toHaveBeenLastCalledWith('x y z');
		expect(caretAt()).toBe(4);
	});

	it('undoes and redoes, with a typing burst as one step', () => {
		type('b');
		vi.advanceTimersByTime(100);
		type('bu');
		vi.advanceTimersByTime(100);
		type('bun');
		vi.advanceTimersByTime(COALESCE_MS + 1);
		type('bunn'); // a pause starts a new step
		key('z', { ctrlKey: true });
		expect(onchange).toHaveBeenLastCalledWith('bun');
		key('z', { ctrlKey: true });
		expect(onchange).toHaveBeenLastCalledWith('');
		key('z', { ctrlKey: true }); // nothing older
		expect(onchange).toHaveBeenCalledTimes(6);
		key('z', { ctrlKey: true, shiftKey: true });
		expect(onchange).toHaveBeenLastCalledWith('bun');
		key('y', { ctrlKey: true });
		expect(onchange).toHaveBeenLastCalledWith('bunn');
		expect(caretAt()).toBe(4);
	});

	it('ends a burst at a space, and Cmd+Z works too', () => {
		type('big');
		type('big ', 4, ' ');
		type('big b');
		key('z', { metaKey: true });
		expect(onchange).toHaveBeenLastCalledWith('big ');
	});

	it('takes the browser history inputs (menu Undo) itself', () => {
		type('abc');
		const e = new InputEvent('beforeinput', { inputType: 'historyUndo', cancelable: true });
		el.dispatchEvent(e);
		expect(e.defaultPrevented).toBe(true);
		expect(onchange).toHaveBeenLastCalledWith('');
		const p = new InputEvent('beforeinput', { inputType: 'insertParagraph', cancelable: true });
		el.dispatchEvent(p);
		expect(p.defaultPrevented).toBe(true);
	});

	it('Enter submits', () => {
		type('bunny');
		expect(key('Enter').defaultPrevented).toBe(true);
		expect(onsubmit).toHaveBeenCalledOnce();
		// The insertParagraph some browsers still send for that Enter does not submit again.
		el.dispatchEvent(
			new InputEvent('beforeinput', { inputType: 'insertParagraph', cancelable: true })
		);
		expect(onsubmit).toHaveBeenCalledOnce();
	});

	it("a soft keyboard's search key (keyCode 229, then insertParagraph) submits", () => {
		type('bunny');
		key('Enter', { keyCode: 229 });
		expect(onsubmit).not.toHaveBeenCalled();
		const e = new InputEvent('beforeinput', { inputType: 'insertParagraph', cancelable: true });
		el.dispatchEvent(e);
		expect(e.defaultPrevented).toBe(true);
		expect(onsubmit).toHaveBeenCalledOnce();
		// A hardware Enter (no insertParagraph follows), then the soft keyboard's search key.
		key('Enter');
		expect(onsubmit).toHaveBeenCalledTimes(2);
		key('Enter', { keyCode: 229 });
		el.dispatchEvent(
			new InputEvent('beforeinput', { inputType: 'insertParagraph', cancelable: true })
		);
		expect(onsubmit).toHaveBeenCalledTimes(3);
		// Not while composing.
		el.dispatchEvent(new CompositionEvent('compositionstart'));
		el.dispatchEvent(
			new InputEvent('beforeinput', { inputType: 'insertParagraph', cancelable: true })
		);
		expect(onsubmit).toHaveBeenCalledTimes(3);
	});

	it('Esc closes a popover first, then clears, and undo brings the text back', () => {
		type('bunny');
		onescape.mockReturnValueOnce(true);
		key('Escape');
		expect(el.textContent).toBe('bunny');
		key('Escape');
		expect(onchange).toHaveBeenLastCalledWith('');
		expect(el.textContent).toBe('');
		expect(el.childNodes).toHaveLength(0); // :empty again
		key('z', { ctrlKey: true });
		expect(onchange).toHaveBeenLastCalledWith('bunny');
		// Esc on an empty bar is left to others.
		editor.set('');
		expect(key('Escape').defaultPrevented).toBe(false);
	});

	it("starts its history at the URL's query, so undo never empties a pasted link", () => {
		editor.set('res:1080p', true);
		key('z', { ctrlKey: true });
		expect(editor.text).toBe('res:1080p');
		expect(onchange).not.toHaveBeenCalled();
	});

	it('takes text from outside as one undo step, without reporting it back', () => {
		type('a');
		onchange.mockClear();
		editor.set('size:>4GB');
		expect(onchange).not.toHaveBeenCalled();
		expect(editor.text).toBe('size:>4GB');
		expect(caretAt()).toBe(9);
		key('z', { ctrlKey: true });
		expect(onchange).toHaveBeenLastCalledWith('a');
	});

	it('marks a parse error, an empty span at the end too', () => {
		type('size:big x');
		editor.setError({ from: 5, to: 8 });
		expect(spans()).toEqual([
			['qt-field', 'size:'],
			['qt-value qt-error', 'big']
		]);
		expect(el.textContent).toBe('size:big x');
		editor.setError({ from: 10, to: 10 });
		expect(el.lastElementChild?.className).toBe('qt-error-end');
		expect(el.textContent).toBe('size:big x');
		editor.setError(null);
		expect(el.querySelector('.qt-error, .qt-error-end')).toBeNull();
	});

	it('attaching mid-composition leaves the composition alone until it ends', () => {
		const pre = document.createElement('div');
		const composed = document.createTextNode('name:にほん');
		pre.append(composed);
		const changes = vi.fn();
		const ed = attach(pre, { onchange: changes, onsubmit, composing: true });
		expect(pre.firstChild).toBe(composed); // not re-rendered
		pre.textContent = 'name:日本';
		pre.dispatchEvent(new CompositionEvent('compositionend', { data: '日本' }));
		expect(changes).toHaveBeenLastCalledWith('name:日本');
		expect(pre.querySelector('.qt-field')?.textContent).toBe('name:');
		ed.destroy();
	});

	it('picks up what was typed before it attached', () => {
		const pre = document.createElement('div');
		pre.textContent = 'res:4k';
		const ed = attach(pre, { onchange, onsubmit });
		expect(ed.text).toBe('res:4k');
		expect(pre.querySelector('.qt-field')?.textContent).toBe('res:');
		ed.destroy();
	});

	it('focusEnd puts the caret at the end', () => {
		type('bunny', 1);
		el.blur();
		editor.focusEnd();
		expect(document.activeElement).toBe(el);
		expect(caretAt()).toBe(5);
	});

	it('builds DOM nodes, never markup: tags in the text stay text', () => {
		type('<img src=x onerror=alert(1)>');
		expect(el.querySelector('img')).toBeNull();
		expect(el.textContent).toBe('<img src=x onerror=alert(1)>');
	});
});
