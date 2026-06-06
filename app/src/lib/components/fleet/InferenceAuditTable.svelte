<script lang="ts">
	// Read-only inference audit ledger for the Control Plane. Inference bypasses
	// the engine net (the HTTP router meters directly), so `GET
	// /api/v1/inference/requests` is the only durable record of who served what,
	// with which token counts and outcome. We render the newest rows as a compact
	// table; empty → FleetEmpty.
	//
	// All presentation logic (status→badge, token formatting, id truncation) lives
	// in the pure `./inference-audit` helpers so it can be unit-tested without the
	// DOM (house style — see model-pool.ts / grouping.ts).
	import { onMount } from 'svelte';
	import Activity from '@lucide/svelte/icons/activity';
	import { listInferenceRequests, type InferenceRequestLogRow } from '$lib/api/inference';
	import { Badge } from '$lib/components/ui/badge';
	import FleetEmpty from './FleetEmpty.svelte';
	import { fmtDate } from './format';
	import { shortId, fmtTokens, statusVariant } from './inference-audit';

	let {
		// Optional: scope to a single workflow instance's requests.
		instanceId,
		limit = 50,
		// Pre-supplied rows make the component trivially testable without mocking
		// the network; when null it self-loads on mount.
		rows: rowsProp = null
	}: {
		instanceId?: string;
		limit?: number;
		rows?: InferenceRequestLogRow[] | null;
	} = $props();

	// Snapshot the prop once: this component either renders a fixed `rows` set
	// (tests) or self-loads on mount — it never reactively follows the prop, so
	// we capture the initial value into local state and own it thereafter.
	// svelte-ignore state_referenced_locally
	const initialRows = rowsProp;
	let rows = $state<InferenceRequestLogRow[]>(initialRows ?? []);
	let loading = $state(initialRows === null);
	let error = $state<string | null>(null);

	onMount(() => {
		if (initialRows !== null) return;
		void load();
	});

	async function load() {
		loading = true;
		try {
			rows = await listInferenceRequests({ instanceId, limit });
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch inference requests';
		} finally {
			loading = false;
		}
	}
</script>

<div data-testid="inference-audit" class="space-y-3">
	{#if error}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
		>
			{error}
		</div>
	{:else if loading}
		<p class="text-sm text-muted-foreground">Loading inference audit…</p>
	{:else if rows.length === 0}
		<FleetEmpty message="No inference requests recorded yet.">
			{#snippet icon()}<Activity class="size-10 text-muted-foreground/40" />{/snippet}
		</FleetEmpty>
	{:else}
		<div class="overflow-x-auto rounded-xl border border-border">
			<table class="w-full text-sm" data-testid="inference-audit-table">
				<thead>
					<tr class="border-b border-border text-left text-muted-foreground">
						<th class="px-3 py-2 font-medium">Started</th>
						<th class="px-3 py-2 font-medium">Model</th>
						<th class="px-3 py-2 font-medium">Instance</th>
						<th class="px-3 py-2 font-medium">Step</th>
						<th class="px-3 py-2 text-right font-medium">Tokens (in/out/total)</th>
						<th class="px-3 py-2 font-medium">Status</th>
					</tr>
				</thead>
				<tbody>
					{#each rows as row (row.request_id)}
						{@const t = fmtTokens(row)}
						<tr class="border-b border-border/50 last:border-0" data-testid="inference-audit-row">
							<td class="whitespace-nowrap px-3 py-2 text-muted-foreground tabular-nums">
								{fmtDate(row.started_at)}
							</td>
							<td class="px-3 py-2 font-medium text-foreground">{row.model_id}</td>
							<td
								class="max-w-[10rem] truncate px-3 py-2 font-mono text-xs text-muted-foreground"
								title={row.instance_id ?? undefined}
							>
								{shortId(row.instance_id)}
							</td>
							<td class="px-3 py-2 text-muted-foreground">{shortId(row.step_id)}</td>
							<td class="whitespace-nowrap px-3 py-2 text-right tabular-nums text-muted-foreground">
								{t.prompt}/{t.completion}/<span class="text-foreground">{t.total}</span>
							</td>
							<td class="px-3 py-2">
								<Badge variant={statusVariant(row.status)} size="xs">{row.status}</Badge>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>
