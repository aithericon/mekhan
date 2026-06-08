<script lang="ts">
	import { listDataEntries, type DataEntry, type DataEntriesResponse } from '$lib/api/data';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import * as Select from '$lib/components/ui/select';
	import Search from '@lucide/svelte/icons/search';
	import Hash from '@lucide/svelte/icons/hash';
	import Star from '@lucide/svelte/icons/star';
	import Server from '@lucide/svelte/icons/server';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import FileQuestion from '@lucide/svelte/icons/file-question';
	import Database from '@lucide/svelte/icons/database';

	let { onViewServers }: { onViewServers: () => void } = $props();

	let resp = $state<DataEntriesResponse | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let page = $state(0);
	let search = $state('');
	let category = $state('all');
	let sort = $state('-created_at');
	let expanded = $state<Set<string>>(new Set());
	let showUncatalogued = $state(false);

	const categories = ['all', 'model', 'dataset', 'plot', 'log', 'checkpoint', 'config', 'metric', 'other', 'legacy'];
	const sortOptions = [
		{ value: '-created_at', label: 'Newest' },
		{ value: 'created_at', label: 'Oldest' },
		{ value: 'name', label: 'Name A-Z' },
		{ value: '-size_bytes', label: 'Largest' }
	];

	const statusColors: Record<string, string> = {
		indexed: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300',
		verified: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		registered: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		copied: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200'
	};
	const statusColor = (s: string) =>
		statusColors[s] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';

	function fmtSize(n: number | null | undefined): string {
		if (n == null) return '—';
		if (n < 1024) return `${n} B`;
		const u = ['KB', 'MB', 'GB', 'TB'];
		let v = n / 1024,
			i = 0;
		while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
		return `${v.toFixed(1)} ${u[i]}`;
	}
	const entryKey = (e: DataEntry) =>
		e.entry_id ?? e.content_hash ?? `${e.name}-${e.copies[0]?.path ?? ''}`;

	function toggle(k: string) {
		const n = new Set(expanded);
		n.has(k) ? n.delete(k) : n.add(k);
		expanded = n;
	}

	async function load(s: string, cat: string, srt: string, pg: number) {
		loading = true;
		error = null;
		try {
			resp = await listDataEntries({
				search: s.trim() || undefined,
				category: cat === 'all' ? undefined : cat,
				sort: srt,
				page: pg,
				page_size: 25
			});
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load entries';
			resp = null;
		} finally {
			loading = false;
		}
	}

	let debounce: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const s = search, cat = category, srt = sort, pg = page;
		clearTimeout(debounce);
		debounce = setTimeout(() => load(s, cat, srt, pg), 250);
		return () => clearTimeout(debounce);
	});
</script>

