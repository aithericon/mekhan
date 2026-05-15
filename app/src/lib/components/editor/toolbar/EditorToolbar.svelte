<script lang="ts">
	import Save from '@lucide/svelte/icons/save';
	import Upload from '@lucide/svelte/icons/upload';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Eye from '@lucide/svelte/icons/eye';
	import Code from '@lucide/svelte/icons/code';
	import Pencil from '@lucide/svelte/icons/pencil';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '../AwarenessBar.svelte';
	import ConnectionStatus from '../ConnectionStatus.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		templateName: string;
		published: boolean;
		saving: boolean;
		templateId?: string;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		onsave?: () => void;
		onpublish: () => void;
		onpreview: () => void;
		/** Fork a published template into a fresh editable draft version. */
		onnewversion?: () => void;
		/** Start a run of a published template (opens the instance dialog). */
		onrun?: () => void;
		/** Commit a new template name (parent does the API call + state). */
		onrename?: (name: string) => void;
	};

	let {
		templateName,
		published,
		saving,
		templateId,
		awareness,
		provider,
		onsave,
		onpublish,
		onpreview,
		onnewversion,
		onrun,
		onrename
	}: Props = $props();

	// Inline rename. Published templates are server-locked (409), so editing
	// is only offered on drafts.
	let editing = $state(false);
	let draft = $state('');
	let inputEl = $state<HTMLInputElement | null>(null);

	function startEdit() {
		if (published || !onrename) return;
		draft = templateName;
		editing = true;
	}

	function commit() {
		if (!editing) return;
		editing = false;
		const next = draft.trim();
		if (next && next !== templateName) onrename?.(next);
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

<div class="flex h-10 items-center justify-between border-b border-border bg-card px-3" data-testid="editor-toolbar">
	<div class="flex items-center gap-3">
		{#if editing}
			<Input
				bind:ref={inputEl}
				bind:value={draft}
				onkeydown={onKeydown}
				onblur={commit}
				onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
				aria-label="Template name"
				data-testid="toolbar-template-name-input"
				class="h-7 w-56 text-sm font-medium"
			/>
		{:else if !published && onrename}
			<button
				type="button"
				onclick={startEdit}
				title="Rename template"
				data-testid="toolbar-template-name"
				class="group flex items-center gap-1.5 rounded-md px-1 py-0.5 text-sm font-medium text-foreground hover:bg-accent"
			>
				<span>{templateName}</span>
				<Pencil class="size-3 text-muted-foreground opacity-0 transition-opacity group-hover:opacity-100" />
			</button>
		{:else}
			<span class="text-sm font-medium text-foreground" data-testid="toolbar-template-name">{templateName}</span>
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
		{#if templateId}
			<Button
				variant="ghost"
				size="sm"
				href="/templates/{templateId}/ide"
				data-testid="btn-ide-mode"
			>
				<Code class="size-3.5" />
				IDE Mode
			</Button>
		{/if}

		<Button
			variant="ghost"
			size="sm"
			data-testid="btn-preview-air"
			onclick={onpreview}
		>
			<Eye class="size-3.5" />
			Preview AIR
		</Button>

		{#if onsave}
			<Button
				variant="ghost"
				size="sm"
				data-testid="btn-save"
				disabled={saving || published}
				onclick={onsave}
			>
				<Save class="size-3.5" />
				{saving ? 'Saving...' : 'Save'}
			</Button>
		{/if}

		{#if published && onrun}
			<Button
				size="sm"
				data-testid="btn-run-template"
				onclick={onrun}
			>
				<Rocket class="size-3.5" />
				Run
			</Button>
		{/if}

		{#if published && onnewversion}
			<Button
				size="sm"
				data-testid="btn-new-version"
				onclick={onnewversion}
			>
				<GitBranch class="size-3.5" />
				New Version
			</Button>
		{:else}
			<Button
				size="sm"
				data-testid="btn-publish"
				disabled={published}
				onclick={onpublish}
			>
				<Upload class="size-3.5" />
				Publish
			</Button>
		{/if}
	</div>
</div>
