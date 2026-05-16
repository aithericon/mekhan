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
	import { ProcessView } from '$lib/components/processes';
	import { NetWorkbench } from '$lib/components/petri';
	import FileText from '@lucide/svelte/icons/file-text';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import Network from '@lucide/svelte/icons/network';

	const instanceId = $derived(page.params.id!);

	let instance = $state<WorkflowInstance | null>(null);
	let processes = $state<HpiProcess[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let mode = $state<'process' | 'petri'>('process');
	let selectedProcessId = $state<string | null>(null);
	// The Petri workbench is heavy; only mount it once the user opens it, then
	// keep it alive (hidden) so toggling back doesn't re-init the store.
	let petriMounted = $state(false);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	const hasNet = $derived(!!instance && instance.status !== 'created' && !!instance.net_id);
	const primaryProcess = $derived(processes[0] ?? null);
	const selectedProcess = $derived(
		processes.find((p) => p.process_id === selectedProcessId) ?? primaryProcess
	);
	const processName = $derived(selectedProcess?.name ?? null);

	// Keep a valid selected process as the list resolves / changes.
	$effect(() => {
		if (processes.length === 0) {
			selectedProcessId = null;
			return;
		}
		if (!selectedProcessId || !processes.some((p) => p.process_id === selectedProcessId)) {
			selectedProcessId = processes[0].process_id;
		}
	});

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

	function openPetri() {
		petriMounted = true;
		mode = 'petri';
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
					<h1 class="shrink-0 text-base font-semibold text-foreground">
						{processName ?? 'Run'}
					</h1>
					<Badge class={statusColors[instance.status] ?? ''} variant="secondary">
						{instance.status}
					</Badge>
					<span class="font-mono text-xs text-muted-foreground truncate">
						{instance.net_id}
					</span>
				</div>
				<div class="flex items-center gap-2 shrink-0">
					<Button variant="ghost" size="sm" href="/templates/{instance.template_id}">
						<FileText class="size-3.5" />
						Template v{instance.template_version}
					</Button>
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
			<div class="mt-1 flex flex-wrap gap-x-4 gap-y-0.5 text-xs text-muted-foreground">
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
		{@render lineage()}

		{#if primaryProcess || hasNet}
			<!-- Mode switch: Process is primary; the Petri net is secondary/debug. -->
			<div
				class="flex items-center gap-1 border-b border-border bg-card px-3 py-1 shrink-0"
			>
				<button
					class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors
						{mode === 'process'
						? 'bg-primary text-primary-foreground'
						: 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
					onclick={() => (mode = 'process')}
				>
					<LayoutDashboard class="size-3.5" />
					Process
				</button>
				{#if hasNet}
					<button
						class="inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs transition-colors
							{mode === 'petri'
							? 'bg-accent text-foreground'
							: 'text-muted-foreground/70 hover:bg-accent hover:text-foreground'}"
						onclick={openPetri}
						title="Engine debug: the raw Petri net for this run"
					>
						<Network class="size-3.5" />
						Petri net
					</button>
				{/if}
			</div>

			<div class="relative flex-1 min-h-0">
				<!-- Process (primary) -->
				<div
					class="absolute inset-0 overflow-y-auto"
					class:hidden={mode !== 'process'}
				>
					{#if primaryProcess && selectedProcessId}
						<div class="mx-auto w-full px-6 py-6">
							{#if processes.length > 1}
								<div class="mb-3 flex flex-wrap items-center gap-1.5 text-xs">
									<span class="text-muted-foreground">Processes:</span>
									{#each processes as p (p.process_id)}
										<button
											class="rounded-md px-2 py-0.5 transition-colors
												{selectedProcessId === p.process_id
												? 'bg-primary text-primary-foreground'
												: 'bg-accent text-muted-foreground hover:text-foreground'}"
											onclick={() => (selectedProcessId = p.process_id)}
										>
											{p.name ?? p.process_id.slice(0, 8)}
										</button>
									{/each}
								</div>
							{/if}
							<ProcessView processId={selectedProcessId} />
						</div>
					{:else}
						<div
							class="flex h-full flex-col items-center justify-center gap-2 py-16 text-sm text-muted-foreground"
						>
							<LayoutDashboard class="size-8 text-muted-foreground/40" />
							<p>No process for this run yet.</p>
							{#if hasNet}
								<button class="text-xs underline hover:text-foreground" onclick={openPetri}>
									Inspect the Petri net
								</button>
							{/if}
						</div>
					{/if}
				</div>

				<!-- Petri net (secondary, lazy + kept alive once opened) -->
				{#if petriMounted && instance.net_id}
					<div class="absolute inset-0" class:hidden={mode !== 'petri'}>
						<NetWorkbench netId={instance.net_id} />
					</div>
				{/if}
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
