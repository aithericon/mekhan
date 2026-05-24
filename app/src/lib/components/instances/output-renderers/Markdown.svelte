<script lang="ts">
	// Tiny structural markdown renderer — covers the subset LLM responses
	// actually produce (headings, bold/italic, inline code, code blocks,
	// bullet/numbered lists, paragraphs, horizontal rules). Renders via
	// Svelte primitives, never `{@html}`, so an LLM emitting `<script>` in
	// its response stays inert.
	//
	// Deliberately small: ~120 lines vs. ~30KB for marked. Trade-off is no
	// support for tables, footnotes, autolinks, GFM strikethrough — those
	// rarely appear in chat-style LLM output and the JSON fallback is one
	// click away for the cases where they do.

	type Block =
		| { type: 'heading'; level: number; text: string }
		| { type: 'paragraph'; text: string }
		| { type: 'list'; ordered: boolean; items: string[] }
		| { type: 'code'; lang: string; text: string }
		| { type: 'hr' };

	type Run =
		| { type: 'text'; text: string }
		| { type: 'bold'; text: string }
		| { type: 'italic'; text: string }
		| { type: 'code'; text: string };

	let { content }: { content: string } = $props();

	function parseBlocks(src: string): Block[] {
		const lines = src.split('\n');
		const blocks: Block[] = [];
		let i = 0;
		while (i < lines.length) {
			const line = lines[i];
			// Fenced code block
			if (line.startsWith('```')) {
				const lang = line.slice(3).trim();
				i++;
				const codeLines: string[] = [];
				while (i < lines.length && !lines[i].startsWith('```')) {
					codeLines.push(lines[i]);
					i++;
				}
				if (i < lines.length) i++; // skip closing fence
				blocks.push({ type: 'code', lang, text: codeLines.join('\n') });
				continue;
			}
			// ATX heading
			const hm = /^(#{1,6})\s+(.+)/.exec(line);
			if (hm) {
				blocks.push({ type: 'heading', level: hm[1].length, text: hm[2] });
				i++;
				continue;
			}
			// Horizontal rule
			if (/^(---+|\*\*\*+|___+)\s*$/.test(line)) {
				blocks.push({ type: 'hr' });
				i++;
				continue;
			}
			// List (ordered or unordered). Mixed bullets/numbers collapse into
			// one block — LLMs commonly emit numbered top-level + bullet
			// sub-items, and a single block is good enough rendering.
			const lm = /^\s*([-*+]|\d+\.)\s+(.+)/.exec(line);
			if (lm) {
				const ordered = /^\d+\./.test(lm[1]);
				const items: string[] = [];
				while (i < lines.length) {
					const m = /^\s*([-*+]|\d+\.)\s+(.+)/.exec(lines[i]);
					if (!m) break;
					items.push(m[2]);
					i++;
				}
				blocks.push({ type: 'list', ordered, items });
				continue;
			}
			// Blank line — paragraph separator
			if (line.trim() === '') {
				i++;
				continue;
			}
			// Paragraph — accumulate until structural break.
			const paraLines: string[] = [];
			while (
				i < lines.length &&
				lines[i].trim() !== '' &&
				!/^(\s*#{1,6}\s|```|\s*[-*+]\s|\s*\d+\.\s)/.test(lines[i])
			) {
				paraLines.push(lines[i]);
				i++;
			}
			blocks.push({ type: 'paragraph', text: paraLines.join('\n') });
		}
		return blocks;
	}

	function parseInline(text: string): Run[] {
		const runs: Run[] = [];
		let i = 0;
		const n = text.length;
		while (i < n) {
			// Inline code: `…`
			if (text[i] === '`') {
				const end = text.indexOf('`', i + 1);
				if (end !== -1) {
					runs.push({ type: 'code', text: text.slice(i + 1, end) });
					i = end + 1;
					continue;
				}
			}
			// Bold: **…**
			if (text[i] === '*' && text[i + 1] === '*') {
				const end = text.indexOf('**', i + 2);
				if (end !== -1) {
					runs.push({ type: 'bold', text: text.slice(i + 2, end) });
					i = end + 2;
					continue;
				}
			}
			// Italic: *…*  (must not consume **; we already handled bold above)
			if (text[i] === '*') {
				const end = text.indexOf('*', i + 1);
				// Reject if the closing also looks like bold (i.e. another `*` right after).
				if (end !== -1 && text[end + 1] !== '*') {
					runs.push({ type: 'italic', text: text.slice(i + 1, end) });
					i = end + 1;
					continue;
				}
			}
			// Plain text — accumulate until next inline marker.
			let j = i;
			while (j < n && text[j] !== '`' && text[j] !== '*') j++;
			if (j > i) {
				runs.push({ type: 'text', text: text.slice(i, j) });
			}
			// Skip orphan marker that didn't form a span (avoid infinite loop).
			if (j === i) {
				runs.push({ type: 'text', text: text[i] });
				i++;
			} else {
				i = j;
			}
		}
		return runs;
	}

	const blocks = $derived(parseBlocks(content));
</script>

{#snippet inlineRuns(text: string)}
	{#each parseInline(text) as run, i (i)}
		{#if run.type === 'bold'}<strong class="font-semibold">{run.text}</strong>
		{:else if run.type === 'italic'}<em>{run.text}</em>
		{:else if run.type === 'code'}<code class="rounded bg-muted px-1 py-0.5 font-mono text-sm">{run.text}</code>
		{:else}{run.text}{/if}
	{/each}
{/snippet}

<div class="space-y-3 text-sm text-foreground break-words">
	{#each blocks as block, i (i)}
		{#if block.type === 'heading'}
			{#if block.level <= 2}
				<h2 class="text-base font-semibold">{@render inlineRuns(block.text)}</h2>
			{:else if block.level === 3}
				<h3 class="text-sm font-semibold">{@render inlineRuns(block.text)}</h3>
			{:else}
				<h4 class="text-sm font-semibold text-muted-foreground">
					{@render inlineRuns(block.text)}
				</h4>
			{/if}
		{:else if block.type === 'paragraph'}
			<p class="whitespace-pre-wrap leading-relaxed">{@render inlineRuns(block.text)}</p>
		{:else if block.type === 'list'}
			{#if block.ordered}
				<ol class="ml-5 list-decimal space-y-1">
					{#each block.items as item, j (j)}
						<li>{@render inlineRuns(item)}</li>
					{/each}
				</ol>
			{:else}
				<ul class="ml-5 list-disc space-y-1">
					{#each block.items as item, j (j)}
						<li>{@render inlineRuns(item)}</li>
					{/each}
				</ul>
			{/if}
		{:else if block.type === 'code'}
			<pre class="overflow-x-auto rounded-md border border-border bg-muted/30 p-3 font-mono text-sm whitespace-pre-wrap break-words">{block.text}</pre>
		{:else if block.type === 'hr'}
			<hr class="border-border" />
		{/if}
	{/each}
</div>
