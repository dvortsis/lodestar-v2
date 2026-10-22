// `text/event-stream` framing (WHATWG HTML §9.2), without EventSource: `id:`, `event:`, `data:`
// (multi-line), `retry:`, `:` comments, CRLF / LF / CR line ends, frames split across chunks.

export type SseItem =
	{ type: 'message'; event: string; data: string; id?: string } | { type: 'retry'; ms: number };

export class SseParser {
	private line = '';
	private afterCR = false;
	private data: string[] = [];
	private event = '';
	private id: string | undefined;

	/** Feeds decoded text (any chunking) and returns the items it completed. */
	push(chunk: string): SseItem[] {
		const out: SseItem[] = [];
		for (const ch of chunk) {
			if (ch === '\n' && this.afterCR) {
				this.afterCR = false; // the LF of a CRLF whose CR already ended the line
				continue;
			}
			this.afterCR = ch === '\r';
			if (ch === '\n' || ch === '\r') this.endLine(out);
			else this.line += ch;
		}
		return out;
	}

	private endLine(out: SseItem[]): void {
		const line = this.line;
		this.line = '';
		if (line === '') {
			if (this.data.length) {
				out.push({
					type: 'message',
					event: this.event || 'message',
					data: this.data.join('\n'),
					...(this.id === undefined ? {} : { id: this.id })
				});
			}
			this.data = [];
			this.event = '';
			this.id = undefined;
			return;
		}
		if (line[0] === ':') return; // comment / heartbeat
		const i = line.indexOf(':');
		const field = i < 0 ? line : line.slice(0, i);
		let value = i < 0 ? '' : line.slice(i + 1);
		if (value[0] === ' ') value = value.slice(1);
		if (field === 'data') this.data.push(value);
		else if (field === 'event') this.event = value;
		else if (field === 'id') {
			if (!value.includes('\0')) this.id = value;
		} else if (field === 'retry' && /^\d+$/.test(value))
			out.push({ type: 'retry', ms: Number(value) });
	}
}
