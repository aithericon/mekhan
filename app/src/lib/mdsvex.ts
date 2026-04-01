function slugify(text: string): string {
	return text
		.toLowerCase()
		.replace(/[^a-z0-9\s-]/g, '')
		.replace(/\s+/g, '-')
		.replace(/-+/g, '-')
		.replace(/^-|-$/g, '');
}

function escapeHtml(value: string): string {
	return value
		.replaceAll('&', '&amp;')
		.replaceAll('<', '&lt;')
		.replaceAll('>', '&gt;')
		.replaceAll('"', '&quot;')
		.replaceAll("'", '&#39;');
}

function sanitizeUrl(url: string): string {
	const trimmed = url.trim();
	if (trimmed.startsWith('/')) return trimmed;
	try {
		const parsed = new URL(trimmed);
		if (
			parsed.protocol === 'http:' ||
			parsed.protocol === 'https:' ||
			parsed.protocol === 'mailto:'
		) {
			return parsed.toString();
		}
	} catch {
		return '#';
	}
	return '#';
}

export function renderInline(source: string): string {
	const codeTokens: string[] = [];
	let content = escapeHtml(source);

	content = content.replace(/`([^`]+)`/g, (_match, code: string) => {
		const token = `@@CODE_${codeTokens.length}@@`;
		codeTokens.push(`<code>${code}</code>`);
		return token;
	});

	// Images must be matched before links to avoid partial matches
	content = content.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, (_match, alt: string, url: string) => {
		const safeUrl = sanitizeUrl(url);
		return `<img src="${escapeHtml(safeUrl)}" alt="${escapeHtml(alt)}" loading="lazy" style="max-width:100%;height:auto;display:inline" />`;
	});

	content = content.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_match, text: string, url: string) => {
		const safeUrl = sanitizeUrl(url);
		return `<a href="${escapeHtml(safeUrl)}" target="_blank" rel="noreferrer">${text}</a>`;
	});
	content = content.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
	content = content.replace(/~~([^~]+)~~/g, '<del>$1</del>');
	content = content.replace(/\*([^*]+)\*/g, '<em>$1</em>');

	return content.replace(/@@CODE_(\d+)@@/g, (_match, idx: string) => codeTokens[Number(idx)] ?? '');
}

function splitTableRow(line: string): string[] {
	return line
		.replace(/^\|/, '')
		.replace(/\|$/, '')
		.split('|')
		.map((cell) => cell.trim());
}

function parseAlignmentRow(line: string): ('left' | 'center' | 'right')[] {
	return splitTableRow(line).map((cell) => {
		const trimmed = cell.replace(/\s/g, '');
		if (trimmed.startsWith(':') && trimmed.endsWith(':')) return 'center';
		if (trimmed.endsWith(':')) return 'right';
		return 'left';
	});
}

function isBlockBoundary(line: string): boolean {
	if (!line.trim()) return true;
	return (
		/^#{1,6}\s/.test(line) ||
		/^```/.test(line) ||
		/^>\s?/.test(line) ||
		/^[-*+]\s/.test(line) ||
		/^\d+[.)]\s/.test(line) ||
		/^\|.+\|$/.test(line.trim()) ||
		/^!\[/.test(line.trim()) ||
		/^[-*_]{3,}$/.test(line.trim())
	);
}

