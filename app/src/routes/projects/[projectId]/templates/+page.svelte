<script lang="ts">
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Plus from '@lucide/svelte/icons/plus';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription
	} from '$lib/components/ui/card';
	import {
		listTemplates,
		attachTemplateToProject,
		detachTemplateFromProject,
		type Template
	} from '$lib/api/client';
	import { getProjectContext } from '$lib/components/projects/project-context';

	const ctx = getProjectContext();
	const projectId = $derived(ctx.projectId);

	let attached = $state<Template[]>([]);

	// Attach-template state
	let attachQuery = $state('');
	let attachResults = $state<Template[]>([]);
	let attachSearching = $state(false);
	let attachError = $state<string | null>(null);

	// Base id is what project_templates keys on (COALESCE(base_template_id, id)).
	function baseId(t: Template): string {
		return t.base_template_id ?? t.id;
	}

	async function loadAttached() {
		try {
			const res = await listTemplates({ projectId, pageSize: 100 });
			attached = res.items ?? [];
		} catch {
			attached = [];
		}
	}

	$effect(() => {
		if (projectId) loadAttached();
	});

	const attachedBaseIds = $derived(new Set(attached.map(baseId)));

	async function runSearch() {
		const q = attachQuery.trim();
		if (!q) {
			attachResults = [];
			return;
		}
		attachSearching = true;
		attachError = null;
		try {
			const res = await listTemplates({ search: q, pageSize: 10 });
			// Hide already-attached templates from the picker.
			attachResults = (res.items ?? []).filter((t) => !attachedBaseIds.has(baseId(t)));
		} catch (e) {
			attachError = e instanceof Error ? e.message : 'Search failed';
		} finally {
			attachSearching = false;
		}
	}

	async function attach(t: Template) {
		attachError = null;
		try {
			await attachTemplateToProject(projectId, t.id);
			attachResults = attachResults.filter((x) => x.id !== t.id);
			await loadAttached();
		} catch (e) {
			attachError = e instanceof Error ? e.message : 'Failed to attach';
		}
	}

	async function detach(t: Template) {
		if (!confirm(`Detach '${t.name}' from this project?`)) return;
		try {
			await detachTemplateFromProject(projectId, baseId(t));
			attached = attached.filter((x) => x.id !== t.id);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to detach');
		}
	}
</script>

<Card>
	<CardHeader>
		<CardTitle>Templates</CardTitle>
		<CardDescription>
			Templates attached to this project. Their enabled triggers make up the API contract.
		</CardDescription>
	</CardHeader>
	<CardContent class="space-y-4">
		<!-- Attach picker -->
		<div class="space-y-2">
			<form onsubmit={(e) => { e.preventDefault(); runSearch(); }} class="flex gap-2">
				<Input
					placeholder="Search templates to attach…"
					bind:value={attachQuery}
					data-testid="input-attach-search"
				/>
				<Button type="submit" variant="outline" size="sm" disabled={attachSearching}>
					{attachSearching ? 'Searching…' : 'Search'}
				</Button>
			</form>
			{#if attachError}
				<div class="text-sm text-destructive">{attachError}</div>
			{/if}
			{#if attachResults.length > 0}
				<ul class="space-y-1">
					{#each attachResults as t (t.id)}
						<li class="flex items-center justify-between gap-2 rounded-md border border-dashed border-border px-3 py-1.5 text-sm">
							<span class="min-w-0 truncate">
								{t.name} <span class="text-sm text-muted-foreground">v{t.version}</span>
							</span>
							<Button
								variant="ghost"
								size="sm"
								onclick={() => attach(t)}
								data-testid={`btn-attach-${t.id}`}
							>
								<Plus class="size-3.5" /> Attach
							</Button>
						</li>
					{/each}
				</ul>
			{/if}
		</div>

		<!-- Attached templates -->
		<div class="border-t border-border/60 pt-3">
			{#if attached.length === 0}
				<p class="text-sm text-muted-foreground">No templates attached yet.</p>
			{:else}
				<ul class="space-y-2">
					{#each attached as t (t.id)}
						<li
							class="flex items-center justify-between gap-2 rounded-md border border-border bg-card/50 px-3 py-2 text-sm"
							data-testid={`attached-${t.id}`}
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
								title="Detach from project"
								onclick={() => detach(t)}
								data-testid={`btn-detach-${t.id}`}
							>
								<Trash2 class="size-3.5" />
							</Button>
						</li>
					{/each}
				</ul>
			{/if}
		</div>
	</CardContent>
</Card>
