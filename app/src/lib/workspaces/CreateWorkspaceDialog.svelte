<script lang="ts">
	import Building from '@lucide/svelte/icons/building';
	import {
		Dialog,
		DialogContent,
		DialogHeader,
		DialogTitle,
		DialogDescription,
		DialogFooter
	} from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { ApiError } from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	let { open = $bindable(false) }: { open?: boolean } = $props();

	let displayName = $state('');
	let slug = $state('');
	let submitting = $state(false);
	let error = $state<string | null>(null);

	// Mirror the server slugifier so the user sees the slug they'll actually
	// get. Empty `slug` field → derive from the name (same as the backend).
	function slugify(input: string): string {
		return input
			.toLowerCase()
			.replace(/[^a-z0-9]+/g, '-')
			.replace(/^-+|-+$/g, '')
			.slice(0, 63)
			.replace(/-+$/g, '');
	}
	const previewSlug = $derived(slugify(slug.trim() || displayName));

	function reset() {
		displayName = '';
		slug = '';
		error = null;
		submitting = false;
	}

	// Reset whenever the dialog opens fresh.
	$effect(() => {
		if (open) reset();
	});

	async function submit(e: SubmitEvent) {
		e.preventDefault();
		if (submitting) return;
		const name = displayName.trim();
		if (!name) {
			error = 'Give your workspace a name.';
			return;
		}
		if (!previewSlug) {
			error = 'Add letters or digits — the name must produce a URL slug.';
			return;
		}
		submitting = true;
		error = null;
		try {
			const ws = await workspaces.create({
				display_name: name,
				slug: slug.trim() || null
			});
			// Drop the user into their new tenant (sets cookie + hard reload).
			await workspaces.switchTo(ws.id);
		} catch (err) {
			submitting = false;
			if (err instanceof ApiError) {
				error =
					err.status === 409
						? `The slug “${previewSlug}” is already taken — pick another.`
						: (err.body.error ?? 'Could not create workspace.');
			} else {
				error = 'Could not create workspace.';
			}
		}
	}
</script>

<Dialog bind:open>
	<DialogContent class="sm:max-w-md">
		<DialogHeader>
			<DialogTitle class="flex items-center gap-2">
				<Building class="size-4" />
				New workspace
			</DialogTitle>
			<DialogDescription>
				A workspace is an isolated tenant — its own templates, data, runs, and
				members. You'll be its owner.
			</DialogDescription>
		</DialogHeader>

		<form onsubmit={submit} class="space-y-4">
			<div class="space-y-1.5">
				<Label for="ws-name">Name</Label>
				<Input
					id="ws-name"
					bind:value={displayName}
					placeholder="Acme Robotics"
					autocomplete="off"
					data-testid="create-workspace-name"
				/>
			</div>

			<div class="space-y-1.5">
				<Label for="ws-slug">Slug <span class="text-muted-foreground">(optional)</span></Label>
				<Input
					id="ws-slug"
					bind:value={slug}
					placeholder={previewSlug || 'acme-robotics'}
					autocomplete="off"
					data-testid="create-workspace-slug"
				/>
				<p class="text-xs text-muted-foreground">
					URL identifier:
					<span class="font-mono text-foreground">{previewSlug || '—'}</span>
				</p>
			</div>

			{#if error}
				<p class="text-sm text-destructive" data-testid="create-workspace-error">{error}</p>
			{/if}

			<DialogFooter>
				<Button
					type="button"
					variant="ghost"
					onclick={() => (open = false)}
					disabled={submitting}
				>
					Cancel
				</Button>
				<Button type="submit" disabled={submitting} data-testid="create-workspace-submit">
					{submitting ? 'Creating…' : 'Create workspace'}
				</Button>
			</DialogFooter>
		</form>
	</DialogContent>
</Dialog>
