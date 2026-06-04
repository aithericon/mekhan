<script lang="ts">
	// Fleet → Interfaces catalog.
	// A runner picker (left) → on select, GET the runner's self-reported ROS
	// interface catalog (topics/services/actions) and render it read-only,
	// grouped into three tables (name + type columns). Mirrors the data-loading
	// idiom of RunnerList/PresenceBoard: load the runner list on mount, then
	// fetch the catalog on selection. A 404 (no catalog ever pushed) surfaces as
	// a friendly "no catalog reported yet" state, not an error.
	import Server from '@lucide/svelte/icons/server';
	import Radio from '@lucide/svelte/icons/radio';
	import { Badge } from '$lib/components/ui/badge';
	import {
		listRunners,
		getRunnerPresence,
		getRunnerInterfaces,
		type RunnerSummary,
		type RunnerPresenceSnapshot,
		type RunnerInterfaces,
		type InterfaceEntry
	} from '$lib/api/runners';
	import { fmtDate } from './format';
	import { filterFleetByGroup } from './grouping';
	import StatusDot from './StatusDot.svelte';
	import FleetEmpty from './FleetEmpty.svelte';

	type Props = {
		/** When set, only show runners in this group alias (the capacity `path`).
		 *  Omitted ⇒ every runner advertising an interface catalog (the default). */
		group?: string | null;
	};
	let { group = null }: Props = $props();

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	let selectedId = $state<string | null>(null);
	let catalog = $state<RunnerInterfaces | null>(null);
	let catalogLoading = $state(false);
	let catalogError = $state<string | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	const presenceById = $derived(Object.fromEntries(presence.map((p) => [p.runner_id, p])));

	const selectedRunner = $derived(runners.find((r) => r.id === selectedId) ?? null);

	/** The three interface groups in render order, with their entries. */
	const groups = $derived<{ label: string; entries: InterfaceEntry[] }[]>(
		catalog
			? [
					{ label: 'Topics', entries: catalog.catalog.topics ?? [] },
					{ label: 'Services', entries: catalog.catalog.services ?? [] },
					{ label: 'Actions', entries: catalog.catalog.actions ?? [] }
				]
			: []
	);

	const totalEntries = $derived(groups.reduce((n, g) => n + g.entries.length, 0));

	// ── Load ───────────────────────────────────────────────────────────────────

	async function loadRunners() {
		loading = true;
		error = null;
		try {
			const [rPage, pSnaps] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence()
			]);
			// When scoped to a group, keep only that group's runners.
			runners = filterFleetByGroup(rPage.items, [], group).runners;
			presence = pSnaps;
			// Auto-select the first runner so the panel isn't empty on open.
			if (selectedId === null && runners.length > 0) {
				void select(runners[0].id);
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load runners';
			runners = [];
		} finally {
			loading = false;
		}
	}

	async function select(id: string) {
		selectedId = id;
		catalog = null;
		catalogError = null;
		catalogLoading = true;
		try {
			// getRunnerInterfaces resolves to `null` on a 404 (no catalog yet) —
			// that's a friendly empty state, not an error.
			catalog = await getRunnerInterfaces(id);
		} catch (e) {
			catalogError = e instanceof Error ? e.message : 'Failed to load catalog';
		} finally {
			catalogLoading = false;
		}
	}

	$effect(() => {
		void loadRunners();
	});
</script>

<div class="space-y-4" data-testid="interfaces-catalog">
	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	{#if loading}
		<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
	{:else if runners.length === 0}
		<FleetEmpty message="No runners enrolled yet.">
			{#snippet icon()}<Server class="size-10 text-muted-foreground/40" />{/snippet}
		</FleetEmpty>
	{:else}
		<div class="grid gap-6 md:grid-cols-[260px_1fr]">
			<!-- ── Runner picker ──────────────────────────────────────────────────── -->
			<aside class="space-y-1" data-testid="interfaces-runner-picker">
				<h3 class="mb-2 px-1 text-sm font-medium text-muted-foreground">Runners</h3>
				{#each runners as runner (runner.id)}
					{@const online = presenceById[runner.id]?.present ?? false}
					<button
						type="button"
						onclick={() => select(runner.id)}
						class="flex w-full items-center gap-2 rounded-lg border px-3 py-2 text-left text-sm transition-colors
							{selectedId === runner.id
							? 'border-primary/50 bg-accent/60 text-foreground'
							: 'border-border bg-card text-muted-foreground hover:bg-accent/40'}"
						data-testid="interfaces-runner-{runner.id}"
					>
						<StatusDot tone={online ? 'live' : 'idle'} />
						<span class="min-w-0 flex-1 truncate font-medium text-foreground">{runner.name}</span>
					</button>
				{/each}
			</aside>

			<!-- ── Catalog panel ──────────────────────────────────────────────────── -->
			<section class="min-w-0" data-testid="interfaces-panel">
				{#if !selectedRunner}
					<p class="py-16 text-center text-sm text-muted-foreground">
						Select a runner to view its interface catalog.
					</p>
				{:else}
					<div class="mb-4 flex flex-wrap items-baseline justify-between gap-2">
						<div class="min-w-0">
							<h2 class="truncate text-lg font-semibold text-foreground">{selectedRunner.name}</h2>
							<p class="truncate font-mono text-sm text-muted-foreground">{selectedRunner.id}</p>
						</div>
						{#if catalog}
							<div class="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
								{#if catalog.catalog_version}
									<Badge variant="outline">v{catalog.catalog_version}</Badge>
								{/if}
								<span>Discovered {fmtDate(catalog.discovered_at)}</span>
							</div>
						{/if}
					</div>

					{#if catalogError}
						<div
							class="rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
						>
							{catalogError}
						</div>
					{:else if catalogLoading}
						<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
							Loading catalog…
						</div>
					{:else if !catalog || totalEntries === 0}
						<FleetEmpty
							message="No catalog reported yet."
							hint="This runner has not self-reported any ROS topics, services, or actions."
						>
							{#snippet icon()}<Radio class="size-10 text-muted-foreground/40" />{/snippet}
						</FleetEmpty>
					{:else}
						<div class="space-y-6">
							{#each groups as group (group.label)}
								<div data-testid="interfaces-group-{group.label.toLowerCase()}">
									<div class="mb-2 flex items-center gap-2">
										<h3 class="text-sm font-medium text-foreground">{group.label}</h3>
										<Badge variant="secondary" class="text-sm">{group.entries.length}</Badge>
									</div>
									{#if group.entries.length === 0}
										<p class="px-1 py-1 text-sm text-muted-foreground">
											No {group.label.toLowerCase()} advertised.
										</p>
									{:else}
										<div class="overflow-hidden rounded-lg border border-border">
											<table class="w-full text-sm">
												<thead class="bg-muted/50 text-muted-foreground">
													<tr>
														<th class="px-3 py-2 text-left font-medium">Name</th>
														<th class="px-3 py-2 text-left font-medium">Type</th>
													</tr>
												</thead>
												<tbody>
													{#each group.entries as entry (entry.name)}
														<tr class="border-t border-border">
															<td class="px-3 py-2 font-mono text-foreground">{entry.name}</td>
															<td class="px-3 py-2 font-mono text-muted-foreground">{entry.type}</td>
														</tr>
													{/each}
												</tbody>
											</table>
										</div>
									{/if}
								</div>
							{/each}
						</div>
					{/if}
				{/if}
			</section>
		</div>
	{/if}
</div>
