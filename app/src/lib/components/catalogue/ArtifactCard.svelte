<script lang="ts">
	import {
		catalogueDownloadUrl,
		dataEntryContentUrl,
		type CatalogueEntry,
		type LiveArtifactEntry
	} from '$lib/api/client';
	import { pickRenderer } from '$lib/components/process-live/renderers/registry';
	import JsonRenderer from '$lib/components/process-live/renderers/JsonRenderer.svelte';
	import { catalogueColumnsToSchemaNode, fileMetadataDataTypeToSchemaNode } from '$lib/schema/model';
	import type { SchemaNode } from '$lib/schema/model';
	import { pickMetadataRenderer } from './metadata/registry';
	import { instanceIdFromNet, instanceIdFromExecution } from '$lib/utils';
	import { copiesForHash, type DataCopy } from '$lib/api/data';
	import { StatusBadge } from '$lib/components/status';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import * as Dialog from '$lib/components/ui/dialog';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Download from '@lucide/svelte/icons/download';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Workflow from '@lucide/svelte/icons/workflow';
	import Activity from '@lucide/svelte/icons/activity';
	import Server from '@lucide/svelte/icons/server';
	import Star from '@lucide/svelte/icons/star';
	import Info from '@lucide/svelte/icons/info';
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
		copies = undefined,
		detailsOpen = undefined,
		onDetailsOpenChange,
		onSchemaClick,
		onNetClick,
		onViewServer
	}: {
		entry: CatalogueEntry;
		/**
		 * Render the detail sections inline, statically (no dialog, no toggle).
		 * For embedding contexts that are already an overlay (e.g. the
		 * provenance event sheet). List views leave this off — details live in
		 * a dialog opened from the row.
		 */
		expanded?: boolean;
		highlighted?: boolean;
		/**
		 * Physical copies of this entry's content (unified Data browser).
		 * Omit (undefined) to let the card fetch them itself by content hash —
		 * call-sites outside the Data browser (process artifacts, lineage,
		 * provenance) get the same Download affordance without plumbing copies.
		 */
		copies?: DataCopy[];
		/**
		 * Controlled open state for the details dialog (e.g. ?inspect= deep
		 * links). Omit for internal, uncontrolled state.
		 */
		detailsOpen?: boolean;
		onDetailsOpenChange?: (open: boolean) => void;
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
	const netShort = $derived(
		instanceIdFromNet(entry.source_net)?.slice(0, 8) ?? null
	);
	const lineageTarget = $derived(entry.process_id ?? entry.job_id?.split(':')[0] ?? null);
	// The producing workflow instance. Multi-tenancy made net_id
	// `mekhan-{ws}-{instance}`; `source_net` IS the net_id, so derive the bare
	// instance UUID from it. Fall back to execution_id (`mekhan-{ws}-{inst}-{run}`),
	// whose instance UUID is the second segment. /instances/{id} → process view.
	const instanceId = $derived(
		instanceIdFromNet(entry.source_net) ?? instanceIdFromExecution(entry.execution_id)
	);
	const Icon = $derived(fileIcon());

	// Self-fetched copies for call-sites that didn't pass the `copies` prop.
	let fetchedCopies = $state<DataCopy[] | null>(null);
	$effect(() => {
		if (copies !== undefined || !entry.content_hash) return;
		const hash = entry.content_hash;
		copiesForHash(hash).then((c) => {
			if (entry.content_hash === hash) fetchedCopies = c;
		});
	});
	const allCopies = $derived(copies ?? fetchedCopies ?? []);

	// The canonical (or first) physical copy — surfaced in the collapsed row.
	const primaryCopy = $derived(allCopies.find((c) => c.is_canonical) ?? allCopies[0] ?? null);

	// ── Media preview ───────────────────────────────────────────────────────────
	// The process-live renderer registry works off a LiveArtifactEntry; the card
	// holds a CatalogueEntry. Adapt the overlapping fields so the same media
	// renderers (image/video/audio/…) drive the catalogue detail preview.
	//
	// Two byte sources: platform-store artifacts carry a `storage_path`
	// (catalogueDownloadUrl). Crawled / by-reference entries have none — their
	// bytes live on a file server and are served by content hash, but only once
	// the server is ADOPTED so a servable copy exists. `content_url` carries that
	// resolved by-reference URL so the renderers can fetch either source.
	const byRefContentUrl = $derived(
		!entry.storage_path && entry.content_hash && allCopies.some((c) => c.servable)
			? dataEntryContentUrl(entry.content_hash)
			: null
	);
	const liveEntry = $derived.by((): LiveArtifactEntry => ({
		execution_id: entry.execution_id,
		name: entry.name,
		category: entry.category,
		filename: entry.filename,
		mime_type: entry.mime_type ?? null,
		storage_path: entry.storage_path ?? null,
		content_url: byRefContentUrl,
		size_bytes: entry.size_bytes ?? null,
		process_step: entry.process_step ?? null,
		signal_key: entry.signal_key ?? null,
		user_metadata: (entry.user_metadata ?? null) as Record<string, unknown> | null,
		created_at: entry.created_at,
		id: entry.id,
		artifact_id: entry.entry_id ?? entry.id
	}));

	// Pick a media renderer for the preview section. Needs a fetchable source —
	// a platform `storage_path` or a servable by-reference copy. Suppress raw
	// text/json dumps when a structured tabular sample (the `preview` rows) shows
	// below — otherwise the same bytes read twice (once as a dump, once as a table).
	const previewRenderer = $derived.by(() => {
		if (!entry.storage_path && !byRefContentUrl) return null;
		const r = pickRenderer(liveEntry);
		if (!r) return null;
		if (preview && /^(text\/|application\/json)/.test(entry.mime_type ?? '')) return null;
		return r;
	});

	// Per-record schema (column → type) recovered from the probe's raw nested
	// `DataType`s, fed to the JSON preview tree so its fields are type-annotated
	// rather than rendered as bare values. Null for legacy / non-record files.
	const jsonRecordSchema = $derived.by(() => {
		const fm = entry.file_metadata as { columns?: unknown } | null | undefined;
		return catalogueColumnsToSchemaNode(fm?.columns) ?? undefined;
	});

	// Per-column type trees (name → SchemaNode) from the same raw nested types,
	// so the Format & schema columns table can render struct/list types as an
	// expandable tree instead of a truncated `struct<…>` string.
	const columnSchemas = $derived.by((): Map<string, SchemaNode> | undefined => {
		const fm = entry.file_metadata as { columns?: unknown } | null | undefined;
		if (!Array.isArray(fm?.columns)) return undefined;
		const map = new Map<string, SchemaNode>();
		for (const col of fm.columns) {
			if (col && typeof col === 'object') {
				const c = col as Record<string, unknown>;
				if (typeof c.name === 'string') {
					map.set(c.name, fileMetadataDataTypeToSchemaNode(c.data_type));
				}
			}
		}
		return map.size > 0 ? map : undefined;
	});

	const hasDetails = $derived(
		allCopies.length > 0 ||
		!!contentHash || !!entry.entry_id || !!executionId || !!entry.source_net ||
		!!entry.process_step || !!schema ||
		columns.length > 0 || !!details || numRows != null || dims.length > 0 ||
		attributes.length > 0 || !!preview || !!dataQuality || unixMode != null ||
		!!entry.storage_path || Object.keys(um).length > 0
	);
	// Details live in a dialog unless the caller asked for static inline render.
	const canOpenDetails = $derived(hasDetails && !expanded);

	// Controlled (detailsOpen prop) with uncontrolled fallback.
	let internalOpen = $state(false);
	// Scroll container — focus target on open (see onOpenAutoFocus below).
	let scrollEl = $state<HTMLDivElement | null>(null);
	const dialogOpen = $derived(detailsOpen ?? internalOpen);
	function setDialogOpen(open: boolean) {
		if (detailsOpen === undefined) internalOpen = open;
		onDetailsOpenChange?.(open);
	}

	function openDetails() {
		if (canOpenDetails) setDialogOpen(true);
	}

	// Filter / navigation actions inside the dialog dismiss it — the result of
	// the action (filtered list, Servers tab) is behind the overlay.
	function handleSchemaClick(digest: string) {
		setDialogOpen(false);
		onSchemaClick?.(digest);
	}
	function handleNetClick(net: string) {
		setDialogOpen(false);
		onNetClick?.(net);
	}
	function handleViewServer(key: string) {
		setDialogOpen(false);
		onViewServer?.(key);
	}
</script>

<div
	class="group overflow-hidden rounded-lg border bg-card transition-colors {highlighted
		? 'border-primary ring-1 ring-primary/30'
		: 'border-border'} {!expanded ? 'hover:bg-accent/30' : ''}"
