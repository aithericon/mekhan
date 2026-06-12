<script lang="ts">
	import History from '@lucide/svelte/icons/history';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import Check from '@lucide/svelte/icons/check';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem
	} from '$lib/components/ui/dropdown-menu';
	import { goto } from '$app/navigation';
	import { getTemplateVersions, type Template } from '$lib/api/client';

	type Props = {
		/** Any version's id in the chain — the backend resolves the whole chain. */
		templateId: string;
		/** Version number of the template currently open in the editor. */
		currentVersion: number;
		/** Which editor surface to navigate back into when picking a version. */
		mode?: 'canvas' | 'ide';
	};

	let { templateId, currentVersion, mode = 'canvas' }: Props = $props();

	let open = $state(false);
	let versions = $state<Template[]>([]);
	let loading = $state(false);
	let loadError = $state<string | null>(null);
	let loadedFor = $state<string | null>(null);

	// Fetch lazily the first time the menu is opened (and refetch if the
	// editor switched to a different version chain). Avoids an extra request
	// on every editor load for templates that only have a single version.
	async function ensureLoaded() {
		if (loading || loadedFor === templateId) return;
		loading = true;
		loadError = null;
		try {
			const all = await getTemplateVersions(templateId);
			versions = [...all].sort((a, b) => b.version - a.version);
			loadedFor = templateId;
		} catch (e) {
			loadError = e instanceof Error ? e.message : 'Failed to load versions';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open) void ensureLoaded();
	});

	function hrefFor(v: Template): string {
		return mode === 'ide' ? `/templates/${v.id}/ide` : `/templates/${v.id}`;
	}

	// In-app navigation: both editor routes key their session-owning component
	// on the `[id]` param, so a param-only `goto` tears the old Yjs session
	// down (WS closed) and mounts the picked version fresh — no full reload.
	function select(v: Template) {
		open = false;
		if (v.version === currentVersion) return;
		void goto(hrefFor(v));
	}

	const fmtDate = (s: string) =>
		new Date(s).toLocaleDateString(undefined, {
			month: 'short',
			day: 'numeric',
			year: 'numeric'
		});
</script>

<DropdownMenu bind:open>
	<DropdownMenuTrigger
		data-testid="btn-version-menu"
		title="Version history"
		class="inline-flex h-6 items-center gap-1 rounded-md border border-border bg-background px-2 text-sm font-medium text-muted-foreground transition-colors hover:bg-accent hover:text-foreground data-[state=open]:bg-accent"
	>
		<History class="size-3" />
		v{currentVersion}
		<ChevronDown class="size-3 opacity-60" />
	</DropdownMenuTrigger>
	<DropdownMenuContent align="start" class="w-64">
		<div class="px-2 py-1.5 text-sm font-medium tracking-wide text-muted-foreground uppercase">
			Version history
		</div>
		{#if loading}
			<div class="px-2 py-3 text-sm text-muted-foreground" data-testid="version-menu-loading">
				Loading versions…
			</div>
		{:else if loadError}
			<div class="px-2 py-3 text-sm text-destructive" data-testid="version-menu-error">
				{loadError}
			</div>
		{:else if versions.length === 0}
			<div class="px-2 py-3 text-sm text-muted-foreground">No versions found</div>
		{:else}
			{#each versions as v (v.id)}
				{@const isCurrent = v.version === currentVersion}
				<DropdownMenuItem
					data-testid="version-item-{v.version}"
					class="flex items-start gap-2"
					onSelect={() => select(v)}
				>
					<span class="mt-0.5 w-3.5 shrink-0">
						{#if isCurrent}<Check class="size-3.5 text-primary" />{/if}
					</span>
					<span class="min-w-0 flex-1">
						<span class="flex items-center gap-1.5">
							<span class="font-medium text-foreground">v{v.version}</span>
							<span
								class="rounded px-1 text-sm {v.published
									? 'bg-green-100 text-green-700'
									: 'bg-amber-100 text-amber-700'}"
							>
								{v.published ? 'Published' : 'Draft'}
							</span>
							{#if v.is_latest}
								<span class="rounded bg-accent px-1 text-sm text-muted-foreground">
									latest
								</span>
							{/if}
						</span>
						<span class="mt-0.5 block text-sm text-muted-foreground">
							{v.published && v.published_at
								? `Published ${fmtDate(v.published_at)}`
								: `Updated ${fmtDate(v.updated_at)}`}
						</span>
					</span>
				</DropdownMenuItem>
			{/each}
		{/if}
	</DropdownMenuContent>
</DropdownMenu>
