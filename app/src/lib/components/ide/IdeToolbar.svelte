<script lang="ts">
	import Upload from '@lucide/svelte/icons/upload';
	import LayoutGrid from '@lucide/svelte/icons/layout-grid';
	import Pencil from '@lucide/svelte/icons/pencil';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '$lib/components/editor/AwarenessBar.svelte';
	import ConnectionStatus from '$lib/components/editor/ConnectionStatus.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		templateName: string;
		templateId: string;
		published: boolean;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		onPublish: () => void;
		/** Commit a new template name (parent does the API call + state). */
		onRename?: (name: string) => void;
	};

	let { templateName, templateId, published, awareness, provider, onPublish, onRename }: Props =
		$props();

	// Inline rename. Published templates are locked (server returns 409), so
	// editing is only offered on drafts.
	let editing = $state(false);
	let draft = $state('');
	let inputEl = $state<HTMLInputElement | null>(null);

	function startEdit() {
		if (published || !onRename) return;
		draft = templateName;
		editing = true;
	}

	function commit() {
		if (!editing) return;
		editing = false;
		const next = draft.trim();
		if (next && next !== templateName) onRename?.(next);
	}

	function onKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			commit();
		} else if (e.key === 'Escape') {
			e.preventDefault();
			editing = false;
		}
	}

	$effect(() => {
		if (editing) inputEl?.focus();
	});
</script>

<div class="flex h-10 items-center justify-between border-b border-border bg-card px-3">
	<div class="flex items-center gap-3">
		{#if editing}
			<Input
				bind:ref={inputEl}
				bind:value={draft}
				onkeydown={onKeydown}
				onblur={commit}
				onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
				aria-label="Template name"
				class="h-7 w-56 text-sm font-medium"
			/>
		{:else if !published && onRename}
			<button
				type="button"
				onclick={startEdit}
				title="Rename template"
				class="group flex items-center gap-1.5 rounded-md px-1 py-0.5 text-sm font-medium text-foreground hover:bg-accent"
			>
				<span>{templateName}</span>
				<Pencil class="size-3 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
			</button>
		{:else}
			<span class="text-sm font-medium text-foreground">{templateName}</span>
		{/if}
		{#if published}
			<Badge class="bg-green-100 text-green-700" variant="secondary">
				Published
			</Badge>
		{:else}
			<Badge class="bg-amber-100 text-amber-700" variant="secondary">
				Draft
			</Badge>
		{/if}
		{#if provider}
			<ConnectionStatus {provider} />
		{/if}
		{#if awareness}
			<AwarenessBar {awareness} />
		{/if}
	</div>

	<div class="flex items-center gap-1.5">
		<Button variant="ghost" size="sm" href="/templates/{templateId}">
			<LayoutGrid class="size-3.5" />
			Canvas Mode
		</Button>

		<Button size="sm" disabled={published} onclick={onPublish}>
			<Upload class="size-3.5" />
			Publish
		</Button>
	</div>
</div>
