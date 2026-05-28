<script lang="ts">
	// Top-level resource list. Renders the workspace's resources (one row
	// each) with Create + Delete affordances. The schema-driven create
	// flow lives in `ResourceEditModal.svelte`; this component owns the
	// list, filter dropdown, and refresh — not the form.
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import * as Select from '$lib/components/ui/select';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import {
		deleteResource,
		listResources,
		listResourceTypes,
		type ResourceSummary,
		type ResourceTypeInfo
	} from '$lib/api/resources';
	import ResourceEditModal from './ResourceEditModal.svelte';

	type Props = {
		workspace_id?: string;
	};

	let { workspace_id }: Props = $props();

	let resources = $state<ResourceSummary[]>([]);
	let types = $state<ResourceTypeInfo[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let typeFilter = $state<string>('');

	let editorOpen = $state(false);
	let editingId = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			const [p, t] = await Promise.all([
				listResources({
					resource_type: typeFilter || undefined,
					workspace_id,
					perPage: 200
				}),
				types.length === 0 ? listResourceTypes() : Promise.resolve(types)
			]);
			resources = p.items;
			if (types.length === 0) types = t;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load resources';
			resources = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		void typeFilter;
		void workspace_id;
		load();
	});

	function openCreate() {
		editingId = null;
		editorOpen = true;
	}

	function openEdit(id: string) {
		editingId = id;
		editorOpen = true;
	}

	async function handleDelete(id: string, path: string) {
		if (!confirm(`Soft-delete resource "${path}"? Existing pinned instances keep resolving against their pinned version.`)) return;
		try {
			await deleteResource(id);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete';
		}
	}

	function onSaved() {
		editorOpen = false;
		editingId = null;
		load();
	}

	const typeIcon: Record<string, string> = {
		postgres: '🐘',
		openai: '✨',
		slack: '💬',
		s3: '📦',
		smtp: '✉️',
		google_oauth: '🔑'
	};

	const formatDate = (s: string) => new Date(s).toLocaleString();
</script>

<div class="space-y-4" data-testid="resources-list">
	<div class="flex flex-wrap items-center gap-3">
		<div class="flex items-center gap-2">
			<span class="text-sm font-medium text-muted-foreground">Type</span>
			<Select.Root
				type="single"
				value={typeFilter}
				onValueChange={(v) => (typeFilter = v ?? '')}
			>
				<Select.Trigger class="h-9 min-w-[160px]">
					{typeFilter
						? (types.find((t) => t.name === typeFilter)?.display_name ?? typeFilter)
						: 'All types'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="" label="All types" />
					{#each types as t (t.name)}
						<Select.Item value={t.name} label={t.display_name} />
					{/each}
				</Select.Content>
			</Select.Root>
		</div>
		<Button
			variant="default"
			size="sm"
			onclick={openCreate}
			class="ml-auto gap-1.5"
			data-testid="resource-create-button"
		>
			<Plus class="size-4" />
			New resource
		</Button>
	</div>

	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading…
		</div>
	{:else if resources.length === 0}
		<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
			<KeyRound class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No resources defined</p>
			<p class="text-sm text-muted-foreground">
				Resources are typed credentials (Postgres, OpenAI, …) workflows bind by alias at launch.
			</p>
			<Button variant="outline" size="sm" class="mt-4 gap-1.5" onclick={openCreate}>
				<Plus class="size-4" />
				Create your first resource
			</Button>
		</div>
	{:else}
		<div class="space-y-2">
			{#each resources as r (r.id)}
				<div
					class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/40"
					data-testid="resource-item-{r.id}"
				>
					<button
						type="button"
						class="flex min-w-0 flex-1 items-start gap-3 text-left"
						onclick={() => openEdit(r.id)}
					>
						<span class="mt-0.5 text-base leading-none">{typeIcon[r.resource_type] ?? '🔐'}</span>
						<div class="min-w-0 flex-1">
							<div class="flex flex-wrap items-center gap-2">
								<span class="font-mono text-sm font-medium text-foreground">{r.path}</span>
								<Badge variant="secondary">{r.resource_type}</Badge>
								<Badge variant="outline">v{r.latest_version}</Badge>
							</div>
							<p class="mt-1 truncate text-sm text-muted-foreground">{r.display_name}</p>
							<p class="mt-1 text-sm text-muted-foreground">
								Updated {formatDate(r.updated_at)}
							</p>
						</div>
					</button>
					<div class="flex shrink-0 items-center gap-1 opacity-0 transition-opacity group-hover:opacity-100">
						<Button
							variant="ghost"
							size="sm"
							class="gap-1 text-sm text-muted-foreground"
							onclick={() => openEdit(r.id)}
							title="Edit / rotate"
						>
							<RotateCcw class="size-3.5" />
							Edit
						</Button>
						<Button
							variant="ghost"
							size="sm"
							class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
							onclick={() => handleDelete(r.id, r.path)}
							title="Soft-delete"
						>
							<Trash2 class="size-3.5" />
						</Button>
					</div>
				</div>
			{/each}
		</div>
	{/if}
</div>

<ResourceEditModal
	bind:open={editorOpen}
	resource_id={editingId}
	{types}
	{workspace_id}
	onsaved={onSaved}
/>