<!-- Filters -->
<div class="mb-4 flex flex-wrap items-center gap-2">
	<div class="relative min-w-[14rem] flex-1">
		<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
		<Input
			type="text"
			placeholder="Search name or content hash…"
			class="h-8 pl-8 text-sm"
			bind:value={search}
			oninput={() => (page = 0)}
			data-testid="data-search"
		/>
	</div>
	<Select.Root type="single" value={category} onValueChange={(v) => { category = v ?? 'all'; page = 0; }}>
		<Select.Trigger class="h-8 w-40 text-sm">{category === 'all' ? 'All categories' : category}</Select.Trigger>
		<Select.Content>
			{#each categories as c}
				<Select.Item value={c} label={c === 'all' ? 'All categories' : c} />
			{/each}
		</Select.Content>
	</Select.Root>
	<Select.Root type="single" value={sort} onValueChange={(v) => { if (v) { sort = v; page = 0; } }}>
		<Select.Trigger class="h-8 w-36 text-sm">{sortOptions.find((o) => o.value === sort)?.label}</Select.Trigger>
		<Select.Content>
			{#each sortOptions as o}
				<Select.Item value={o.value} label={o.label} />
			{/each}
		</Select.Content>
	</Select.Root>
</div>

{#if error}
	<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200">{error}</div>
{/if}

{#if loading && !resp}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
{:else if resp && resp.items.length === 0 && resp.uncatalogued.length === 0}
	<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
		<Database class="size-10 text-muted-foreground/40" />
		<p class="mt-3 text-sm text-muted-foreground">No catalogued content yet</p>
	</div>
{:else if resp}
	<!-- Column header -->
	<div class="grid grid-cols-12 gap-3 px-4 pb-1.5 text-sm font-semibold uppercase tracking-wider text-muted-foreground">
		<span class="col-span-6">Entry</span>
		<span class="col-span-2">Category</span>
		<span class="col-span-2 text-right">Size</span>
		<span class="col-span-2 text-right">Copies</span>
	</div>

	<div class="space-y-1.5">
		{#each resp.items as entry (entryKey(entry))}
			{@const k = entryKey(entry)}
			{@const open = expanded.has(k)}
			<div class="rounded-lg border border-border bg-card transition-colors hover:bg-accent/30">
				<button class="grid w-full grid-cols-12 items-center gap-3 px-4 py-2.5 text-left" onclick={() => toggle(k)}>
					<div class="col-span-6 flex min-w-0 items-center gap-1.5">
						{#if open}<ChevronDown class="size-3.5 shrink-0 text-muted-foreground" />{:else}<ChevronRight class="size-3.5 shrink-0 text-muted-foreground" />{/if}
						<span class="truncate text-sm font-medium text-foreground" title={entry.name}>{entry.name}</span>
					</div>
					<div class="col-span-2"><Badge variant="secondary">{entry.category}</Badge></div>
					<div class="col-span-2 text-right text-sm tabular-nums text-muted-foreground">{fmtSize(entry.size_bytes)}</div>
					<div class="col-span-2 text-right text-sm tabular-nums text-muted-foreground">{entry.copies.length}</div>
				</button>

				{#if open}
					<div class="border-t border-border px-4 py-2.5">
						{#if entry.content_hash}
							<div class="mb-2 flex items-center gap-1 text-sm text-muted-foreground">
								<Hash class="size-3" />
								<span class="font-mono">{entry.content_hash.slice(0, 24)}</span>
								<CopyButton text={entry.content_hash} title="Copy content hash" iconClass="w-3 h-3" />
							</div>
						{/if}
						{#if entry.copies.length === 0}
							<p class="text-sm italic text-muted-foreground">No physical copies tracked.</p>
						{:else}
							<div class="space-y-1">
								{#each entry.copies as c}
									<div class="flex items-center gap-2 text-sm">
										{#if c.is_canonical}<Star class="size-3 shrink-0 fill-amber-400 text-amber-400" />{/if}
										<button
											class="inline-flex items-center gap-1 text-muted-foreground hover:text-foreground"
											onclick={onViewServers}
											title="View server {c.file_server_id}"
										>
											<Server class="size-3" />
											<span class="font-medium">{c.server_display_name ?? c.file_server_id}</span>
											{#if c.server_kind}<Badge variant="outline" class="px-1 py-0 text-[10px]">{c.server_kind}</Badge>{/if}
										</button>
										<span class="truncate font-mono text-muted-foreground" title={c.path}>{c.path}</span>
										<Badge class={statusColor(c.status)} variant="secondary">{c.status}</Badge>
									</div>
								{/each}
							</div>
						{/if}
						{#if entry.entry_id}
							<a class="mt-2 inline-block text-sm text-primary hover:underline" href={`/catalogue?search=${encodeURIComponent(entry.content_hash ?? entry.name)}`}>Open in catalogue →</a>
						{/if}
					</div>
				{/if}
			</div>
		{/each}
	</div>

	<!-- Pagination -->
	{#if resp.total_pages > 1}
		<div class="mt-4 flex items-center justify-between">
			<p class="text-sm text-muted-foreground">Showing {resp.items.length} of {resp.total.toLocaleString()} entries</p>
			<div class="flex items-center gap-1">
				<Button variant="ghost" size="icon-sm" disabled={!resp.has_previous} onclick={() => (page = page - 1)}><ChevronLeft class="size-4" /></Button>
				<span class="px-2 text-sm tabular-nums text-muted-foreground">{resp.page + 1} / {resp.total_pages}</span>
				<Button variant="ghost" size="icon-sm" disabled={!resp.has_next} onclick={() => (page = page + 1)}><ChevronRight class="size-4" /></Button>
			</div>
		</div>
	{/if}

	<!-- Uncatalogued (index-only) files -->
	{#if resp.uncatalogued_count > 0}
		<div class="mt-6 rounded-lg border border-dashed border-border">
			<button class="flex w-full items-center gap-2 px-4 py-2.5 text-left" onclick={() => (showUncatalogued = !showUncatalogued)}>
				{#if showUncatalogued}<ChevronDown class="size-3.5 text-muted-foreground" />{:else}<ChevronRight class="size-3.5 text-muted-foreground" />{/if}
				<FileQuestion class="size-4 text-muted-foreground" />
				<span class="text-sm font-medium text-foreground">Uncatalogued files</span>
				<Badge variant="secondary">{resp.uncatalogued_count.toLocaleString()}</Badge>
				<span class="text-sm text-muted-foreground">— observed on disk, not yet hashed/registered</span>
			</button>
			{#if showUncatalogued}
				<div class="space-y-1 border-t border-border px-4 py-2.5">
					{#each resp.uncatalogued as u}
						{@const c = u.copies[0]}
						<div class="flex items-center gap-2 text-sm">
							<span class="truncate font-medium text-foreground">{u.name}</span>
							{#if c}
								<button class="inline-flex items-center gap-1 text-muted-foreground hover:text-foreground" onclick={onViewServers}>
									<Server class="size-3" /><span>{c.server_display_name ?? c.file_server_id}</span>
								</button>
								<span class="truncate font-mono text-muted-foreground" title={c.path}>{c.path}</span>
								<Badge class={statusColor(c.status)} variant="secondary">{c.status}</Badge>
							{/if}
						</div>
					{/each}
					{#if resp.uncatalogued_count > resp.uncatalogued.length}
						<p class="pt-1 text-sm text-muted-foreground">…and {(resp.uncatalogued_count - resp.uncatalogued.length).toLocaleString()} more</p>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
{/if}
