// Text measurement for label spacing. Spacing is measured in the font actually in use (the host's
// font size follows the user's browser setting), never estimated from a character count.
let ctx: CanvasRenderingContext2D | null | undefined;

/** Pixel size in a CSS font shorthand ("16px Atkinson…"); 16 when none can be read. */
export function fontPx(font: string): number {
	const m = /(\d+(?:\.\d+)?)px/.exec(font);
	return m ? Number(m[1]) : 16;
}

/** Width of `text` in `font`, in CSS px. Falls back to 0.6 em per character without a canvas. */
export function measureText(font: string, text: string): number {
	if (ctx === undefined) {
		try {
			ctx = document.createElement('canvas').getContext('2d');
		} catch {
			ctx = null;
		}
	}
	if (ctx) {
		ctx.font = font;
		return ctx.measureText(text).width;
	}
	return text.length * fontPx(font) * 0.6;
}

export const widest = (font: string, texts: readonly string[]) =>
	texts.reduce((w, t) => Math.max(w, measureText(font, t)), 0);

/** Forget the cached canvas (tests swap `getContext`). */
export function resetMeasure() {
	ctx = undefined;
}
