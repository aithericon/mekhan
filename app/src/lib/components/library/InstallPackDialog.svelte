<script lang="ts">
	// Install-pack dialog: accepts a PackBundle JSON either by file pick (reads the
	// file text) or by pasting into a textarea, parses + validates the JSON locally,
	// then calls importPack. Surfaces 409 coordinate-conflicts from the ApiError.
	import { importPack, ApiError, type PackBundle, type PackImportResult } from '$lib/api/client';
	import {
		Dialog,
		DialogContent,
		DialogHeader,
		DialogTitle,
		DialogDescription,
		DialogFooter
	} from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Textarea } from '$lib/components/ui/textarea';
	import Upload from '@lucide/svelte/icons/upload';

	let {
		open = $bindable(false),
		onimported
	}: {
		open?: boolean;
		/** Fired after a successful import so the caller can refresh + toast. */
		onimported?: (result: PackImportResult) => void;
	} = $props();

	let raw = $state('');
	let fileName = $state<string | null>(null);
	let submitting = $state(false);
	let localError = $state<string | null>(null);
	let fileInput = $state<HTMLInputElement | null>(null);

	function reset() {
		raw = '';
		fileName = null;
		submitting = false;
		localError = null;
	}

	$effect(() => {
		if (!open) reset();
	});

	async function onFilePicked(e: Event) {
		const input = e.currentTarget as HTMLInputElement;
		const file = input.files?.[0];
		if (!file) return;
		try {
			raw = await file.text();
			fileName = file.name;
			localError = null;
		} catch {
			localError = 'Could not read the selected file';
		}
	}

	async function submit() {
		localError = null;
		let bundle: PackBundle;
		try {
			bundle = JSON.parse(raw) as PackBundle;
		} catch {
			localError = 'Not valid JSON — paste a pack bundle or pick a .json file';
			return;
		}
		if (!bundle || typeof bundle !== 'object' || !bundle.manifest || !Array.isArray(bundle.nodes)) {
			localError = 'JSON is not a pack bundle (missing `manifest` or `nodes`)';
			return;
		}
		submitting = true;
		try {
			const result = await importPack(bundle);
			open = false;
			onimported?.(result);
		} catch (e) {
			if (e instanceof ApiError) {
				if (e.status === 409) {
					const coord = (e.body as { coordinate?: string }).coordinate;
					localError = coord
						? `Coordinate conflict: "${coord}" already exists in this workspace`
						: e.body.error ?? 'A node coordinate in this pack already exists';
				} else {
					localError = e.body.error ?? e.message;
				}
			} else {
				localError = e instanceof Error ? e.message : 'Import failed';
			}
		} finally {
			submitting = false;
		}
	}
</script>

<Dialog bind:open>
	<DialogContent class="sm:max-w-lg" data-testid="library-pack-install-dialog">
		<DialogHeader>
			<DialogTitle>Install pack</DialogTitle>
			<DialogDescription>
				Paste a pack bundle JSON, or pick a <code>.json</code> file exported from another workspace.
			</DialogDescription>
		</DialogHeader>

		<div class="space-y-3">
			<input
				bind:this={fileInput}
				type="file"
				accept="application/json,.json"
				class="hidden"
				onchange={onFilePicked}
				data-testid="library-pack-install-file"
			/>
			<div class="flex items-center gap-2">
				<Button variant="outline" size="sm" onclick={() => fileInput?.click()}>
					<Upload class="size-4" />
					Choose file
				</Button>
				{#if fileName}
					<span class="truncate text-sm text-muted-foreground">{fileName}</span>
				{/if}
			</div>

			<Textarea
				bind:value={raw}
				rows={10}
				placeholder={'{\n  "manifest": { "name": "...", "vendor": "...", "slug": "..." },\n  "nodes": [],\n  "assets": []\n}'}
				class="font-mono text-xs"
				data-testid="library-pack-install-textarea"
			/>

			{#if localError}
				<p class="text-sm text-destructive" data-testid="library-pack-install-error">{localError}</p>
			{/if}
		</div>

		<DialogFooter>
			<Button variant="ghost" onclick={() => (open = false)} disabled={submitting}>Cancel</Button>
			<Button
				onclick={submit}
				disabled={submitting || raw.trim().length === 0}
				data-testid="library-pack-install-submit"
			>
				{submitting ? 'Installing…' : 'Install'}
			</Button>
		</DialogFooter>
	</DialogContent>
</Dialog>
