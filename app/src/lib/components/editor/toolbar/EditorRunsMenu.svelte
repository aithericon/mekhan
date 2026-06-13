<script lang="ts">
	import Activity from '@lucide/svelte/icons/activity';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import { goto } from '$app/navigation';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem,
		DropdownMenuSeparator
	} from '$lib/components/ui/dropdown-menu';
	import { listInstances, type InstanceListItem } from '$lib/api/client';
	import { StatusBadge } from '$lib/components/status';
	import { allRunsHref, runWhenLabel, RUNS_MENU_LIMIT } from './runs-menu';

	type Props = {
		/** Template version-chain family id — the menu lists runs of EVERY
		 *  version, so a draft still sees runs of its published ancestors. */
		familyId: string;
	};

	let { familyId }: Props = $props();

	let open = $state(false);
	let runs = $state<InstanceListItem[]>([]);
	let loading = $state(false);
	let loadError = $state<string | null>(null);

	// Refetch on EVERY open (unlike TemplateVersionMenu's load-once): runs
	// accumulate while the editor is open, so a cached page would hide the run
	// the user just launched. Nothing is fetched until the menu first opens.
	async function loadRuns() {
		loading = true;
		loadError = null;
		try {
			runs = (
				await listInstances({
					templateFamily: familyId,
					mode: 'any',
					perPage: RUNS_MENU_LIMIT
				})
			).items;
		} catch (e) {
			loadError = e instanceof Error ? e.message : 'Failed to load runs';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open) void loadRuns();
	});
</script>

<DropdownMenu bind:open>
	<DropdownMenuTrigger
		data-testid="btn-runs-menu"
		title="Recent runs of this workflow (all versions)"
		class="inline-flex h-8 items-center gap-1.5 rounded-md px-3 text-sm font-medium text-foreground transition-colors hover:bg-accent hover:text-accent-foreground data-[state=open]:bg-accent"
	>
		<Activity class="size-3.5" />
		Runs
		<ChevronDown class="size-3 opacity-60" />
	</DropdownMenuTrigger>
	<DropdownMenuContent align="end" class="w-64">
		<div class="px-2 py-1.5 text-sm font-medium tracking-wide text-muted-foreground uppercase">
			Recent runs
		</div>
		{#if loading && runs.length === 0}
			<div class="px-2 py-3 text-sm text-muted-foreground" data-testid="runs-menu-loading">
				Loading runs…
			</div>
		{:else if loadError}
			<div class="px-2 py-3 text-sm text-destructive" data-testid="runs-menu-error">
				{loadError}
			</div>
		{:else if runs.length === 0}
			<div class="px-2 py-3 text-sm text-muted-foreground" data-testid="runs-menu-empty">
				No runs yet
			</div>
		{:else}
			{#each runs as run (run.id)}
				<DropdownMenuItem
					data-testid="runs-menu-item-{run.id}"
					class="flex items-start gap-2"
					onSelect={() => goto(`/instances/${run.id}`)}
				>
					<span class="min-w-0 flex-1">
						<span class="flex items-center gap-1.5">
							<StatusBadge domain="workflow" status={run.status} />
							<span class="text-sm text-muted-foreground">v{run.template_version}</span>
							{#if run.mode !== 'live'}
								<span class="rounded bg-accent px-1 text-sm text-muted-foreground">
									{run.mode === 'test_run' ? 'test' : run.mode}
								</span>
							{/if}
						</span>
						<span class="mt-0.5 block text-sm text-muted-foreground">
							{runWhenLabel(run)}
						</span>
					</span>
				</DropdownMenuItem>
			{/each}
		{/if}
		<DropdownMenuSeparator />
		<DropdownMenuItem data-testid="runs-menu-view-all" onSelect={() => goto(allRunsHref(familyId))}>
			View all runs
		</DropdownMenuItem>
	</DropdownMenuContent>
</DropdownMenu>
