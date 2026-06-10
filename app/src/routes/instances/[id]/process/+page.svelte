<script lang="ts">
	import { ProcessView } from '$lib/components/processes';
	import { FilterPills } from '$lib/components/shell';
	import { useInstanceContext } from '$lib/components/instances/instance-context';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';

	const ctx = useInstanceContext();

	let selectedProcessId = $state<string | null>(null);

	// Keep a valid selected process as the list resolves / changes.
	$effect(() => {
		if (ctx.processes.length === 0) {
			selectedProcessId = null;
			return;
		}
		if (
			!selectedProcessId ||
			!ctx.processes.some((p) => p.process_id === selectedProcessId)
		) {
			selectedProcessId = ctx.processes[0].process_id;
		}
	});

	const primaryProcess = $derived(ctx.processes[0] ?? null);
	const hasNet = $derived(
		!!ctx.instance && ctx.instance.status !== 'created' && !!ctx.instance.net_id
	);
</script>

<div class="absolute inset-0 overflow-y-auto">
	{#if primaryProcess && selectedProcessId}
		<div class="mx-auto w-full px-6 py-6">
			{#if ctx.processes.length > 1}
				<div class="mb-3 flex flex-wrap items-center gap-1.5 text-sm">
					<span class="text-muted-foreground">Processes:</span>
					<FilterPills
						active={selectedProcessId}
						onSelect={(v) => (selectedProcessId = v)}
						options={ctx.processes.map((p) => ({
							value: p.process_id,
							label: p.name ?? p.process_id.slice(0, 8)
						}))}
					/>
				</div>
			{/if}
			<ProcessView processId={selectedProcessId} instance={ctx.instance} />
		</div>
	{:else}
		<div
			class="flex h-full flex-col items-center justify-center gap-2 py-16 text-sm text-muted-foreground"
		>
			<LayoutDashboard class="size-8 text-muted-foreground/40" />
			<p>No process for this run yet.</p>
			{#if hasNet}
				<a
					class="text-sm underline hover:text-foreground"
					href="/instances/{ctx.instanceId}/petri-net"
				>
					Inspect the Petri net
				</a>
			{/if}
		</div>
	{/if}
</div>
