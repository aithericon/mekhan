<script lang="ts">
	import Upload from '@lucide/svelte/icons/upload';
	import LayoutGrid from '@lucide/svelte/icons/layout-grid';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Pencil from '@lucide/svelte/icons/pencil';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '$lib/components/editor/AwarenessBar.svelte';
	import ConnectionStatus from '$lib/components/editor/ConnectionStatus.svelte';
	import TemplateVersionMenu from '$lib/components/editor/toolbar/TemplateVersionMenu.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		templateName: string;
		templateId: string;
		published: boolean;
		/** Version number of the open template; enables the history menu. */
		version?: number;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		onPublish: () => void;
		/** Fork a published template into a fresh editable draft version. */
		onNewVersion?: () => void;
		/** Start a run of a published template (opens the instance dialog). */
		onRun?: () => void;
		/** Commit a new template name (parent does the API call + state). */
		onRename?: (name: string) => void;
	};

	let {
		templateName,
		templateId,
		published,
		version,
		awareness,
		provider,
		onPublish,
		onNewVersion,
		onRun,
		onRename
	}: Props = $props();

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
		{#if templateId && version !== undefined}
			<TemplateVersionMenu {templateId} currentVersion={version} mode="ide" />
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

		{#if published && onRun}
			<Button size="sm" data-testid="btn-run-template" onclick={onRun}>
				<Rocket class="size-3.5" />
				Run
			</Button>
		{/if}

		{#if published && onNewVersion}
			<Button size="sm" data-testid="btn-new-version" onclick={onNewVersion}>
				<GitBranch class="size-3.5" />
				New Version
			</Button>
		{:else}
			<Button size="sm" data-testid="btn-publish" disabled={published} onclick={onPublish}>
				<Upload class="size-3.5" />
				Publish
			</Button>
		{/if}
	</div>
</div>
