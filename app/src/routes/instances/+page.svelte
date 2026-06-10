<script lang="ts">
	import { page } from '$app/state';
	import { listInstances, cancelInstance, type InstanceListItem } from '$lib/api/client';
	import { PageShell, PageHeader, FilterPills, type FilterPill } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Activity from '@lucide/svelte/icons/activity';
	import X from '@lucide/svelte/icons/x';

	let instances = $state<InstanceListItem[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const templateFilter = $derived(page.url.searchParams.get('template_id') ?? undefined);
	const statusFilter = $derived(page.url.searchParams.get('status') ?? undefined);
	/// `'any'` returns everything; an explicit category scopes; absent
	/// defaults to live-only (the historical view).
	const modeFilter = $derived(page.url.searchParams.get('mode') ?? undefined);
	const filteredTemplateName = $derived(
		templateFilter ? (instances[0]?.template_name ?? 'this template') : null
	);

	const modeBadgeClass: Record<string, string> = {
		draft: 'bg-amber-100 text-amber-800',
		test_run: 'bg-purple-100 text-purple-800'
	};

	const baseQuery = $derived(
		templateFilter ? `template_id=${encodeURIComponent(templateFilter)}&` : ''
	);

	// Mode filter pills. Default view hides drafts and test runs; `any`
	// shows everything so the user can scope into them.
	const activeMode = $derived(
		modeFilter === undefined || modeFilter === 'live'
			? 'live'
			: modeFilter === 'all'
				? 'any'
				: modeFilter
	);
	const modeOptions = $derived<FilterPill[]>([
		{ value: 'live', label: 'Live', href: `/instances?${baseQuery}` },
		{ value: 'draft', label: 'Drafts', href: `/instances?${baseQuery}mode=draft` },
		{ value: 'test_run', label: 'Test runs', href: `/instances?${baseQuery}mode=test_run` },
		{ value: 'any', label: 'All', href: `/instances?${baseQuery}mode=any` }
	]);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	async function load() {
		loading = true;
		error = null;
		try {
			const result = await listInstances({
				templateId: templateFilter,
				status: statusFilter,
				mode: modeFilter
			});
			instances = result.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load instances';
			instances = [];
		} finally {
			loading = false;
		}
	}

	async function handleCancel(id: string) {
		if (!confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(id);
			instances = instances.map((i) => (i.id === id ? { ...i, status: 'cancelled' } : i));
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();

	$effect(() => {
		// Re-load when the URL filter (template_id / status / mode) changes.
		void templateFilter;
		void statusFilter;
		void modeFilter;
		load();
	});
</script>

<PageShell testid="instances-page">
	{#snippet band()}
		<PageHeader title="Instances" subtitle="Running and completed workflow instances" />
	{/snippet}

	<FilterPills class="mb-4" testid="mode-filter" active={activeMode} options={modeOptions} />

	{#if templateFilter}
		<div
			class="mb-4 flex items-center gap-2 rounded-lg border border-border bg-accent/40 px-3 py-2 text-sm"
		>
			<span class="text-muted-foreground">Runs of</span>
			<span class="font-medium text-foreground">{filteredTemplateName}</span>
			{#if statusFilter}
				<Badge variant="secondary">{statusFilter}</Badge>
			{/if}
			<a
				href="/instances"
				class="ml-auto inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
			>
				<X class="size-3" /> Clear
			</a>
		</div>
	{/if}

	{#if error}
		<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading...
		</div>
	{:else if instances.length === 0}
		<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
			<Activity class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No instances yet</p>
			<p class="text-sm text-muted-foreground">
				Publish a template and run it to create instances
			</p>
		</div>
	{:else}
		<div class="space-y-2">
			{#each instances as instance (instance.id)}
				<a
					href="/instances/{instance.id}"
					class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/50"
					data-testid="instance-item-{instance.id}"
				>
					<div class="min-w-0">
						<div class="flex items-center gap-2">
							<span class="text-sm font-medium text-foreground">
								{instance.template_name ?? instance.net_id}
							</span>
							<Badge variant="secondary">
								v{instance.template_version}
							</Badge>
							<Badge class={statusColors[instance.status] ?? ''} variant="secondary">
								{instance.status}
							</Badge>
							{#if instance.mode && instance.mode !== 'live'}
								<Badge
									class={modeBadgeClass[instance.mode] ?? ''}
									variant="secondary"
								>
									{instance.mode === 'test_run' ? 'test run' : instance.mode}
								</Badge>
							{/if}
						</div>
						{#if instance.current_step}
							<p class="mt-1 text-sm text-muted-foreground">
								Current: {instance.current_step}
							</p>
						{/if}
						<p class="mt-1 text-sm text-muted-foreground">
							<span class="font-mono">{instance.net_id}</span>
							<span class="mx-1">&middot;</span>
							{formatDate(instance.created_at)}
						</p>
					</div>
					{#if instance.status === 'running' || instance.status === 'created'}
						<Button
							variant="ghost"
							size="sm"
							class="text-muted-foreground opacity-0 transition-all hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100"
							onclick={(e: MouseEvent) => {
								e.preventDefault();
								e.stopPropagation();
								handleCancel(instance.id);
							}}
						>
							Cancel
						</Button>
					{/if}
				</a>
			{/each}
		</div>
	{/if}
</PageShell>
