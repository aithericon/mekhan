<script lang="ts">
	import { onMount } from 'svelte';
	import { toast } from 'svelte-sonner';
	import { listPacks, type LibraryPackSummary } from '$lib/api/client';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Package from '@lucide/svelte/icons/package';
	import Plus from '@lucide/svelte/icons/plus';
	import InstallPackDialog from '$lib/components/library/InstallPackDialog.svelte';

	let packs = $state<LibraryPackSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let installOpen = $state(false);

	async function reload() {
		loading = true;
		error = null;
		try {
			packs = await listPacks();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load library packs';
			packs = [];
		} finally {
			loading = false;
		}
	}

	function originClass(origin: string): string {
		if (origin === 'system') return 'bg-violet-100 text-violet-700';
		if (origin === 'community') return 'bg-sky-100 text-sky-700';
		return 'bg-slate-100 text-slate-700'; // workspace
	}

	onMount(reload);
</script>

<div data-testid="library-packs-page">
	<div class="mb-4 flex items-center justify-end">
		<Button onclick={() => (installOpen = true)} data-testid="library-pack-install">
			<Plus class="size-4" />
			Install pack
		</Button>
	</div>

	{#if error}
		<div
			class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
			data-testid="library-pack-error"
		>
			{error}
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading...</div>
	{:else if packs.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
			data-testid="library-pack-empty"
		>
			<Package class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No library packs installed</p>
			<p class="mt-1 text-sm text-muted-foreground/70">
				Install a pack bundle to add a set of branded library nodes at once.
			</p>
		</div>
	{:else}
		<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3" data-testid="library-pack-list">
			{#each packs as pack (pack.id)}
				<a
					href={`/library/packs/${pack.id}`}
					class="flex flex-col gap-3 rounded-lg border border-border bg-card p-4 transition-colors hover:border-primary/50"
					data-testid="library-pack-card"
				>
					<div class="flex items-start gap-3">
						<div
							class="flex size-10 shrink-0 items-center justify-center rounded-md border border-border text-muted-foreground"
						>
							<Package class="size-5" />
						</div>
						<div class="min-w-0 flex-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="truncate text-sm font-medium text-foreground">{pack.name}</span>
								<Badge class={originClass(pack.origin)} variant="secondary">{pack.origin}</Badge>
							</div>
							<div class="mt-1 flex flex-wrap items-center gap-x-3 text-sm text-muted-foreground">
								<span>{pack.vendor}</span>
								<span>v{pack.version}</span>
								<span>{pack.nodeCount} {pack.nodeCount === 1 ? 'node' : 'nodes'}</span>
							</div>
						</div>
					</div>
					{#if pack.description}
						<p class="line-clamp-2 text-sm text-muted-foreground">{pack.description}</p>
					{/if}
				</a>
			{/each}
		</div>
	{/if}
</div>

<InstallPackDialog
	bind:open={installOpen}
	onimported={(r) => {
		toast.success(`Installed "${r.pack.name}" (${r.nodeCount} ${r.nodeCount === 1 ? 'node' : 'nodes'})`);
		reload();
	}}
/>
