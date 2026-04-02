<script lang="ts">
	import type { CatalogueEntry } from '$lib/types/catalogue';
	import { catalogueDownloadUrl } from '$lib/api/client';
	import { Badge } from '$lib/components/ui/badge';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Download from '@lucide/svelte/icons/download';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	let {
		entry,
		expanded = false,
		highlighted = false,
		onToggle,
		onSchemaClick,
		onNetClick
	}: {
		entry: CatalogueEntry;
		expanded?: boolean;
		highlighted?: boolean;
		onToggle?: () => void;
		onSchemaClick?: (digest: string) => void;
		onNetClick?: (net: string) => void;
	} = $props();

	const categoryColors: Record<string, string> = {
		model: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		dataset: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		plot: 'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		log: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-300',
		checkpoint: 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200',
		config: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200',
		metric: 'bg-rose-100 text-rose-800 dark:bg-rose-900 dark:text-rose-200',
		other: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300'
	};

	function formatBytes(bytes: number | null): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
	}

	const formatDate = (s: string) =>
		new Intl.DateTimeFormat(undefined, {
			year: 'numeric', month: 'short', day: 'numeric',
			hour: '2-digit', minute: '2-digit'
		}).format(new Date(s));

	function catColor(cat: string): string {
		return categoryColors[cat.toLowerCase()] ?? categoryColors.other;
	}

	const fm = $derived(entry.file_metadata as Record<string, unknown>);
	const schema = $derived(fm?.schema_fingerprint as { digest: string; version: number } | undefined);
	const checksum = $derived(fm?.checksum as { algorithm: string; digest: string } | undefined);
	const columns = $derived(fm?.columns as { name: string; data_type: unknown }[] | undefined);
	const lineageTarget = $derived(entry.process_id ?? entry.job_id?.split(':')[0] ?? null);
	const hasDetails = $derived(
		(columns && columns.length > 0) ||
		Object.keys(entry.user_metadata).length > 0 ||
		schema || checksum
	);
</script>

<div
	class="rounded-lg border bg-card transition-colors {highlighted ? 'border-primary ring-1 ring-primary/30' : 'border-border hover:bg-accent/30'}"
>
	<!-- Header row -->
	<div class="flex items-start justify-between gap-4 p-4">
		<div class="min-w-0 flex-1">
			<!-- Name + badges -->
			<div class="flex flex-wrap items-center gap-1.5">
				<span class="text-sm font-medium text-foreground truncate">{entry.name}</span>
				<Badge class={catColor(entry.category)} variant="secondary">{entry.category}</Badge>
				{#if fm?.format}
					<Badge variant="outline" class="text-[10px] font-mono">{fm.format}</Badge>
				{:else if entry.mime_type}
					<Badge variant="outline" class="text-[10px] font-mono">{entry.mime_type}</Badge>
				{/if}
				{#if entry.job_id}
					<span class="text-[10px] font-mono text-muted-foreground">{entry.job_id}</span>
				{/if}
			</div>

			<!-- Key info row -->
			<div class="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-sm text-muted-foreground">
				<span>{formatDate(entry.created_at)}</span>
				<span class="font-medium tabular-nums text-foreground">{formatBytes(entry.size_bytes)}</span>
				{#if entry.source_net}
					{#if onNetClick}
						<button
							class="hover:text-primary hover:underline underline-offset-2"
							onclick={() => onNetClick(entry.source_net!)}
						>Net: <span class="font-mono">{entry.source_net}</span></button>
					{:else}
						<span>Net: <span class="font-mono">{entry.source_net}</span></span>
					{/if}
				{/if}
			</div>
		</div>

		<!-- Actions -->
		<div class="flex shrink-0 items-center gap-1">
			{#if lineageTarget}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<a
							href="/catalogue/lineage/{lineageTarget}?artifact={encodeURIComponent(entry.id)}"
							class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						>
							<GitBranch class="size-4" />
						</a>
					</Tooltip.Trigger>
					<Tooltip.Content>View lineage</Tooltip.Content>
				</Tooltip.Root>
			{/if}

			{#if entry.storage_path}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<a
							href={catalogueDownloadUrl(entry.storage_path)}
							class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
							download
						>
							<Download class="size-4" />
						</a>
					</Tooltip.Trigger>
					<Tooltip.Content>Download</Tooltip.Content>
				</Tooltip.Root>
			{/if}

			{#if hasDetails && onToggle}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<button
							class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
							onclick={onToggle}
						>
							{#if expanded}
								<ChevronDown class="size-4" />
							{:else}
								<ChevronRight class="size-4" />
							{/if}
						</button>
					</Tooltip.Trigger>
					<Tooltip.Content>{expanded ? 'Hide' : 'Show'} details</Tooltip.Content>
				</Tooltip.Root>
			{/if}
		</div>
	</div>

	<!-- Expanded details -->
	{#if expanded && hasDetails}
		<div class="border-t border-border px-4 pb-4 pt-3 space-y-3">
			<!-- Identifiers & provenance -->
			<div class="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
				{#if schema?.digest}
					<button
						class="font-mono hover:text-primary hover:underline underline-offset-2"
						onclick={() => onSchemaClick?.(schema!.digest)}
						title="Filter by this schema fingerprint"
					>
						Schema: {schema.digest}
					</button>
				{/if}
				{#if entry.correlation_id}
					<span>Correlation: <span class="font-mono">{entry.correlation_id}</span></span>
				{/if}
				{#if fm?.num_rows != null}
					<span>{fm.num_rows} row{fm.num_rows === 1 ? '' : 's'} · {fm?.num_columns ?? '?'} col{(fm?.num_columns ?? 0) === 1 ? '' : 's'}</span>
				{/if}
			</div>

			<!-- Storage path -->
			{#if entry.storage_path}
				<div class="text-sm font-mono text-muted-foreground break-all">
					{entry.storage_path}
				</div>
			{/if}

			<!-- Schema columns -->
			{#if columns && columns.length > 0}
				<div>
					<p class="mb-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground">
						Schema ({columns.length} columns)
					</p>
					<div class="flex flex-wrap gap-1">
						{#each columns as col}
							<span class="inline-flex items-center gap-1 rounded border border-border bg-muted/50 px-1.5 py-0.5 text-xs">
								<span class="font-medium text-foreground">{col.name}</span>
								<span class="text-muted-foreground">{typeof col.data_type === 'string' ? col.data_type : JSON.stringify(col.data_type)}</span>
							</span>
						{/each}
					</div>
				</div>
			{/if}

			<!-- User metadata -->
			{#if Object.keys(entry.user_metadata).length > 0}
				<div>
					<p class="mb-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
						User metadata
					</p>
					<pre class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-[11px] text-foreground">{JSON.stringify(entry.user_metadata, null, 2)}</pre>
				</div>
			{/if}
		</div>
	{/if}
</div>
