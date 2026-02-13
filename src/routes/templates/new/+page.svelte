<script lang="ts">
	import { goto } from '$app/navigation';
	import { createTemplate } from '$lib/api/client';
	import type { WorkflowGraph } from '$lib/types/editor';

	let name = $state('');
	let description = $state('');
	let submitting = $state(false);
	let error = $state<string | null>(null);

	const defaultGraph: WorkflowGraph = {
		nodes: [
			{
				id: 'node-start',
				type: 'start',
				position: { x: 100, y: 200 },
				data: { type: 'start', label: 'Start' }
			},
			{
				id: 'node-end',
				type: 'end',
				position: { x: 500, y: 200 },
				data: { type: 'end', label: 'End' }
			}
		],
		edges: []
	};

	async function handleSubmit(e: SubmitEvent) {
		e.preventDefault();
		if (!name.trim()) return;

		submitting = true;
		error = null;
		try {
			const template = await createTemplate({
				name: name.trim(),
				description: description.trim(),
				graph: defaultGraph,
				author_id: 'default-user'
			});
			goto(`/templates/${template.id}`);
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to create template';
		} finally {
			submitting = false;
		}
	}
</script>

<div class="h-full overflow-y-auto" data-testid="new-template-page">
	<div class="mx-auto max-w-lg px-6 py-8">
		<h1 class="text-2xl font-semibold tracking-tight text-foreground">New Template</h1>
		<p class="mt-1 text-sm text-muted-foreground">
			Create a new workflow template
		</p>

		{#if error}
			<div class="mt-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{/if}

		<form class="mt-6 space-y-4" data-testid="new-template-form" onsubmit={handleSubmit}>
			<div class="space-y-1.5">
				<label for="name" class="text-sm font-medium text-foreground">Name</label>
				<input
					id="name"
					type="text"
					bind:value={name}
					required
					placeholder="My Workflow"
					data-testid="input-template-name"
					class="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:border-ring focus:outline-none"
				/>
			</div>

			<div class="space-y-1.5">
				<label for="description" class="text-sm font-medium text-foreground">Description</label>
				<textarea
					id="description"
					bind:value={description}
					rows={3}
					placeholder="Optional description"
					data-testid="input-template-description"
					class="w-full rounded-lg border border-input bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:border-ring focus:outline-none"
				></textarea>
			</div>

			<div class="flex items-center gap-3 pt-2">
				<button
					type="submit"
					disabled={submitting || !name.trim()}
					data-testid="btn-submit-template"
					class="rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90 disabled:opacity-50"
				>
					{submitting ? 'Creating...' : 'Create Template'}
				</button>
				<a
					href="/templates"
					class="rounded-lg border border-border px-4 py-2 text-sm font-medium text-foreground transition-colors hover:bg-accent"
				>
					Cancel
				</a>
			</div>
		</form>
	</div>
</div>
