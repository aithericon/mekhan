<script lang="ts">
	import { goto } from '$app/navigation';
	import { createTemplate } from '$lib/api/client';
	import type { WorkflowGraph } from '$lib/types/editor';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Textarea } from '$lib/components/ui/textarea';
	import { PageShell, PageHeader } from '$lib/components/shell';

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
				graph: defaultGraph
			});
			goto(`/templates/${template.id}`);
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to create template';
		} finally {
			submitting = false;
		}
	}
</script>

<PageShell width="narrow" testid="new-template-page">
	{#snippet band()}
		<PageHeader title="New Template" subtitle="Create a new workflow template" />
	{/snippet}

	{#if error}
		<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	<form class="space-y-4" data-testid="new-template-form" onsubmit={handleSubmit}>
		<div class="space-y-1.5">
			<Label for="name">Name</Label>
			<Input
				id="name"
				bind:value={name}
				required
				placeholder="My Workflow"
				data-testid="input-template-name"
			/>
		</div>

		<div class="space-y-1.5">
			<Label for="description">Description</Label>
			<Textarea
				id="description"
				bind:value={description}
				rows={3}
				placeholder="Optional description"
				data-testid="input-template-description"
			/>
		</div>

		<div class="flex items-center gap-3 pt-2">
			<Button
				type="submit"
				disabled={submitting || !name.trim()}
				data-testid="btn-submit-template"
			>
				{submitting ? 'Creating...' : 'Create Template'}
			</Button>
			<Button variant="outline" href="/templates">
				Cancel
			</Button>
		</div>
	</form>
</PageShell>
