<script lang="ts">
	import { listUncatalogued, type UncataloguedResponse } from '$lib/api/data';
	import { StatusBadge } from '$lib/components/status';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import FileQuestion from '@lucide/svelte/icons/file-question';
	import Server from '@lucide/svelte/icons/server';
	import Hash from '@lucide/svelte/icons/hash';

	let { onViewServer }: { onViewServer?: (key: string) => void } = $props();

	let resp = $state<UncataloguedResponse | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Loaded once when this tab mounts (bits-ui only mounts the active tab's
	// content), so the expensive whole-workspace anti-join runs only if the user
	// actually opens this tab — never on the hot Entries path.
	$effect(() => {
		loading = true;
		error = null;
		listUncatalogued()
			.then((r) => (resp = r))
			.catch((e) => {
				error = e instanceof Error ? e.message : 'Failed to load uncatalogued files';
			})
			.finally(() => (loading = false));
	});

	const truncatePath = (path: string, max = 72) =>
		path.length <= max ? path : '…' + path.slice(-(max - 1));
</script>

<p class="mb-4 text-sm text-muted-foreground">
	Files observed on a file server during a crawl but with no logical catalogue
	identity yet — not hashed or not registered. They gain an entry (and show up
	under <span class="font-medium text-foreground">Entries</span>) once reconciled.
</p>

{#if error}
	<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200">{error}</div>
{/if}

{#if loading}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
{:else if resp && resp.uncatalogued_count === 0}
	<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
		<FileQuestion class="size-10 text-muted-foreground/40" />
		<p class="mt-3 text-sm text-muted-foreground">No uncatalogued files</p>
		<p class="text-sm text-muted-foreground">Everything the platform has observed on disk is catalogued</p>
	</div>
{:else if resp}
	<div class="mb-3 flex items-center gap-2 text-sm text-muted-foreground">
		<FileQuestion class="size-4" />
		<span class="font-semibold tabular-nums text-foreground">{resp.uncatalogued_count.toLocaleString()}</span>
		uncatalogued {resp.uncatalogued_count === 1 ? 'file' : 'files'}
	</div>

	<div class="space-y-1.5">
		{#each resp.uncatalogued as u (u.copies[0]?.path ?? u.name)}
			{@const c = u.copies[0]}
			<div class="rounded-lg border border-border bg-card px-4 py-2.5 transition-colors hover:bg-accent/30">
				<div class="flex items-center gap-2">
					<span class="truncate font-medium text-foreground">{u.name}</span>
					{#if c}
						<StatusBadge domain="copy" status={c.status} />
					{/if}
				</div>
				{#if c}
					<div class="mt-0.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-sm text-muted-foreground">
						<button class="inline-flex items-center gap-1 hover:text-foreground" onclick={() => onViewServer?.(c.file_server_id)} title="View server {c.file_server_id}">
							<Server class="size-3" /><span>{c.server_display_name ?? c.file_server_id}</span>
						</button>
						<span class="truncate font-mono" title={c.path}>{truncatePath(c.path)}</span>
						{#if u.content_hash}
							<span class="inline-flex items-center gap-1">
								<Hash class="size-3" />
								<span class="font-mono">{u.content_hash.slice(0, 16)}</span>
								<CopyButton text={u.content_hash} title="Copy content hash" iconClass="w-3 h-3" />
							</span>
						{/if}
					</div>
				{/if}
			</div>
		{/each}
	</div>

	{#if resp.uncatalogued_count > resp.uncatalogued.length}
		<p class="mt-3 text-center text-sm text-muted-foreground">
			Showing the {resp.uncatalogued.length} most recent — and
			{(resp.uncatalogued_count - resp.uncatalogued.length).toLocaleString()} more
		</p>
	{/if}
{/if}
