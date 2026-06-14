<!--
  InsertArtifactDialog — the toolbar "Insert media" picker for the Report editor.

  Inserts an `artifactEmbed` block referencing one of the run's processes. The
  block embeds ALL renderable media for that process (grouped + scrubbable by the
  reused ArtifactsPanel), so the only choices here are which process (when the run
  has more than one) and an optional caption.
-->
<script lang="ts">
	import type { Editor } from '@tiptap/core';
	import * as Dialog from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import type { ArtifactEmbedContext } from './embed-context';

	let {
		open = $bindable(),
		editor,
		context
	}: {
		open: boolean;
		editor: Editor | null;
		context: ArtifactEmbedContext;
	} = $props();

	let processId = $state<string>('');
	let caption = $state('');

	// Default to the first process once the dialog opens / processes load.
	$effect(() => {
		if (open && !processId && context.processes.length > 0) {
			processId = context.processes[0].id;
		}
	});

	function insert() {
		if (!editor || !processId) return;
		const proc = context.processes.find((p) => p.id === processId);
		editor
			.chain()
			.focus()
			.insertContent({
				type: 'artifactEmbed',
				attrs: { processId, processName: proc?.name ?? '', caption: caption.trim() }
			})
			.run();
		open = false;
		caption = '';
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="sm:max-w-md">
		<Dialog.Header>
			<Dialog.Title>Embed run media</Dialog.Title>
			<Dialog.Description>
				Insert a live panel of this run's renderable artifacts — images, plots, video.
				It updates as the run produces more.
			</Dialog.Description>
		</Dialog.Header>

		<div class="flex flex-col gap-4 py-2">
			{#if context.processes.length === 0}
				<p class="text-sm text-muted-foreground">
					This run has no processes yet — start the run, then embed its media.
				</p>
			{:else}
				{#if context.processes.length > 1}
					<div class="flex flex-col gap-1.5">
						<Label for="embed-process">Process</Label>
						<select
							id="embed-process"
							bind:value={processId}
							class="border-input bg-background ring-offset-background focus-visible:ring-ring h-9 rounded-md border px-3 text-sm focus-visible:outline-none focus-visible:ring-2"
						>
							{#each context.processes as p (p.id)}
								<option value={p.id}>{p.name}</option>
							{/each}
						</select>
					</div>
				{/if}
				<div class="flex flex-col gap-1.5">
					<Label for="embed-caption">Caption (optional)</Label>
					<Input id="embed-caption" bind:value={caption} placeholder="e.g. Final renders" />
				</div>
			{/if}
		</div>

		<Dialog.Footer>
			<Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
			<Button onclick={insert} disabled={!processId}>Insert</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
