<script lang="ts">
	import { catalogueDownloadUrl, type CatalogueEntry } from '$lib/api/client';
	import type { DataCopy } from '$lib/api/data';
	import { Badge } from '$lib/components/ui/badge';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Download from '@lucide/svelte/icons/download';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Workflow from '@lucide/svelte/icons/workflow';
	import Activity from '@lucide/svelte/icons/activity';
	import Server from '@lucide/svelte/icons/server';
	import Star from '@lucide/svelte/icons/star';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	// File-type icons
	import File from '@lucide/svelte/icons/file';
	import FileText from '@lucide/svelte/icons/file-text';
	import Image from '@lucide/svelte/icons/image';
	import Table from '@lucide/svelte/icons/table';
	import Braces from '@lucide/svelte/icons/braces';
	import Dna from '@lucide/svelte/icons/dna';
	import ScrollText from '@lucide/svelte/icons/scroll-text';
	import Database from '@lucide/svelte/icons/database';
	import Box from '@lucide/svelte/icons/box';
	import Archive from '@lucide/svelte/icons/archive';
	import Save from '@lucide/svelte/icons/save';
	import Gauge from '@lucide/svelte/icons/gauge';
	import LineChart from '@lucide/svelte/icons/line-chart';
	import Settings2 from '@lucide/svelte/icons/settings-2';
	import Music from '@lucide/svelte/icons/music';
	import Film from '@lucide/svelte/icons/film';
	import DetailTable from './DetailTable.svelte';

	let {
		entry,
		expanded = false,
		highlighted = false,
		copies = [],
		onToggle,
		onSchemaClick,
		onNetClick,
		onViewServer
	}: {
		entry: CatalogueEntry;
		expanded?: boolean;
		highlighted?: boolean;
		/** Physical copies of this entry's content (unified Data browser). */
		copies?: DataCopy[];
		onToggle?: () => void;
		onSchemaClick?: (digest: string) => void;
		onNetClick?: (net: string) => void;
		/** Jump to a file server (Servers tab) by its inventory key. */
		onViewServer?: (key: string) => void;
	} = $props();

	// ── Category → colour + soft pill (icon-led, no loud filled badge) ──────────
	const catStyle: Record<string, { fg: string; pill: string; icon: typeof File }> = {
		dataset:    { fg: 'text-emerald-500', pill: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400', icon: Database },
		model:      { fg: 'text-blue-500',    pill: 'bg-blue-500/10 text-blue-600 dark:text-blue-400',          icon: Box },
		plot:       { fg: 'text-violet-500',  pill: 'bg-violet-500/10 text-violet-600 dark:text-violet-400',    icon: LineChart },
		log:        { fg: 'text-slate-500',   pill: 'bg-slate-500/10 text-slate-600 dark:text-slate-400',       icon: ScrollText },
		checkpoint: { fg: 'text-orange-500',  pill: 'bg-orange-500/10 text-orange-600 dark:text-orange-400',    icon: Save },
		config:     { fg: 'text-cyan-500',    pill: 'bg-cyan-500/10 text-cyan-600 dark:text-cyan-400',          icon: Settings2 },
		metric:     { fg: 'text-rose-500',    pill: 'bg-rose-500/10 text-rose-600 dark:text-rose-400',          icon: Gauge },
		legacy:     { fg: 'text-amber-500',   pill: 'bg-amber-500/10 text-amber-600 dark:text-amber-400',       icon: Archive },
		other:      { fg: 'text-slate-500',   pill: 'bg-slate-500/10 text-slate-600 dark:text-slate-400',       icon: File }
	};
	const cat = $derived(catStyle[entry.category?.toLowerCase()] ?? catStyle.other);

	// ── Copy status colours ─────────────────────────────────────────────────────
	const statusColors: Record<string, string> = {
		indexed: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300',
		verified: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		registered: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		copied: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200',
		deleted: 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400',
		mismatch: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
	};
	const copyStatusColor = (s: string) =>
		statusColors[s] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';

	// ── Helpers ─────────────────────────────────────────────────────────────────
	function formatBytes(bytes: number | null | undefined): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
	}

	const fullDate = (s: string) =>
		new Intl.DateTimeFormat(undefined, {
			year: 'numeric', month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit'
		}).format(new Date(s));

	function relTime(s: string): string {
		const diff = Date.now() - new Date(s).getTime();
		if (diff < 0) return fullDate(s);
		if (diff < 60_000) return 'just now';
		if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
		if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
		if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)}d ago`;
		return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', year: 'numeric' }).format(new Date(s));
	}

	// unix_mode (e.g. 33188) → symbolic perms ("rw-r--r--").
	function symbolicMode(mode: number): string {
		const p = mode & 0o777;
		const bit = (n: number, r: string) => ((p >> n) & 1 ? r : '-');
		return [
			bit(8, 'r'), bit(7, 'w'), bit(6, 'x'),
			bit(5, 'r'), bit(4, 'w'), bit(3, 'x'),
			bit(2, 'r'), bit(1, 'w'), bit(0, 'x')
		].join('');
	}

	function fileExt(name: string): string {
		const m = name.match(/\.([a-z0-9]+)$/i);
		return m ? m[1].toLowerCase() : '';
	}

	// File-type icon: format family (authoritative, from the probe) first, then
	// an extension/format-name heuristic, then category.
	function fileIcon(): typeof File {
		switch (family) {
			case 'image': return Image;
			case 'audio': return Music;
			case 'video': return Film;
			case 'tabular':
			case 'spreadsheet': return Table;
			case 'archive': return Archive;
			case 'config': return Settings2;
			case 'scientific':
			case 'mesh': return Box;
		}
		const ext = fileExt(entry.name);
		const key = ext || (formatLabel ?? '').toLowerCase();
		if (['svg', 'png', 'jpg', 'jpeg', 'gif', 'webp'].includes(key)) return Image;
		if (['csv', 'tsv', 'parquet', 'arrow', 'xlsx'].includes(key) || numRows != null) return Table;
		if (['json', 'xml', 'yaml', 'yml', 'toml'].includes(key)) return Braces;
		if (['fasta', 'fastq', 'fa', 'gb', 'genome', 'vcf'].includes(key)) return Dna;
		if (['log'].includes(key)) return ScrollText;
		if (['txt', 'md', 'rst'].includes(key)) return FileText;
		return cat.icon;
	}

	// ── Derived data ────────────────────────────────────────────────────────────
	// The normalized, UI-facing probe metadata (built server-side in
	// catalogue/metadata_view.rs). `null` for rows whose `file_metadata` couldn't
	// be parsed as fmeta::FileMetadata (empty / legacy / pre-probe) — the card
	// then degrades to size + content hash.
	const mv = $derived(entry.metadata_view ?? null);
	const um = $derived((entry.user_metadata ?? {}) as Record<string, unknown>);
	const formatLabel = $derived(mv?.format ?? null);
	const family = $derived(mv?.family ?? null);
	const schema = $derived(mv?.schema_fingerprint ?? null);
	const columns = $derived(mv?.columns ?? []);
	const details = $derived(mv?.details ?? null);
	// Dims that duplicate a chip shown elsewhere (rows×cols, image width/height in
	// details) add noise; surface only the interesting ones (z/y/x, lat/lon, …).
	const REDUNDANT_DIMS = new Set(['rows', 'columns', 'width', 'height']);
	const dims = $derived((mv?.dimensions ?? []).filter((d) => !REDUNDANT_DIMS.has(d.name)));
	const attributes = $derived(mv?.attributes ?? []);
	const preview = $derived(mv?.preview ?? null);
	const dataQuality = $derived(mv?.data_quality ?? null);
	const numRows = $derived(mv?.num_rows ?? null);
	const numCols = $derived(mv?.num_columns ?? null);
	const unixMode = $derived(mv?.unix_mode ?? null);
	const modifiedAt = $derived(mv?.modified_at ?? null);
	const pct = (n: number) => `${Math.round(n * 100)}%`;

	const contentHash = $derived(entry.content_hash ?? null);
	const shortHash = $derived(contentHash ? contentHash.slice(0, 10) : null);
	// job_id == execution_id for job-net rows; collapse to one "Execution" id.
	const executionId = $derived(entry.execution_id || entry.job_id || null);
	const netShort = $derived(entry.source_net ? entry.source_net.replace(/^mekhan-/, '').slice(0, 8) : null);
	const lineageTarget = $derived(entry.process_id ?? entry.job_id?.split(':')[0] ?? null);
	// The producing workflow instance: net_id is `mekhan-{instance_uuid}`, and
	// `source_net` IS the net_id. Fall back to execution_id (= net_id + a run
	// suffix), taking the leading UUID. /instances/{id} → process view.
	const instanceId = $derived(
		entry.source_net
			? entry.source_net.replace(/^mekhan-/, '')
			: entry.execution_id
				? entry.execution_id.replace(/^mekhan-/, '').split('-').slice(0, 5).join('-')
				: null
	);
	const Icon = $derived(fileIcon());

	// The canonical (or first) physical copy — surfaced in the collapsed row.
	const primaryCopy = $derived(copies.find((c) => c.is_canonical) ?? copies[0] ?? null);

	const hasDetails = $derived(
		copies.length > 0 ||
		!!contentHash || !!entry.entry_id || !!executionId || !!entry.source_net ||
		!!entry.signal_key || !!entry.process_step || !!schema ||
		columns.length > 0 || !!details || numRows != null || dims.length > 0 ||
		attributes.length > 0 || !!preview || !!dataQuality || unixMode != null ||
		!!entry.storage_path || Object.keys(um).length > 0
	);
	const canToggle = $derived(hasDetails && !!onToggle);

	function toggle() {
		if (canToggle) onToggle?.();
	}
</script>

<div
	class="group overflow-hidden rounded-lg border bg-card transition-colors {highlighted
		? 'border-primary ring-1 ring-primary/30'
		: 'border-border'} {!expanded ? 'hover:bg-accent/30' : ''}"
>
	<!-- Header (whole region toggles when collapsible) -->
	<div class="flex items-start gap-3 p-3.5">
		<!-- File-type icon -->
		<div class="mt-0.5 shrink-0 {cat.fg}">
			<Icon class="size-5" />
		</div>

		<!-- Title + metadata breadcrumb -->
		<div class="min-w-0 flex-1">
			<!-- Line 1: name · category · format — the toggle target -->
			<div
				class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 {canToggle ? 'cursor-pointer' : ''}"
				onclick={toggle}
				onkeydown={(e) => { if (canToggle && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); toggle(); } }}
				role="button"
				tabindex={canToggle ? 0 : -1}
			>
				<span class="truncate text-sm font-semibold text-foreground" title={entry.name}>{entry.name}</span>
				<span class="shrink-0 rounded px-1.5 py-0.5 text-[11px] font-medium capitalize {cat.pill}">{entry.category}</span>
				{#if formatLabel}
					<span class="shrink-0 font-mono text-[11px] uppercase text-muted-foreground">{formatLabel}</span>
				{/if}
			</div>

			<!-- Line 2: size · time · location · hash (muted breadcrumb; chips are individually interactive) -->
			<div class="mt-1 flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1 text-xs text-muted-foreground">
				{#if entry.size_bytes != null}
					<span class="font-medium tabular-nums text-foreground/80">{formatBytes(entry.size_bytes)}</span>
				{/if}
				{#if numRows != null}
					<span class="tabular-nums">{numRows.toLocaleString()} × {numCols ?? '?'}</span>
				{/if}
				<Tooltip.Root>
					<Tooltip.Trigger class="cursor-default">{relTime(entry.created_at)}</Tooltip.Trigger>
					<Tooltip.Content>Created {fullDate(entry.created_at)}</Tooltip.Content>
				</Tooltip.Root>

				{#if primaryCopy}
					<span class="text-border">·</span>
					<button
						class="inline-flex max-w-[14rem] items-center gap-1 truncate hover:text-foreground"
						onclick={() => onViewServer?.(primaryCopy.file_server_id)}
						title="On {primaryCopy.server_display_name ?? primaryCopy.file_server_id}"
					>
						<Server class="size-3 shrink-0" />
						<span class="truncate">{primaryCopy.server_display_name ?? primaryCopy.file_server_id}</span>
					</button>
					{#if copies.length > 1}
						<span class="text-foreground/70">+{copies.length - 1}</span>
					{/if}
				{/if}

				{#if netShort}
					<span class="text-border">·</span>
					{#if onNetClick}
						<button
							class="font-mono hover:text-foreground"
							onclick={() => onNetClick?.(entry.source_net!)}
							title="Filter by net {entry.source_net}"
						>net:{netShort}</button>
					{:else}
						<span class="font-mono" title={entry.source_net ?? ''}>net:{netShort}</span>
					{/if}
				{/if}

				{#if shortHash}
					<span class="text-border">·</span>
					<span class="inline-flex items-center gap-1 font-mono" title="Content hash (SHA-256)">
						{shortHash}
						<span class="opacity-0 transition-opacity group-hover:opacity-100">
							<CopyButton text={contentHash ?? ''} title="Copy content hash" iconClass="size-3" />
						</span>
					</span>
				{/if}
			</div>
		</div>

		<!-- Actions -->
		<div class="flex shrink-0 items-center gap-0.5">
			{#if instanceId}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<a
							href="/instances/{instanceId}/process"
							class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						>
							<Activity class="size-4" />
						</a>
					</Tooltip.Trigger>
					<Tooltip.Content>Open instance</Tooltip.Content>
				</Tooltip.Root>
			{/if}

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

			{#if executionId}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<a
							href="/catalogue/provenance/{encodeURIComponent(entry.execution_id)}/{encodeURIComponent(entry.id)}"
							class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						>
							<Workflow class="size-4" />
						</a>
					</Tooltip.Trigger>
					<Tooltip.Content>Trace provenance</Tooltip.Content>
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

			{#if canToggle}
				<button
					class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
					onclick={toggle}
					aria-label={expanded ? 'Hide details' : 'Show details'}
				>
					<ChevronDown class="size-4 transition-transform duration-200 {expanded ? 'rotate-180' : ''}" />
				</button>
			{/if}
		</div>
	</div>

	<!-- Expanded details -->
	{#if expanded && hasDetails}
		<div class="space-y-4 border-t border-border bg-muted/20 px-4 py-3.5">
			<!-- Location: where the bytes physically live -->
			{#if copies.length > 0 || entry.storage_path}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Location</h4>
					<div class="space-y-1.5">
						{#each copies as c}
							<div class="flex items-center gap-2 text-sm">
								{#if c.is_canonical}
									<Tooltip.Root>
										<Tooltip.Trigger><Star class="size-3.5 shrink-0 fill-amber-400 text-amber-400" /></Tooltip.Trigger>
										<Tooltip.Content>Canonical copy</Tooltip.Content>
									</Tooltip.Root>
								{:else}
									<span class="size-3.5 shrink-0"></span>
								{/if}
								{#if onViewServer}
									<button
										class="inline-flex shrink-0 items-center gap-1 font-medium text-foreground hover:text-primary"
										onclick={() => onViewServer?.(c.file_server_id)}
										title="View server {c.file_server_id}"
									>
										<Server class="size-3.5" />{c.server_display_name ?? c.file_server_id}
									</button>
								{:else}
									<span class="inline-flex shrink-0 items-center gap-1 font-medium text-foreground">
										<Server class="size-3.5" />{c.server_display_name ?? c.file_server_id}
									</span>
								{/if}
								{#if c.server_kind}<span class="shrink-0 font-mono text-[10px] uppercase text-muted-foreground">{c.server_kind}</span>{/if}
								<span class="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground" title={c.path}>{c.path}</span>
								<Badge class="{copyStatusColor(c.status)} shrink-0" variant="secondary">{c.status}</Badge>
							</div>
						{/each}
						{#if entry.storage_path}
							<div class="flex items-center gap-2 text-sm">
								<span class="size-3.5 shrink-0"></span>
								<span class="inline-flex shrink-0 items-center gap-1 font-medium text-foreground">
									<Database class="size-3.5" />Platform store
								</span>
								<span class="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground" title={entry.storage_path}>{entry.storage_path}</span>
							</div>
						{/if}
					</div>
				</section>
			{/if}

			<!-- Format & schema -->
			{#if formatLabel || schema || details || numRows != null || columns.length > 0 || dims.length > 0 || unixMode != null}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Format &amp; schema</h4>
					<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
						{#if formatLabel}
							<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs">{formatLabel}</span>
						{/if}
						{#if numRows != null}
							<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs tabular-nums">{numRows.toLocaleString()} rows × {numCols ?? '?'} cols</span>
						{/if}
						{#each dims as d}
							<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
								<span class="text-muted-foreground">{d.name}:</span>
								<span class="font-medium text-foreground tabular-nums">{d.size != null ? d.size.toLocaleString() : '∞'}</span>
							</span>
						{/each}
						{#if details}
							{#each details.fields ?? [] as f}
								<span class="rounded border border-border bg-background px-1.5 py-0.5 text-xs">
									<span class="text-muted-foreground">{f.label}:</span>
									<span class="font-medium text-foreground">{f.value}{f.unit ? ` ${f.unit}` : ''}</span>
								</span>
							{/each}
						{/if}
						{#if unixMode != null}
							<span class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs" title="unix mode {unixMode}">{symbolicMode(unixMode)}</span>
						{/if}
						{#if schema?.digest}
							<button
								class="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-xs hover:border-primary hover:text-primary"
								onclick={() => onSchemaClick?.(schema!.digest)}
								title="Filter by this schema fingerprint (v{schema.version})"
							>schema {schema.digest}</button>
						{/if}
					</div>

					{#if columns.length > 0}
						<div class="mt-2 flex flex-wrap gap-1">
							{#each columns as col}
								<span class="inline-flex items-center gap-1 rounded border border-border bg-background px-1.5 py-0.5 text-xs">
									<span class="font-medium text-foreground">{col.name}</span>
									<span class="text-muted-foreground">{col.data_type}{col.nullable ? '?' : ''}</span>
									{#each col.classifications ?? [] as tag}
										<span class="rounded-sm bg-amber-500/10 px-1 text-[10px] text-amber-600 dark:text-amber-400" title="{pct(tag.confidence)} confidence">{tag.category}</span>
									{/each}
								</span>
							{/each}
						</div>
					{/if}

					{#if details}
						{#each details.tables ?? [] as t}
							<DetailTable title={t.title} columns={t.columns} rows={t.rows} />
						{/each}
					{/if}
				</section>
			{/if}

			<!-- Preview (first rows of tabular data) -->
			{#if preview && preview.rows.length > 0}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
						Preview
						{#if preview.total_row_count != null}
							<span class="ml-1 font-normal normal-case text-muted-foreground/70">of {preview.total_row_count.toLocaleString()} rows</span>
						{/if}
					</h4>
					<DetailTable columns={preview.columns} rows={preview.rows} />
				</section>
			{/if}

			<!-- Data quality -->
			{#if dataQuality}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Data quality</h4>
					<div class="flex flex-wrap items-center gap-x-2 gap-y-1.5 text-xs">
						<span class="rounded border border-border bg-background px-1.5 py-0.5">
							<span class="text-muted-foreground">completeness:</span>
							<span class="font-medium text-foreground tabular-nums">{pct(dataQuality.completeness)}</span>
						</span>
					</div>
					{#if (dataQuality.columns ?? []).length > 0}
						<DetailTable
							columns={['column', 'completeness', 'distinctness', 'score']}
							rows={(dataQuality.columns ?? []).map((c) => [c.column_name, pct(c.completeness), pct(c.distinctness), pct(c.score)])}
						/>
					{/if}
				</section>
			{/if}

			<!-- File attributes (author, conventions, …) -->
			{#if attributes.length > 0}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Attributes</h4>
					<dl class="grid grid-cols-[10rem_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
						{#each attributes as a}
							<dt class="truncate text-muted-foreground" title={a.key}>{a.key}</dt>
							<dd class="min-w-0 truncate font-mono text-foreground" title={a.value}>{a.value}</dd>
						{/each}
					</dl>
				</section>
			{/if}

			<!-- Identity + provenance (a compact definition list of the IDs) -->
			<section>
				<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Identity &amp; provenance</h4>
				<dl class="grid grid-cols-[7rem_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-sm">
					{#if contentHash}
						<dt class="text-muted-foreground">Content hash</dt>
						<dd class="flex min-w-0 items-center gap-1.5">
							<span class="truncate font-mono text-foreground" title={contentHash}>{contentHash}</span>
							<span class="shrink-0 text-[10px] uppercase text-muted-foreground">sha-256</span>
							<CopyButton text={contentHash} title="Copy content hash" iconClass="size-3" />
						</dd>
					{/if}
					{#if entry.entry_id}
						<dt class="text-muted-foreground">Entry ID</dt>
						<dd class="flex min-w-0 items-center gap-1.5">
							<span class="truncate font-mono text-xs text-muted-foreground" title={entry.entry_id}>{entry.entry_id}</span>
							<CopyButton text={entry.entry_id} title="Copy entry id" iconClass="size-3" />
						</dd>
					{/if}
					{#if entry.process_step}
						<dt class="text-muted-foreground">Step</dt>
						<dd class="min-w-0 truncate text-foreground">{entry.process_step}</dd>
					{/if}
					{#if instanceId}
						<dt class="text-muted-foreground">Instance</dt>
						<dd class="flex min-w-0 items-center gap-1.5">
							<a href="/instances/{instanceId}/process" class="inline-flex items-center gap-1 truncate font-mono text-xs hover:text-primary">
								<Activity class="size-3 shrink-0" />{instanceId}
							</a>
						</dd>
					{/if}
					{#if entry.source_net}
						<dt class="text-muted-foreground">Net</dt>
						<dd class="flex min-w-0 items-center gap-1.5">
							{#if onNetClick}
								<button class="truncate font-mono text-xs hover:text-primary" onclick={() => onNetClick?.(entry.source_net!)} title="Filter by net">{entry.source_net}</button>
							{:else}
								<span class="truncate font-mono text-xs text-muted-foreground">{entry.source_net}</span>
							{/if}
						</dd>
					{/if}
					{#if executionId}
						<dt class="text-muted-foreground">Execution</dt>
						<dd class="flex min-w-0 items-center gap-1.5">
							<span class="truncate font-mono text-xs text-muted-foreground" title={executionId}>{executionId}</span>
							<CopyButton text={executionId} title="Copy execution id" iconClass="size-3" />
						</dd>
					{/if}
					{#if entry.signal_key}
						<dt class="text-muted-foreground">Correlation</dt>
						<dd class="min-w-0 truncate font-mono text-xs text-muted-foreground" title={entry.signal_key}>{entry.signal_key}</dd>
					{/if}
					<dt class="text-muted-foreground">Created</dt>
					<dd class="min-w-0 text-foreground">{fullDate(entry.created_at)}</dd>
					{#if modifiedAt}
						<dt class="text-muted-foreground">Modified</dt>
						<dd class="min-w-0 text-muted-foreground">{fullDate(modifiedAt)}</dd>
					{/if}
				</dl>
			</section>

			<!-- User metadata -->
			{#if Object.keys(um).length > 0}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">User metadata</h4>
					<pre class="overflow-x-auto rounded-md border border-border bg-background px-3 py-2 text-xs text-foreground">{JSON.stringify(um, null, 2)}</pre>
				</section>
			{/if}
		</div>
	{/if}
</div>
