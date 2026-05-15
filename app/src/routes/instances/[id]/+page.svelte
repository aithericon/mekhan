<script lang="ts">
	import { page } from '$app/state';
	import {
		getInstance,
		cancelInstance,
		listProcessesByInstance,
		type WorkflowInstance,
		type HpiProcess
	} from '$lib/api/client';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { NetWorkbench } from '$lib/components/petri';
	import type { WorkbenchApi } from '$lib/components/petri/NetWorkbench.svelte';
	import FileText from '@lucide/svelte/icons/file-text';
	import GitBranch from '@lucide/svelte/icons/git-branch';

	const instanceId = $derived(page.params.id!);

	let instance = $state<WorkflowInstance | null>(null);
	let processes = $state<HpiProcess[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	const hasNet = $derived(
		!!instance && instance.status !== 'created' && !!instance.net_id
	);
	const primaryProcess = $derived(processes[0] ?? null);

	async function load() {
		loading = true;
		error = null;
		try {
			instance = await getInstance(instanceId);
			try {
				processes = (await listProcessesByInstance(instanceId)).items;
			} catch {
				processes = [];
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load instance';
		} finally {
			loading = false;
		}
	}

	async function handleCancel() {
		if (!instance || !confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(instance.id);
			instance = { ...instance, status: 'cancelled' };
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	$effect(() => {
		load();
	});
</script>

{#snippet lineage()}
	{#if instance}
		<div class="border-b border-border bg-card px-4 py-2 shrink-0">
			<div class="flex items-center justify-between gap-3">
				<div class="flex items-center gap-3 min-w-0">
					<h1 class="text-sm font-semibold text-foreground">Run</h1>
					<Badge class={statusColors[instance.status] ?? ''} variant="secondary">
						{instance.status}
					</Badge>
					<span class="font-mono text-[11px] text-muted-foreground truncate">
						{instance.net_id}
					</span>
				</div>
				<div class="flex items-center gap-2 shrink-0">
					<Button variant="ghost" size="sm" href="/templates/{instance.template_id}">
						<FileText class="size-3.5" />
						Template v{instance.template_version}
					</Button>
					{#if primaryProcess}
						<Button
							variant="ghost"
							size="sm"
							href="/processes/{primaryProcess.process_id}"
						>
							<GitBranch class="size-3.5" />
							{processes.length > 1 ? `Processes (${processes.length})` : 'Process'}
						</Button>
					{/if}
					{#if instance.status === 'running' || instance.status === 'created'}
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
			<div class="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-[11px] text-muted-foreground">
				<span>created {formatDate(instance.created_at)}</span>
				<span>started {formatDate(instance.started_at ?? null)}</span>
				<span>completed {formatDate(instance.completed_at ?? null)}</span>
				{#if instance.current_step}
					<span class="text-foreground">step: {instance.current_step}</span>
				{/if}
			</div>
		</div>
	{/if}
{/snippet}

{#snippet header(_api: WorkbenchApi)}
	{@render lineage()}
{/snippet}

<div class="flex h-full flex-col" data-testid="instance-page">
	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
			Loading...
		</div>
	{:else if error}
		<div
			class="mx-6 mt-6 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
		>
			{error}
		</div>
	{:else if instance}
		{#if hasNet}
			<NetWorkbench netId={instance.net_id} {header} />
		{:else}
			{@render lineage()}
			<div
				class="flex flex-1 items-center justify-center py-16 text-sm text-muted-foreground"
			>
				Instance has not started yet. No Petri net is available.
			</div>
		{/if}
	{/if}
</div>