export function renderMdsvex(source?: string): string {
	if (!source) return '';

	const lines = source.replaceAll('\r\n', '\n').split('\n');
	const html: string[] = [];
	let i = 0;

	while (i < lines.length) {
		const line = lines[i];
		const trimmed = line.trim();

		if (!trimmed) {
			i += 1;
			continue;
		}

		if (trimmed.startsWith('```')) {
			const lang = trimmed.slice(3).trim();
			const codeLines: string[] = [];
			i += 1;
			while (i < lines.length && !lines[i].trim().startsWith('```')) {
				codeLines.push(lines[i]);
				i += 1;
			}
			if (i < lines.length) i += 1;
			const languageAttr = lang ? ` class="language-${escapeHtml(lang)}"` : '';
			html.push(`<pre><code${languageAttr}>${escapeHtml(codeLines.join('\n'))}</code></pre>`);
			continue;
		}

		const headingMatch = line.match(/^(#{1,6})\s+(.*)$/);
		if (headingMatch) {
			const level = headingMatch[1].length;
			const text = headingMatch[2].trim();
			const id = slugify(text);
			html.push(`<h${level} id="${id}">${renderInline(text)}</h${level}>`);
			i += 1;
			continue;
		}

		// Horizontal rule
		if (/^[-*_]{3,}$/.test(trimmed)) {
			html.push('<hr />');
			i += 1;
			continue;
		}

		// Block-level image: standalone ![alt](url) line
		const blockImageMatch = trimmed.match(/^!\[([^\]]*)\]\(([^)]+)\)$/);
		if (blockImageMatch) {
			const alt = blockImageMatch[1];
			const safeUrl = sanitizeUrl(blockImageMatch[2]);
			let figure = `<figure><img src="${escapeHtml(safeUrl)}" alt="${escapeHtml(alt)}" loading="lazy" style="height:auto" />`;
			if (alt) {
				figure += `<figcaption>${escapeHtml(alt)}</figcaption>`;
			}
			figure += '</figure>';
			html.push(figure);
			i += 1;
			continue;
		}

		// GFM table: current line is pipe row and next line is separator
		if (
			/^\|.+\|$/.test(trimmed) &&
			i + 1 < lines.length &&
			/^\|[\s:|-]+\|$/.test(lines[i + 1].trim())
		) {
			const headerCells = splitTableRow(trimmed);
			const alignments = parseAlignmentRow(lines[i + 1].trim());
			i += 2;

			const thCells = headerCells
				.map((cell, idx) => {
					const align = alignments[idx] ?? 'left';
					return `<th style="text-align:${align}">${renderInline(cell)}</th>`;
				})
				.join('');

			const bodyRows: string[] = [];
			while (i < lines.length && /^\|.+\|$/.test(lines[i].trim())) {
				const cells = splitTableRow(lines[i].trim());
				const tds = cells
					.map((cell, idx) => {
						const align = alignments[idx] ?? 'left';
						return `<td style="text-align:${align}">${renderInline(cell)}</td>`;
					})
					.join('');
				bodyRows.push(`<tr>${tds}</tr>`);
				i += 1;
			}

			html.push(
				`<table><thead><tr>${thCells}</tr></thead><tbody>${bodyRows.join('')}</tbody></table>`
			);
			continue;
		}

		if (/^>\s?/.test(line)) {
			const quoteLines: string[] = [];
			while (i < lines.length && /^>\s?/.test(lines[i])) {
				quoteLines.push(lines[i].replace(/^>\s?/, ''));
				i += 1;
			}

			// GitHub-style callouts: > [!NOTE], > [!WARNING], > [!TIP], > [!IMPORTANT], > [!CAUTION]
			const calloutMatch = quoteLines[0]?.match(/^\[!(NOTE|WARNING|TIP|IMPORTANT|CAUTION)\]\s*$/i);
			if (calloutMatch) {
				const type = calloutMatch[1].toLowerCase();
				// Matches Callout component: src/lib/components/ui/callout/callout.svelte
				const svgAttrs = 'xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"';
				const calloutConfig: Record<string, { border: string; bg: string; text: string; icon: string }> = {
					note: { border: 'border-info/25', bg: 'bg-info/10', text: 'text-info', icon: `<svg ${svgAttrs}><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>` },
					tip: { border: 'border-success/25', bg: 'bg-success/10', text: 'text-success', icon: `<svg ${svgAttrs}><path d="M21.174 6.812a1 1 0 0 0-3.986-3.987L3.842 16.174a2 2 0 0 0-.5.83l-1.321 4.352a.5.5 0 0 0 .623.622l4.353-1.32a2 2 0 0 0 .83-.497z"/></svg>` },
					important: { border: 'border-primary/25', bg: 'bg-primary/10', text: 'text-primary', icon: `<svg ${svgAttrs}><circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>` },
					warning: { border: 'border-warning/25', bg: 'bg-warning/10', text: 'text-warning', icon: `<svg ${svgAttrs}><path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/></svg>` },
					caution: { border: 'border-destructive/25', bg: 'bg-destructive/10', text: 'text-destructive', icon: `<svg ${svgAttrs}><circle cx="12" cy="12" r="10"/><path d="m15 9-6 6"/><path d="m9 9 6 6"/></svg>` }
				};
				const c = calloutConfig[type] ?? calloutConfig.note;
				const body = quoteLines.slice(1).filter((l) => l.trim() !== '');
				const bodyHtml = renderInline(body.join('\n')).replaceAll('\n', '<br />');
				html.push(
					`<div class="flex gap-3 rounded-xl border ${c.border} ${c.bg} p-4">` +
					`<div class="shrink-0 pt-0.5 ${c.text}">${c.icon}</div>` +
					`<div class="min-w-0 flex-1">` +
					`<div class="prose-base text-foreground/90">${bodyHtml}</div></div></div>`
				);
			} else {
				html.push(
					`<blockquote>${renderInline(quoteLines.join('\n')).replaceAll('\n', '<br />')}</blockquote>`
				);
			}
			continue;
		}

		if (/^[-*+]\s/.test(line)) {
			const items: string[] = [];
			while (i < lines.length && /^[-*+]\s/.test(lines[i])) {
				items.push(`<li>${renderInline(lines[i].replace(/^[-*+]\s/, '').trim())}</li>`);
				i += 1;
			}
			html.push(`<ul>${items.join('')}</ul>`);
			continue;
		}

		if (/^\d+[.)]\s/.test(line)) {
			const items: string[] = [];
			while (i < lines.length && /^\d+[.)]\s/.test(lines[i])) {
				items.push(`<li>${renderInline(lines[i].replace(/^\d+[.)]\s/, '').trim())}</li>`);
				i += 1;
			}
			html.push(`<ol>${items.join('')}</ol>`);
			continue;
		}

		const paragraphLines: string[] = [];
		while (i < lines.length && !isBlockBoundary(lines[i])) {
			paragraphLines.push(lines[i].trim());
			i += 1;
		}
		// If no lines were collected (unhandled block boundary), consume the line to avoid infinite loop
		if (paragraphLines.length === 0) {
			paragraphLines.push(lines[i].trim());
			i += 1;
		}
		html.push(`<p>${renderInline(paragraphLines.join('\n')).replaceAll('\n', '<br />')}</p>`);
	}

	return html.join('\n');
}
