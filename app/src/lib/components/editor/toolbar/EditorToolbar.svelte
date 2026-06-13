<script lang="ts">
	import Save from '@lucide/svelte/icons/save';
	import Undo2 from '@lucide/svelte/icons/undo-2';
	import Redo2 from '@lucide/svelte/icons/redo-2';
	import Upload from '@lucide/svelte/icons/upload';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Rocket from '@lucide/svelte/icons/rocket';
	import Eye from '@lucide/svelte/icons/eye';
	import Code from '@lucide/svelte/icons/code';
	import Pencil from '@lucide/svelte/icons/pencil';
	import FlaskConical from '@lucide/svelte/icons/flask-conical';
	import Settings from '@lucide/svelte/icons/settings';
	import Share2 from '@lucide/svelte/icons/share-2';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Lock from '@lucide/svelte/icons/lock';
	import type { Awareness } from 'y-protocols/awareness';
	import type { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import AwarenessBar from '../AwarenessBar.svelte';
	import ConnectionStatus from '../ConnectionStatus.svelte';
	import TemplateVersionMenu from './TemplateVersionMenu.svelte';
	import EditorRunsMenu from './EditorRunsMenu.svelte';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import { Input } from '$lib/components/ui/input';

	type Props = {
		templateName: string;
		/** When this template is a private sub-workflow, the owning parent's
		 *  family id + name drive a breadcrumb link back to it. */
		ownerId?: string | null;
		ownerName?: string | null;
		published: boolean;
		saving: boolean;
		templateId?: string;
		/** Version number of the open template; enables the history menu. */
		version?: number;
		awareness?: Awareness;
		provider?: MekhanWsProvider;
		/** Version-chain family id (`base_template_id ?? id`) — enables the
		 *  Runs menu. Set for drafts too: a draft's family may already have
		 *  runs from earlier published versions. */
		runsFamilyId?: string;
		onsave?: () => void;
		onpublish: () => void;
		onpreview: () => void;
		/** Fork a published template into a fresh editable draft version. */
		onnewversion?: () => void;
		/** Discard this unpublished draft (drafts only; opens a confirm). */
		ondiscard?: () => void;
		/** Start a run of a published template (opens the instance dialog). */
		onrun?: () => void;
		/** Dev-run an unpublished draft without publishing: opens the instance
		 *  dialog with the mode locked to 'draft' (the backend compiles the
		 *  draft per-run). Drafts only. */
		onrundraft?: () => void;
		/** Open the template-tests panel. */
		ontests?: () => void;
		/** Open the template settings panel (tags + visibility). */
		onsettings?: () => void;
		/** Open the object-grant Share dialog (object-Admin only; the page
		 *  passes this conditionally on `my_effective_role`). */
		onshare?: () => void;
		/** Commit a new template name (parent does the API call + state). */
		onrename?: (name: string) => void;
		/** Undo/redo over the local Yjs edit stack (drafts only). */
		onundo?: () => void;
		onredo?: () => void;
		canUndo?: boolean;
		canRedo?: boolean;
	};

	let {
		templateName,
		ownerId = null,
		ownerName = null,
		published,
		saving,
		templateId,
		version,
		runsFamilyId,
		awareness,
		provider,
		onsave,
		onpublish,
		onpreview,
		onnewversion,
		ondiscard,
		onrun,
		onrundraft,
		ontests,
		onsettings,
		onshare,
		onrename,
		onundo,
		onredo,
		canUndo = false,
		canRedo = false
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
		// onrename rejects on failure (shared with the settings panel); the page
		// banner already reports it, so swallow here to avoid an unhandled reject.
		if (next && next !== templateName) void Promise.resolve(onrename?.(next)).catch(() => {});
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
		{#if ownerId}
			<!-- Plain client-side nav: the editor route keys its session on the
			     `[id]` param, so switching templates remounts it cleanly. -->
			<a
				href="/templates/{ownerId}"
				class="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
				title="Open the owning workflow"
				data-testid="toolbar-owner-breadcrumb"
			>
				<Lock class="size-3" />
				<span class="max-w-40 truncate">{ownerName ?? 'Parent workflow'}</span>
			</a>
			<span class="text-sm text-muted-foreground/50">/</span>
		{/if}
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
		{#if templateId && version !== undefined}
			<TemplateVersionMenu {templateId} currentVersion={version} mode="canvas" />
		{/if}
		{#if provider}
			<ConnectionStatus {provider} />
		{/if}
		{#if awareness}
			<AwarenessBar {awareness} />
		{/if}
	</div>

	<div class="flex items-center gap-1.5">
		{#if !published && onundo && onredo}
			<Button
				variant="ghost"
				size="sm"
				disabled={!canUndo}
				onclick={onundo}
				title="Undo (⌘Z)"
				aria-label="Undo"
				data-testid="btn-undo"
			>
				<Undo2 class="size-3.5" />
			</Button>
			<Button
				variant="ghost"
				size="sm"
				disabled={!canRedo}
				onclick={onredo}
				title="Redo (⇧⌘Z)"
				aria-label="Redo"
				data-testid="btn-redo"
			>
				<Redo2 class="size-3.5" />
			</Button>
			<div class="mx-1 h-5 w-px bg-border" role="presentation"></div>
		{/if}

		{#if runsFamilyId}
			<EditorRunsMenu familyId={runsFamilyId} />
		{/if}

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

		{#if ontests}
			<Button
				variant="ghost"
				size="sm"
				data-testid="btn-tests"
				onclick={ontests}
			>
				<FlaskConical class="size-3.5" />
				Tests
			</Button>
		{/if}

		{#if onsettings}
			<Button
				variant="ghost"
				size="sm"
				data-testid="btn-settings"
				onclick={onsettings}
			>
				<Settings class="size-3.5" />
				Settings
			</Button>
		{/if}

		{#if onshare}
			<Button variant="ghost" size="sm" data-testid="btn-share-template" onclick={onshare}>
				<Share2 class="size-3.5" />
				Share
			</Button>
		{/if}

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
			{#if ondiscard}
				<Button
					variant="ghost"
					size="sm"
					class="text-destructive hover:text-destructive"
					data-testid="btn-discard-draft"
					disabled={saving}
					onclick={ondiscard}
					title="Discard this draft"
				>
					<Trash2 class="size-3.5" />
					Discard
				</Button>
			{/if}
			{#if onrundraft}
				<Button
					variant="warm"
					size="sm"
					data-testid="btn-run-draft"
					disabled={saving}
					onclick={onrundraft}
					title="Run this draft without publishing (compiled per-run from the live canvas; creates a draft instance)"
				>
					<FlaskConical class="size-3.5" />
					Run draft
				</Button>
			{/if}
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
