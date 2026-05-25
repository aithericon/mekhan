<script lang="ts">
	// Typed resource picker — popover that filters by `type` and lists all
	// resources of that type in the current workspace. Bound value is the
	// resource's `path` (Windmill-style `[ufg]/<owner>/<name>`), because
	// that's what `CreateInstanceRequest.resource_bindings` accepts.
	//
	// Mirrors `RefPicker.svelte`'s popover shape (chevron trigger, filter
	// input, scrollable list), simplified to a single column — resources
	// are a flat list within a type, not a producer-keyed grouping.
	import * as Popover from '$lib/components/ui/popover';
	import { Input } from '$lib/components/ui/input';
	import { cn } from '$lib/utils.js';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import { listResources, type ResourceSummary } from '$lib/api/resources';

	type Props = {
		/** Filter to this resource type (`postgres`, `openai`, …). */
		type: string;
		/** Currently-bound resource path. `null` when no binding is set. */
		value: string | null;
		/** Optional workspace filter — defaults to the no-workspace path
		 *  (`Uuid::nil()` on the server) until workspaces land. */
		workspace_id?: string;
		disabled?: boolean;
		placeholder?: string;
		onchange: (path: string | null) => void;
	};

	let {
		type,
		value,
		workspace_id,
		disabled = false,
		placeholder = 'Pick a resource…',
		onchange
	}: Props = $props();

	let open = $state(false);
	let query = $state('');
	let resources = $state<ResourceSummary[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	// Lazy-load when the popover opens (or `type` changes while open) — saves
	// a fetch per AutomatedStep panel render. The list is small enough that
	// a single GET covers every interaction inside the popover.
	$effect(() => {
		if (!open) return;
		void type;
		void workspace_id;
		loading = true;
		error = null;
		listResources({ resource_type: type, workspace_id, perPage: 200 })
			.then((p) => {
				resources = p.items;
			})
			.catch((e) => {
				error = e instanceof Error ? e.message : 'Failed to load resources';
				resources = [];
			})
			.finally(() => {
				loading = false;
			});
	});

	$effect(() => {
		if (!open) query = '';
	});

	const q = $derived(query.trim().toLowerCase());
	const visible = $derived.by(() => {
		if (!q) return resources;
		return resources.filter(
			(r) =>
				r.path.toLowerCase().includes(q) ||
				r.display_name.toLowerCase().includes(q)
		);
	});

	function pick(r: ResourceSummary) {
		onchange(r.path);
		open = false;
	}

	function clear() {
		onchange(null);
		open = false;
	}
</script>

<Popover.Root bind:open>
	<Popover.Trigger
		{disabled}
		class={cn(
			'flex h-9 w-full items-center justify-between gap-1.5 rounded-md border border-input bg-transparent px-3 text-sm outline-none transition-colors hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50'
		)}
		data-testid="resource-picker-trigger"
	>
		{#if value}
			<span class="truncate font-mono">{value}</span>
		{:else}
			<span class="text-muted-foreground">{placeholder}</span>
		{/if}
		<ChevronsUpDown class="size-4 shrink-0 opacity-50" />
	</Popover.Trigger>

	<Popover.Content align="start" class="w-[420px] max-w-[90vw] overflow-hidden p-0">
		<div class="flex items-center gap-2 border-b p-3">
			<Input
				type="text"
				value={query}
				placeholder="Filter resources…"
				oninput={(e) => (query = (e.currentTarget as HTMLInputElement).value)}
				class="h-9 flex-1 text-sm"
			/>
			{#if value}
				<button
					type="button"
					class="flex h-9 shrink-0 items-center gap-1 rounded-md px-2 text-sm text-muted-foreground hover:bg-accent hover:text-foreground"
					onclick={clear}
					title="Clear binding"
				>
					<XCircle class="size-4" />
					Clear
				</button>
			{/if}
		</div>

		{#if loading}
			<div class="p-4 text-sm italic text-muted-foreground">Loading…</div>
		{:else if error}
			<div class="p-4 text-sm text-destructive">{error}</div>
		{:else if visible.length === 0}
			<div class="p-4 text-sm italic text-muted-foreground">
				{resources.length === 0
					? `No ${type} resources defined. Create one under /resources.`
					: 'No matching resources.'}
			</div>
		{:else}
			<ul class="max-h-80 overflow-y-auto py-1">
				{#each visible as r (r.id)}
					<li>
						<button
							type="button"
							class={cn(
								'flex w-full items-center justify-between gap-3 px-3 py-2 text-left text-sm transition-colors hover:bg-accent',
								value === r.path && 'bg-accent'
							)}
							onclick={() => pick(r)}
							title={`${r.display_name} (v${r.latest_version})`}
						>
							<div class="flex min-w-0 flex-col">
								<span class="truncate font-mono text-sm">{r.path}</span>
								<span class="truncate text-sm text-muted-foreground">
									{r.display_name}
								</span>
							</div>
							<span class="shrink-0 text-sm text-muted-foreground">v{r.latest_version}</span>
						</button>
					</li>
				{/each}
			</ul>
		{/if}
	</Popover.Content>
</Popover.Root>