>
	<!-- Header (whole region opens the details dialog when available) -->
	<div class="flex items-start gap-3 p-3.5">
		<!-- File-type icon -->
		<div class="mt-0.5 shrink-0 {cat.fg}">
			<Icon class="size-5" />
		</div>

		<!-- Title + metadata breadcrumb -->
		<div class="min-w-0 flex-1">
			<!-- Line 1: name · category · format — opens the details dialog -->
			<div
				class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 {canOpenDetails ? 'cursor-pointer' : ''}"
				onclick={openDetails}
				onkeydown={(e) => { if (canOpenDetails && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); openDetails(); } }}
				role="button"
				tabindex={canOpenDetails ? 0 : -1}
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
					{#if allCopies.length > 1}
						<span class="text-foreground/70">+{allCopies.length - 1}</span>
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
			{:else if entry.content_hash && allCopies.some((c) => c.servable)}
				<!-- By-reference entry: no platform-store copy, but at least one
				     physical copy sits behind a servable endpoint — route it. -->
				<Tooltip.Root>
					<Tooltip.Trigger>
						<a
							href={dataEntryContentUrl(entry.content_hash)}
							class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
							download={entry.filename ?? entry.name ?? true}
						>
							<Download class="size-4" />
						</a>
					</Tooltip.Trigger>
					<Tooltip.Content>Download from file server</Tooltip.Content>
				</Tooltip.Root>
			{:else if entry.content_hash && allCopies.length > 0}
				<!-- Copies exist but no endpoint can deliver them: explain instead of
				     offering a dead click that would 409. -->
				<Tooltip.Root>
					<Tooltip.Trigger>
						<span
							class="inline-flex size-8 cursor-not-allowed items-center justify-center rounded-md text-muted-foreground/40"
							aria-disabled="true"
							data-testid="download-unservable"
						>
							<Download class="size-4" />
						</span>
					</Tooltip.Trigger>
					<Tooltip.Content>
						No servable endpoint for this file's server yet — adopt the server under Data → Servers
						(root + serve group are stamped automatically for crawled files), then Verify.
					</Tooltip.Content>
				</Tooltip.Root>
			{/if}

			{#if canOpenDetails}
				<Tooltip.Root>
					<Tooltip.Trigger
						class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
						onclick={openDetails}
						aria-label="Show details"
					>
						<Info class="size-4" />
					</Tooltip.Trigger>
					<Tooltip.Content>Details &amp; provenance</Tooltip.Content>
				</Tooltip.Root>
			{/if}
		</div>
	</div>

	<!-- Static inline details (embedding contexts that are already an overlay) -->
	{#if expanded && hasDetails}
		<div class="space-y-4 border-t border-border bg-muted/20 px-4 py-3.5">
			{@render detailSections()}
		</div>
	{/if}
</div>

<!-- Detail sections — rendered inline (static `expanded`) or in the dialog. -->
{#snippet detailSections()}
			<!-- Preview: media renderer (image/video/audio/…) for the stored bytes -->
			{#if previewRenderer}
				{@const R = previewRenderer}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Preview</h4>
					<div class="max-h-[26rem] overflow-auto rounded-md border border-border bg-background">
						{#if R === JsonRenderer}
							<JsonRenderer entry={liveEntry} schemaNode={jsonRecordSchema} />
						{:else}
							<R entry={liveEntry} />
						{/if}
					</div>
				</section>
			{/if}

			<!-- Location: where the bytes physically live -->
			{#if allCopies.length > 0 || entry.storage_path}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Location</h4>
					<div class="space-y-1.5">
						{#each allCopies as c}
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
										onclick={() => handleViewServer(c.file_server_id)}
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
								<StatusBadge domain="copy" status={c.status} class="shrink-0" />
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

			<!-- Format & schema — dispatched to a format-family-specific renderer -->
			{#if mv && (formatLabel || schema || details || numRows != null || columns.length > 0 || dims.length > 0 || unixMode != null)}
				{@const MetaRenderer = pickMetadataRenderer(mv)}
				<MetaRenderer {mv} {columnSchemas} onSchemaClick={handleSchemaClick} />
			{/if}

			<!-- Sample rows (first rows of tabular data) -->
			{#if preview && preview.rows.length > 0}
				<section>
					<h4 class="mb-2 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">
						Sample rows
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
								<button class="truncate font-mono text-xs hover:text-primary" onclick={() => handleNetClick(entry.source_net!)} title="Filter by net">{entry.source_net}</button>
							{:else}
								<span class="truncate font-mono text-xs text-muted-foreground">{entry.source_net}</span>
							{/if}
						</dd>
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
{/snippet}

<!-- Details & provenance dialog (list contexts) -->
{#if canOpenDetails}
	<Dialog.Root open={dialogOpen} onOpenChange={setDialogOpen}>
		<Dialog.Content
			class="flex max-h-[85vh] flex-col gap-0 overflow-hidden p-0 sm:max-w-3xl"
			onOpenAutoFocus={(e) => {
				// Default auto-focus lands on the first focusable child — the
				// canonical-copy Star's tooltip trigger — which then opens its
				// tooltip on focus. Redirect focus to the non-interactive scroll
				// container so focus still enters the dialog without that flash.
				e.preventDefault();
				scrollEl?.focus();
			}}
		>
			<Dialog.Header class="border-b border-border px-6 py-4">
				<Dialog.Title class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1 pr-8 text-left">
					<span class="shrink-0 {cat.fg}"><Icon class="size-5" /></span>
					<span class="min-w-0 truncate" title={entry.name}>{entry.name}</span>
					<span class="shrink-0 rounded px-1.5 py-0.5 text-[11px] font-medium capitalize {cat.pill}">{entry.category}</span>
					{#if formatLabel}
						<span class="shrink-0 font-mono text-[11px] uppercase text-muted-foreground">{formatLabel}</span>
					{/if}
				</Dialog.Title>
				<Dialog.Description class="text-left">
					{#if entry.size_bytes != null}{formatBytes(entry.size_bytes)} · {/if}created {fullDate(entry.created_at)}
				</Dialog.Description>
			</Dialog.Header>
			<div bind:this={scrollEl} tabindex="-1" class="space-y-4 overflow-y-auto px-6 py-4 outline-none">
				{@render detailSections()}
			</div>
		</Dialog.Content>
	</Dialog.Root>
{/if}
