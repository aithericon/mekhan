<script lang="ts">
	import { page } from '$app/state';
	import {
		getInstance,
		cancelInstance,
		listProcessesByInstance
	} from '$lib/api/client';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		provideInstanceContext,
		type InstanceContext
	} from '$lib/components/instances/instance-context';
	import FileText from '@lucide/svelte/icons/file-text';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import ListChecks from '@lucide/svelte/icons/list-checks';
	import Workflow from '@lucide/svelte/icons/workflow';
	import Network from '@lucide/svelte/icons/network';

	let { children } = $props();

	const instanceId = $derived(page.params.id!);
	const pathname = $derived(page.url.pathname);

	// Single reactive store shared with every subroute. Subpages mutate
	// `instance`/`processes`/etc. through `reload()`; we never re-assign the
	// object itself so the context handle stays stable.
	const ctx = $state<InstanceContext>({
		instanceId,
		instance: null,
		processes: [],
		loading: true,
		error: null,
		reload
	});

	provideInstanceContext(ctx);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	const hasNet = $derived(
		!!ctx.instance && ctx.instance.status !== 'created' && !!ctx.instance.net_id
	);
	const primaryProcess = $derived(ctx.processes[0] ?? null);
	const processName = $derived(primaryProcess?.name ?? null);

	async function reload() {
		ctx.loading = true;
		ctx.error = null;
		try {
			ctx.instance = await getInstance(ctx.instanceId);
			try {
				ctx.processes = (await listProcessesByInstance(ctx.instanceId)).items;
			} catch {
				ctx.processes = [];
			}
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to load instance';
		} finally {
			ctx.loading = false;
		}
	}

	async function handleCancel() {
		if (!ctx.instance || !confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(ctx.instance.id);
			ctx.instance = { ...ctx.instance, status: 'cancelled' };
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	$effect(() => {
		ctx.instanceId = instanceId;
		reload();
	});

	type TabDef = {
		href: string;
		match: string;
		label: string;
		icon: typeof LayoutDashboard;
		tone?: 'muted';
		title?: string;
	};

	const tabs = $derived<TabDef[]>([
		{
			href: `/instances/${instanceId}/process`,
			match: 'process',
			label: 'Process',
			icon: LayoutDashboard
		},
		...(hasNet
			? [
					{
						href: `/instances/${instanceId}/workflow`,
						match: 'workflow',
						label: 'Workflow',
						icon: Workflow,
						title: 'Template graph overlaid with per-step runtime status'
					},
					{
						href: `/instances/${instanceId}/steps`,
						match: 'steps',
						label: 'Steps',
						icon: ListChecks,
						title: 'Per-step runtime as a table — every iteration as a row'
					},
					{
						href: `/instances/${instanceId}/petri-net`,
						match: 'petri-net',
						label: 'Petri net',
						icon: Network,
						tone: 'muted' as const,
						title: 'Engine debug: the raw Petri net for this run'
					}
				]
			: [])
	]);

	function isActive(match: string): boolean {
		return pathname.startsWith(`/instances/${instanceId}/${match}`);
	}
</script>

<div class="flex h-full flex-col" data-testid="instance-page">
	{#if ctx.loading && !ctx.instance}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading...
		</div>
	{:else if ctx.error && !ctx.instance}
		<div
			class="mx-6 mt-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
		>
			{ctx.error}
		</div>
	{:else if ctx.instance}
		<div class="border-b border-border bg-card px-4 py-2 shrink-0">
			<div class="flex items-center justify-between gap-3">
				<div class="flex items-center gap-3 min-w-0">
					<h1 class="shrink-0 text-base font-semibold text-foreground">
						{processName ?? 'Run'}
					</h1>
					<Badge class={statusColors[ctx.instance.status] ?? ''} variant="secondary">
						{ctx.instance.status}
					</Badge>
					<span class="font-mono text-sm text-muted-foreground truncate">
						{ctx.instance.net_id}
					</span>
				</div>
				<div class="flex items-center gap-2 shrink-0">
					<Button variant="ghost" size="sm" href="/templates/{ctx.instance.template_id}">
						<FileText class="size-3.5" />
						Template v{ctx.instance.template_version}
					</Button>
					{#if ctx.instance.status === 'running' || ctx.instance.status === 'created'}
						<Button
							variant="outline"
							size="sm"
							class="border-destructive/30 text-destructive hover:bg-destructive/10"
							onclick={handleCancel}
						>
							Cancel
						</Button>
					{/if}
				</div>
			</div>
			<div class="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-sm text-muted-foreground">
				<span>created {formatDate(ctx.instance.created_at)}</span>
				<span>started {formatDate(ctx.instance.started_at ?? null)}</span>
				<span>completed {formatDate(ctx.instance.completed_at ?? null)}</span>
				{#if ctx.instance.current_step}
					<span class="text-foreground">step: {ctx.instance.current_step}</span>
				{/if}
			</div>
		</div>

		{#if primaryProcess || hasNet}
			<!-- Tab subroutes: Process (HPI) is primary; Workflow shows the
			     template graph overlaid with per-step runtime info; Steps is the
			     same data as a table; Petri net is the engine debug view. Each
			     is a proper subpage — navigation unmounts the previous view. -->
			<nav
				class="flex items-center gap-1 border-b border-border bg-card px-3 py-1 shrink-0"
				data-testid="instance-tabs"
			>
				{#each tabs as tab (tab.match)}
					{@const active = isActive(tab.match)}
					{@const Icon = tab.icon}
					<a
						href={tab.href}
						class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-sm font-medium transition-colors
							{active
							? tab.tone === 'muted'
								? 'bg-accent text-foreground'
								: 'bg-primary text-primary-foreground'
							: tab.tone === 'muted'
								? 'text-muted-foreground/70 hover:bg-accent hover:text-foreground'
								: 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
						title={tab.title}
						data-testid="instance-tab-{tab.match}"
						aria-current={active ? 'page' : undefined}
					>
						<Icon class="size-3.5" />
						{tab.label}
					</a>
				{/each}
			</nav>

			<div class="relative flex-1 min-h-0">
				{@render children()}
			</div>
		{:else}
			<div
				class="flex flex-1 items-center justify-center py-16 text-sm text-muted-foreground"
			>
				Instance has not started yet. No Petri net is available.
			</div>
		{/if}
	{/if}
</div>
