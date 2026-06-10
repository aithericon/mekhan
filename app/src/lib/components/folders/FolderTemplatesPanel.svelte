<script lang="ts">
	// Templates homed in a folder + the "move a template here" picker. Body of
	// the Templates tab in the folders browser's detail pane (one-home model —
	// not the old many-to-many project membership).
	import X from '@lucide/svelte/icons/x';
	import Plus from '@lucide/svelte/icons/plus';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import { listTemplates, setTemplateFolder, type Template } from '$lib/api/client';

	type Props = {
		folderId: string;
	};
	let { folderId }: Props = $props();

	// `recursive` stays false: only direct members here.
	let homed = $state<Template[]>([]);

	// Add-template state — set the picked template's home folder to this one.
	let addQuery = $state('');
	let addResults = $state<Template[]>([]);
	let addSearching = $state(false);
	let addError = $state<string | null>(null);

	async function loadHomed() {
		if (!folderId) return;
		try {
			const res = await listTemplates({ folderId, pageSize: 100 });
			homed = res.items ?? [];
		} catch {
			homed = [];
		}
	}

	$effect(() => {
		if (folderId) loadHomed();
	});

	const homedIds = $derived(new Set(homed.map((t) => t.id)));

	async function runSearch() {
		const q = addQuery.trim();
		if (!q) {
			addResults = [];
			return;
		}
		addSearching = true;
		addError = null;
		try {
			const res = await listTemplates({ search: q, pageSize: 10 });
			// Hide templates already homed here from the picker.
			addResults = (res.items ?? []).filter((t) => !homedIds.has(t.id));
		} catch (e) {
			addError = e instanceof Error ? e.message : 'Search failed';
		} finally {
			addSearching = false;
		}
	}

	async function setHome(t: Template) {
		addError = null;
		try {
			await setTemplateFolder(t.id, { folder_id: folderId });
			addResults = addResults.filter((x) => x.id !== t.id);
			await loadHomed();
		} catch (e) {
			addError = e instanceof Error ? e.message : 'Failed to move template here';
		}
	}

	async function clearHome(t: Template) {
		if (!confirm(`Move '${t.name}' out of this folder (to the workspace root)?`)) return;
		try {
			await setTemplateFolder(t.id, { folder_id: null });
			homed = homed.filter((x) => x.id !== t.id);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to move template out');
		}
	}
</script>

<div class="space-y-4">
	<p class="text-sm text-muted-foreground">
		Templates homed in this folder. Their enabled triggers make up the API contract.
	</p>

	<!-- Add picker -->
	<div class="space-y-2">
		<form onsubmit={(e) => { e.preventDefault(); runSearch(); }} class="flex gap-2">
			<Input
				placeholder="Search templates to move here…"
				bind:value={addQuery}
				data-testid="input-add-search"
			/>
			<Button type="submit" variant="outline" size="sm" disabled={addSearching}>
				{addSearching ? 'Searching…' : 'Search'}
			</Button>
		</form>
		{#if addError}
			<div class="text-sm text-destructive">{addError}</div>
		{/if}
		{#if addResults.length > 0}
			<ul class="space-y-1">
				{#each addResults as t (t.id)}
					<li class="flex items-center justify-between gap-2 rounded-md border border-dashed border-border px-3 py-1.5 text-sm">
						<span class="min-w-0 truncate">
							{t.name} <span class="text-sm text-muted-foreground">v{t.version}</span>
						</span>
						<Button
							variant="ghost"
							size="sm"
							onclick={() => setHome(t)}
							data-testid={`btn-add-${t.id}`}
						>
							<Plus class="size-3.5" /> Move here
						</Button>
					</li>
				{/each}
			</ul>
		{/if}
	</div>

	<!-- Homed templates -->
	<div class="border-t border-border/60 pt-3">
		{#if homed.length === 0}
			<p class="text-sm text-muted-foreground">No templates homed here yet.</p>
		{:else}
			<ul class="space-y-2">
				{#each homed as t (t.id)}
					<li
						class="flex items-center justify-between gap-2 rounded-md border border-border bg-card/50 px-3 py-2 text-sm"
						data-testid={`homed-${t.id}`}
					>
						<a href={`/templates/${t.id}`} class="min-w-0 flex-1 hover:underline">
							<span class="font-medium">{t.name}</span>
							<span class="ml-2 text-sm text-muted-foreground">v{t.version}</span>
							{#if !t.published}
								<Badge variant="outline" class="ml-2 text-sm">draft</Badge>
							{/if}
						</a>
						<Button
							variant="ghost"
							size="sm"
							title="Move out of this folder"
							onclick={() => clearHome(t)}
							data-testid={`btn-remove-${t.id}`}
						>
							<X class="size-3.5" />
						</Button>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>
