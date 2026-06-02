<script lang="ts">
	// Pick a file from the data catalog (docs/20 §4.1 dual-source). A catalogue
	// entry's `storage_path` is already an S3 key, so "pick from catalog" reuses
	// it verbatim — no upload. Emits the chosen storage_path string, which goes
	// straight into a record's File-field value (same shape as an upload result).
	import { Dialog, DialogContent, DialogTitle, DialogDescription } from '$lib/components/ui/dialog';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import Search from '@lucide/svelte/icons/search';
	import { listCatalogueEntries, type CatalogueEntry } from '$lib/api/client';

	type Props = {
		open: boolean;
		onpick: (storagePath: string, filename: string) => void;
	};

	let { open = $bindable(), onpick }: Props = $props();

	let entries = $state<CatalogueEntry[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let search = $state('');
	let loadedFor = $state<string | null>(null);

	$effect(() => {
		if (!open) {
			loadedFor = null;
			return;
		}
		const key = search;
		if (loadedFor === key) return;
		loadedFor = key;
		void load(key);
	});

	async function load(q: string) {
		loading = true;
		error = null;
		try {
			const page = await listCatalogueEntries({ search: q || undefined, page_size: 50 });
			entries = page.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load catalog';
			entries = [];
		} finally {
			loading = false;
		}
	}

	function pick(entry: CatalogueEntry) {
		if (!entry.storage_path) return;
		onpick(entry.storage_path, entry.filename ?? entry.name);
		open = false;
	}
</script>

<Dialog bind:open>
	<DialogContent class="max-w-2xl">
		<DialogTitle class="text-lg font-semibold">Pick from catalog</DialogTitle>
		<DialogDescription class="text-sm text-muted-foreground">
			Reuse a produced artifact's storage path as this file field's value.
		</DialogDescription>

		<div class="mt-3 flex items-center gap-2">
			<Search class="size-4 text-muted-foreground" />
			<Input
				value={search}
				placeholder="Search catalog…"
				class="text-sm"
				oninput={(e) => (search = (e.currentTarget as HTMLInputElement).value)}
			/>
		</div>

		{#if error}
			<p class="mt-3 text-sm text-destructive">{error}</p>
		{/if}

		<div class="mt-3 max-h-[50vh] space-y-1.5 overflow-y-auto">
			{#if loading}
				<p class="py-8 text-center text-sm text-muted-foreground">Loading…</p>
			{:else if entries.length === 0}
				<p class="py-8 text-center text-sm text-muted-foreground">No catalog entries found.</p>
			{:else}
				{#each entries as entry (entry.id)}
					<button
						type="button"
						class="flex w-full items-center justify-between gap-3 rounded-lg border border-border bg-card p-3 text-left transition-colors hover:bg-accent/40 disabled:opacity-50"
						disabled={!entry.storage_path}
						onclick={() => pick(entry)}
					>
						<div class="min-w-0 flex-1">
							<div class="flex items-center gap-2">
								<span class="truncate text-sm font-medium">{entry.name}</span>
								<Badge variant="secondary">{entry.category}</Badge>
							</div>
							<p class="mt-0.5 truncate font-mono text-xs text-muted-foreground">
								{entry.storage_path ?? '(no storage path)'}
							</p>
						</div>
					</button>
				{/each}
			{/if}
		</div>
	</DialogContent>
</Dialog>
