<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import type { AllocationResponse } from '$lib/api/clusters';

	type Props = {
		leases: AllocationResponse[];
		loading: boolean;
		/** 'active' = filter to held/pending rows; 'recent' = show all, ordered desc */
		variant?: 'active' | 'recent';
	};

	let { leases, loading, variant = 'recent' }: Props = $props();

	const visibleLeases = $derived(
		variant === 'active'
			? leases.filter((l) => l.status === 'held' || l.status === 'pending')
			: leases
	);

	const title = $derived(variant === 'active' ? 'Active leases' : 'Lease history');

	const allocStatusColor: Record<string, string> = {
		pending: 'bg-amber-100 text-amber-700',
		held: 'bg-green-100 text-green-700',
		released: 'bg-slate-100 text-slate-600',
		failed: 'bg-red-100 text-red-700',
		expired: 'bg-orange-100 text-orange-700'
	};

	function flavorClass(f: string | null | undefined): string {
		if (f === 'slurm') return 'bg-sky-500/15 text-sky-700 dark:text-sky-300';
		if (f === 'nomad') return 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300';
		if (f === 'http') return 'bg-violet-500/15 text-violet-700 dark:text-violet-300';
		return 'bg-muted text-muted-foreground';
	}

	/** Format duration from two ISO timestamps or a pre-computed duration_ms. */
	function duration(row: AllocationResponse): string {
		if (row.duration_ms !== null && row.duration_ms !== undefined) {
			return fmtMs(row.duration_ms);
		}
		if (row.acquired_at && row.released_at) {
			const ms = new Date(row.released_at).getTime() - new Date(row.acquired_at).getTime();
			return fmtMs(ms);
		}
		if (row.acquired_at && row.status === 'held') {
			const ms = Date.now() - new Date(row.acquired_at).getTime();
			return `${fmtMs(ms)} (live)`;
		}
		return '—';
	}

	function fmtMs(ms: number): string {
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
		const mins = Math.floor(ms / 60_000);
		const secs = Math.floor((ms % 60_000) / 1000);
		return `${mins}m ${secs}s`;
	}

	function fmtTs(iso: string | null | undefined): string {
		if (!iso) return '—';
		return new Date(iso).toLocaleString(undefined, {
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		});
	}
</script>

<section class="space-y-2">
	<h2 class="text-sm font-semibold tracking-tight text-foreground">{title}</h2>

	{#if loading && visibleLeases.length === 0}
		<p class="text-sm text-muted-foreground">Loading...</p>
	{:else if visibleLeases.length === 0}
		<div class="rounded-md border border-dashed border-border/60 px-4 py-6 text-center">
			<p class="text-sm text-muted-foreground">
				{variant === 'active' ? 'No active leases.' : 'No leases in window.'}
			</p>
		</div>
	{:else}
		<div class="overflow-x-auto rounded-md border border-border/60">
			<table class="w-full text-sm">
				<thead class="bg-muted/40 text-left text-xs text-muted-foreground">
					<tr>
						<th class="px-3 py-2 font-medium">Status</th>
						<th class="px-3 py-2 font-medium">Alloc / grant</th>
						<th class="px-3 py-2 font-medium">Flavor</th>
						<th class="px-3 py-2 font-medium">Node</th>
						<th class="px-3 py-2 font-medium">Acquired</th>
						<th class="px-3 py-2 font-medium">Released</th>
						<th class="px-3 py-2 font-medium">Duration</th>
						{#if variant === 'recent'}
							<th class="px-3 py-2 font-medium">CPU-h</th>
							<th class="px-3 py-2 font-medium">GPU-h</th>
							<th class="px-3 py-2 font-medium">Exit</th>
						{/if}
						<th class="px-3 py-2 font-medium">Instance</th>
					</tr>
				</thead>
				<tbody class="divide-y divide-border/60">
					{#each visibleLeases as row (row.id)}
						{@const statusCls = allocStatusColor[row.status] ?? 'bg-gray-100 text-gray-700'}
						<tr class="hover:bg-muted/30">
							<td class="px-3 py-2">
								<Badge class="{statusCls} font-normal" variant="secondary">{row.status}</Badge>
							</td>
							<td class="px-3 py-2">
								<div class="font-mono text-sm text-foreground">
									{#if row.alloc_id}
										<span title={row.alloc_id}>{row.alloc_id.slice(0, 12)}&hellip;</span>
									{:else}
										<span class="text-muted-foreground">—</span>
									{/if}
								</div>
								<div class="font-mono text-sm text-muted-foreground" title={row.grant_id}>
									{row.grant_id.slice(0, 16)}&hellip;
								</div>
							</td>
							<td class="px-3 py-2">
								{#if row.scheduler_flavor}
									<Badge variant="secondary" class={flavorClass(row.scheduler_flavor)}>
										{row.scheduler_flavor}
									</Badge>
								{:else}
									<span class="text-muted-foreground">—</span>
								{/if}
							</td>
							<td class="px-3 py-2 font-mono text-sm">
								{row.node ?? '—'}
							</td>
							<td class="px-3 py-2 text-sm text-muted-foreground">
								{fmtTs(row.acquired_at)}
							</td>
							<td class="px-3 py-2 text-sm text-muted-foreground">
								{fmtTs(row.released_at)}
							</td>
							<td class="px-3 py-2 font-mono text-sm tabular-nums">
								{duration(row)}
							</td>
							{#if variant === 'recent'}
								<td class="px-3 py-2 font-mono text-sm tabular-nums text-muted-foreground">
									{row.cpu_seconds !== null && row.cpu_seconds !== undefined
										? (row.cpu_seconds / 3600).toFixed(3)
										: '—'}
								</td>
								<td class="px-3 py-2 font-mono text-sm tabular-nums text-muted-foreground">
									{row.gpu_seconds !== null && row.gpu_seconds !== undefined
										? (row.gpu_seconds / 3600).toFixed(3)
										: '—'}
								</td>
								<td class="px-3 py-2 font-mono text-sm tabular-nums">
									{#if row.exit_code !== null && row.exit_code !== undefined}
										<span class={row.exit_code === 0 ? 'text-emerald-600' : 'text-rose-600'}>
											{row.exit_code}
										</span>
									{:else}
										<span class="text-muted-foreground">—</span>
									{/if}
								</td>
							{/if}
							<td class="px-3 py-2">
								{#if row.instance_id}
									<a
										href="/instances/{row.instance_id}"
										class="font-mono text-sm text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
										title={row.instance_id}
									>
										{row.instance_id.slice(0, 8)}&hellip;
									</a>
								{:else}
									<span class="text-muted-foreground">—</span>
								{/if}
							</td>
						</tr>
						{#if row.last_error}
							<tr class="bg-destructive/5">
								<td colspan={variant === 'recent' ? 11 : 8} class="px-3 py-1.5">
									<span class="font-mono text-sm text-destructive">{row.last_error}</span>
								</td>
							</tr>
						{/if}
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</section>
