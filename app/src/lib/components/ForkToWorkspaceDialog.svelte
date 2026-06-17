<script lang="ts">
	// Destination picker for "Fork to workspace". Only shown when the caller can
	// write to MORE THAN ONE workspace — a single-workspace user forks straight
	// in (the caller decides; this dialog just collects the target id).
	import GitFork from '@lucide/svelte/icons/git-fork';
	import {
		Dialog,
		DialogContent,
		DialogHeader,
		DialogTitle,
		DialogDescription,
		DialogFooter
	} from '$lib/components/ui/dialog';
	import { RadioGroup, RadioGroupItem } from '$lib/components/ui/radio-group';
	import { Button } from '$lib/components/ui/button';
	import type { WorkspaceSummary } from '$lib/api/client';

	let {
		open = $bindable(false),
		itemName,
		options,
		defaultId,
		submitting = false,
		onConfirm
	}: {
		open?: boolean;
		/** Name of the template/folder being forked — shown in the title. */
		itemName: string;
		/** Writable destination workspaces. */
		options: WorkspaceSummary[];
		/** Pre-selected workspace id. */
		defaultId?: string;
		submitting?: boolean;
		onConfirm: (workspaceId: string) => void;
	} = $props();

	let selected = $state('');
	// Reset the selection each time the dialog opens.
	$effect(() => {
		if (open) selected = defaultId ?? options[0]?.id ?? '';
	});
</script>

<Dialog bind:open>
	<DialogContent class="sm:max-w-md">
		<DialogHeader>
			<DialogTitle>Fork “{itemName}”</DialogTitle>
			<DialogDescription>Choose a workspace to copy it into.</DialogDescription>
		</DialogHeader>

		<RadioGroup bind:value={selected} class="py-1" data-testid="fork-workspace-options">
			{#each options as ws (ws.id)}
				<label
					class="flex cursor-pointer items-center gap-3 rounded-md border border-border px-3 py-2 transition-colors hover:bg-accent/50 has-[:focus-visible]:ring-2 has-[:focus-visible]:ring-ring"
					data-testid="fork-workspace-option-{ws.slug}"
				>
					<RadioGroupItem value={ws.id} />
					<span class="min-w-0 flex-1">
						<span class="block truncate text-sm">{ws.display_name}</span>
						<span class="block truncate text-xs text-muted-foreground">{ws.slug}</span>
					</span>
				</label>
			{/each}
		</RadioGroup>

		<DialogFooter>
			<Button variant="outline" onclick={() => (open = false)} disabled={submitting}>Cancel</Button>
			<Button
				disabled={!selected || submitting}
				onclick={() => onConfirm(selected)}
				data-testid="fork-workspace-confirm"
			>
				<GitFork class="size-4" />
				{submitting ? 'Forking…' : 'Fork here'}
			</Button>
		</DialogFooter>
	</DialogContent>
</Dialog>
