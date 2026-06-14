<script lang="ts">
	import { onMount } from 'svelte';
	import {
		listNodeLibrary,
		getTemplate,
		demoteTemplate,
		type LibraryNodeDescriptor,
		type Template
	} from '$lib/api/client';
	import { roleAtLeast } from '$lib/api/iam';
	import { resolveNodeIcon } from '$lib/editor/icon-registry';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem
	} from '$lib/components/ui/dropdown-menu';
	import Search from '@lucide/svelte/icons/search';
	import Package from '@lucide/svelte/icons/package';
	import Settings from '@lucide/svelte/icons/settings';
	import ArrowDownToLine from '@lucide/svelte/icons/arrow-down-to-line';
	import EllipsisVertical from '@lucide/svelte/icons/ellipsis-vertical';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import PromoteLibraryDialog from '$lib/components/editor/PromoteLibraryDialog.svelte';

	let nodes = $state<LibraryNodeDescriptor[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let searchQuery = $state('');

	// Management view surfaces the full set so deprecated/retired nodes can be
	// re-branded or reactivated. The two flags ride the listing request.
	let includeDeprecated = $state(true);
	let includeRetired = $state(true);

	// Manage reuses the editor's PromoteLibraryDialog, which needs a full
	// Template (it reads `template_kind` to enter Manage mode + presentation /
	// coordinate / lifecycle). The row only carries the family-root id, so we
	// lazily fetch the Template when Manage is clicked.
	let promoteOpen = $state(false);
	let manageTemplate = $state<Template | null>(null);
	let manageBusyId = $state<string | null>(null);

	async function reload() {
		loading = true;
		error = null;
		try {
			nodes = await listNodeLibrary({ includeDeprecated, includeRetired });
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load library nodes';
			nodes = [];
		} finally {
			loading = false;
		}
	}

	const filtered = $derived(
		(() => {
			const q = searchQuery.trim().toLowerCase();
			if (!q) return nodes;
			return nodes.filter(
				(n) =>
					n.name.toLowerCase().includes(q) ||
					n.coordinate.toLowerCase().includes(q) ||
					(n.presentation?.vendor ?? '').toLowerCase().includes(q) ||
					(n.presentation?.category ?? '').toLowerCase().includes(q)
			);
		})()
	);

	// Manage + Demote are Admin+ only, and system-origin nodes are
	// governance-locked by the backend (demote/lifecycle reject them).
	function canGovern(n: LibraryNodeDescriptor): boolean {
		return n.origin !== 'system' && roleAtLeast(n.myEffectiveRole, 'admin');
	}

	async function openManage(n: LibraryNodeDescriptor) {
		if (manageBusyId) return;
		manageBusyId = n.templateId;
		try {
			manageTemplate = await getTemplate(n.templateId);
			promoteOpen = true;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Could not load node for management';
		} finally {
			manageBusyId = null;
		}
	}

	async function handleDemote(n: LibraryNodeDescriptor) {
		if (!confirm(`Demote "${n.name}" back to a plain workflow? It will no longer appear in the palette.`))
			return;
		try {
			await demoteTemplate(n.templateId);
			await reload();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to demote node';
		}
	}

	function originClass(origin: string): string {
		if (origin === 'system') return 'bg-violet-100 text-violet-700';
		if (origin === 'community') return 'bg-sky-100 text-sky-700';
		return 'bg-slate-100 text-slate-700'; // workspace
	}

	function lifecycleClass(status: string): string {
		if (status === 'deprecated') return 'bg-amber-100 text-amber-700';
		if (status === 'retired') return 'bg-muted text-muted-foreground';
		return 'bg-emerald-100 text-emerald-700'; // active
	}

	onMount(reload);
</script>

<PageShell testid="node-library-page">
	{#snippet band()}
		<PageHeader title="Node Library" subtitle="Manage workspace library nodes — rebrand, lifecycle, and demote">
		</PageHeader>
	{/snippet}

	{#if error}
		<div
			class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
			data-testid="library-error"
		>
			{error}
		</div>
	{/if}

	<div class="mb-4 flex flex-wrap items-center gap-3">
		<div class="relative min-w-64 flex-1">
			<Search
				class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
			/>
			<Input
				type="search"
				placeholder="Search nodes by name, coordinate, vendor"
				bind:value={searchQuery}
				data-testid="library-search"
				class="pl-9"
			/>
		</div>
		<label class="flex items-center gap-2 text-sm text-muted-foreground">
			<Checkbox
				checked={includeDeprecated}
				onCheckedChange={(v: boolean) => {
					includeDeprecated = v;
					reload();
				}}
				data-testid="library-toggle-deprecated"
			/>
			Deprecated
		</label>
		<label class="flex items-center gap-2 text-sm text-muted-foreground">
			<Checkbox
				checked={includeRetired}
				onCheckedChange={(v: boolean) => {
					includeRetired = v;
					reload();
				}}
				data-testid="library-toggle-retired"
			/>
			Retired
		</label>
	</div>

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading...</div>
	{:else if filtered.length === 0 && searchQuery.trim()}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
			data-testid="library-empty"
		>
			<Search class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No nodes match your search</p>
		</div>
	{:else if filtered.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
			data-testid="library-empty"
		>
			<Package class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No library nodes yet</p>
			<p class="mt-1 text-sm text-muted-foreground/70">
				Promote a published template from its editor to advertise it here.
			</p>
		</div>
	{:else}
		<div class="space-y-2" data-testid="library-list">
			{#each filtered as node (node.templateId)}
				{@const Icon = resolveNodeIcon(node.presentation?.icon)}
				{@const govern = canGovern(node)}
				<div
					class="flex flex-col gap-3 rounded-lg border border-border bg-card p-4"
					data-testid="library-row"
				>
					<div class="flex items-start justify-between gap-3">
						<div class="flex min-w-0 items-start gap-3">
							<div
								class="flex size-9 shrink-0 items-center justify-center rounded-md border border-border"
								style={node.presentation?.color
									? `color: ${node.presentation.color}; border-color: ${node.presentation.color}33;`
									: undefined}
							>
								<Icon class="size-5" />
							</div>
							<div class="min-w-0">
								<div class="flex flex-wrap items-center gap-2">
									<span class="truncate text-sm font-medium text-foreground">{node.name}</span>
									<Badge variant="secondary">v{node.version}</Badge>
									<Badge class={originClass(node.origin)} variant="secondary">{node.origin}</Badge>
									<Badge class={lifecycleClass(node.lifecycleStatus)} variant="secondary">
										{node.lifecycleStatus}
									</Badge>
								</div>
								<div class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-sm text-muted-foreground">
									<span class="font-mono text-foreground">{node.coordinate}</span>
									{#if node.presentation?.vendor}
										<span>{node.presentation.vendor}</span>
									{/if}
									{#if node.presentation?.category}
										<span class="rounded border border-border bg-muted px-1.5 py-0.5 text-xs">
											{node.presentation.category}
										</span>
									{/if}
								</div>
								{#if node.supersededBy}
									<p class="mt-1 text-sm text-amber-700">use {node.supersededBy} instead</p>
								{/if}
								{#if node.description}
									<p class="mt-1 truncate text-sm text-muted-foreground">{node.description}</p>
								{/if}
							</div>
						</div>
						{#if govern}
							<div class="flex shrink-0 items-center gap-1">
								<Button
									size="sm"
									variant="outline"
									disabled={manageBusyId === node.templateId}
									onclick={() => openManage(node)}
									data-testid="library-manage"
								>
									<Settings class="size-4" />
									{manageBusyId === node.templateId ? 'Loading…' : 'Manage'}
								</Button>
								<DropdownMenu>
									<DropdownMenuTrigger
										data-testid="library-row-menu"
										aria-label="Node actions"
										class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground data-[state=open]:bg-accent data-[state=open]:text-foreground"
									>
										<EllipsisVertical class="size-4" />
									</DropdownMenuTrigger>
									<DropdownMenuContent align="end">
										<DropdownMenuItem
											variant="destructive"
											data-testid="library-demote"
											onSelect={() => handleDemote(node)}
										>
											<ArrowDownToLine class="size-4" />
											Demote to workflow
										</DropdownMenuItem>
									</DropdownMenuContent>
								</DropdownMenu>
							</div>
						{/if}
					</div>
				</div>
			{/each}
		</div>
	{/if}
</PageShell>

{#if manageTemplate}
	<PromoteLibraryDialog
		bind:open={promoteOpen}
		template={manageTemplate}
		onpromoted={() => {
			promoteOpen = false;
			reload();
		}}
	/>
{/if}
