<script lang="ts">
	import { onMount } from 'svelte';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { toast } from 'svelte-sonner';
	import {
		getPack,
		exportPack,
		deletePack,
		ApiError,
		type LibraryPackDetail,
		type Presentation
	} from '$lib/api/client';
	import { roleAtLeast } from '$lib/api/iam';
	import LibraryIconBox from '$lib/components/library/LibraryIconBox.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Package from '@lucide/svelte/icons/package';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Download from '@lucide/svelte/icons/download';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	const id = $derived(page.params.id!);

	let pack = $state<LibraryPackDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let exporting = $state(false);
	let removing = $state(false);

	async function reload() {
		loading = true;
		error = null;
		try {
			pack = await getPack(id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load pack';
			pack = null;
		} finally {
			loading = false;
		}
	}

	function originClass(origin: string): string {
		if (origin === 'system') return 'bg-violet-100 text-violet-700';
		if (origin === 'community') return 'bg-sky-100 text-sky-700';
		return 'bg-slate-100 text-slate-700'; // workspace
	}

	// Remove is Admin+ only, and system-origin packs are governance-locked by the
	// backend (it 409s) — hide the affordance entirely for them. Pack detail does
	// not carry myEffectiveRole (only the summary does), so we gate on the per-node
	// effective role of the pack's own nodes, which share the pack's workspace.
	const canRemove = $derived(
		!!pack &&
			pack.origin !== 'system' &&
			pack.nodes.some((n) => roleAtLeast(n.myEffectiveRole, 'admin'))
	);

	async function handleExport() {
		if (!pack || exporting) return;
		exporting = true;
		try {
			const bundle = await exportPack({ packId: pack.id });
			const blob = new Blob([JSON.stringify(bundle, null, 2)], { type: 'application/json' });
			const url = URL.createObjectURL(blob);
			const a = document.createElement('a');
			a.href = url;
			a.download = `${pack.vendor}-${pack.slug}-v${pack.version}.pack.json`;
			document.body.appendChild(a);
			a.click();
			a.remove();
			URL.revokeObjectURL(url);
		} catch (e) {
			if (e instanceof ApiError) {
				error = e.body.error ?? e.message;
			} else {
				error = e instanceof Error ? e.message : 'Export failed';
			}
		} finally {
			exporting = false;
		}
	}

	async function handleRemove() {
		if (!pack || removing) return;
		if (
			!confirm(
				`Remove "${pack.name}" and its ${pack.nodes.length} library node${pack.nodes.length === 1 ? '' : 's'}? This cannot be undone.`
			)
		)
			return;
		removing = true;
		error = null;
		try {
			await deletePack(pack.id);
			toast.success(`Removed "${pack.name}"`);
			await goto('/library/packs');
		} catch (e) {
			if (e instanceof ApiError && e.status === 409) {
				error = e.body.error ?? 'This pack is still in use and cannot be removed';
			} else if (e instanceof ApiError) {
				error = e.body.error ?? e.message;
			} else {
				error = e instanceof Error ? e.message : 'Remove failed';
			}
		} finally {
			removing = false;
		}
	}

	onMount(reload);
</script>

<div data-testid="library-pack-detail-page">
	<a
		href="/library/packs"
		class="mb-4 inline-flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground"
	>
		<ArrowLeft class="size-4" />
		Back to packs
	</a>

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
	{:else if pack}
		<div class="mb-6 flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
			<div class="flex items-start gap-3">
				<div
					class="flex size-11 shrink-0 items-center justify-center rounded-md border border-border text-muted-foreground"
				>
					<Package class="size-6" />
				</div>
				<div class="min-w-0">
					<div class="flex flex-wrap items-center gap-2">
						<h2 class="truncate text-lg font-semibold text-foreground">{pack.name}</h2>
						<Badge class={originClass(pack.origin)} variant="secondary">{pack.origin}</Badge>
					</div>
					<div class="mt-1 flex flex-wrap items-center gap-x-3 text-sm text-muted-foreground">
						<span>{pack.vendor}</span>
						<span>v{pack.version}</span>
						<span class="font-mono text-foreground">{pack.slug}</span>
					</div>
					{#if pack.description}
						<p class="mt-1 text-sm text-muted-foreground">{pack.description}</p>
					{/if}
				</div>
			</div>
			<div class="flex shrink-0 items-center gap-2">
				<Button
					variant="outline"
					size="sm"
					onclick={handleExport}
					disabled={exporting}
					data-testid="library-pack-export"
				>
					<Download class="size-4" />
					{exporting ? 'Exporting…' : 'Export'}
				</Button>
				{#if canRemove}
					<Button
						variant="destructive"
						size="sm"
						onclick={handleRemove}
						disabled={removing}
						data-testid="library-pack-remove"
					>
						<Trash2 class="size-4" />
						{removing ? 'Removing…' : 'Remove'}
					</Button>
				{/if}
			</div>
		</div>

		<h3 class="mb-2 text-sm font-medium text-muted-foreground">
			Nodes ({pack.nodes.length})
		</h3>
		{#if pack.nodes.length === 0}
			<div class="rounded-lg border border-dashed border-border py-10 text-center text-sm text-muted-foreground">
				This pack has no library nodes.
			</div>
		{:else}
			<div class="space-y-2" data-testid="library-pack-node-list">
				{#each pack.nodes as node (node.templateId)}
					{@const presentation = (node.presentation ?? undefined) as Presentation | undefined}
					<div
						class="flex items-start gap-3 rounded-lg border border-border bg-card p-4"
						data-testid="library-pack-node-row"
					>
						<LibraryIconBox icon={presentation?.icon} color={presentation?.color} />
						<div class="min-w-0">
							<div class="flex flex-wrap items-center gap-2">
								<span class="truncate text-sm font-medium text-foreground">{node.name}</span>
								<Badge variant="secondary">v{node.version}</Badge>
							</div>
							<div class="mt-1 flex flex-wrap items-center gap-x-3 text-sm text-muted-foreground">
								<span class="font-mono text-foreground">{node.coordinate}</span>
								{#if presentation?.category}
									<span class="rounded border border-border bg-muted px-1.5 py-0.5 text-xs">
										{presentation.category}
									</span>
								{/if}
							</div>
							{#if node.description}
								<p class="mt-1 truncate text-sm text-muted-foreground">{node.description}</p>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}
	{/if}
</div>
