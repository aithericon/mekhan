<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import FileIcon from '@lucide/svelte/icons/file';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import type { RendererProps } from './types';

	type FileRef = {
		url: string;
		filename?: string;
		content_type?: string;
		size?: number;
	};

	let { value }: RendererProps = $props();
	const ref = $derived(value as FileRef);

	const display = $derived(ref.filename ?? ref.url.split('/').pop() ?? ref.url);

	function formatBytes(b: number | undefined): string | null {
		if (b === undefined || b === null) return null;
		if (b < 1024) return `${b} B`;
		if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} KB`;
		return `${(b / (1024 * 1024)).toFixed(1)} MB`;
	}

	const sizeLabel = $derived(formatBytes(ref.size));
</script>

<a
	href={ref.url}
	target="_blank"
	rel="noopener noreferrer"
	class="group inline-flex items-center gap-2 rounded-md border border-border bg-muted/30 px-3 py-2 text-sm transition-colors hover:bg-accent hover:text-accent-foreground"
	title={ref.url}
>
	<FileIcon class="size-4 shrink-0 text-muted-foreground group-hover:text-foreground" />
	<span class="truncate font-medium">{display}</span>
	{#if ref.content_type}
		<Badge variant="outline" class="font-mono text-sm">{ref.content_type}</Badge>
	{/if}
	{#if sizeLabel}
		<span class="text-sm text-muted-foreground">{sizeLabel}</span>
	{/if}
	<ExternalLink class="size-3.5 shrink-0 text-muted-foreground group-hover:text-foreground" />
</a>
