<!--
  PageEditor — a collaborative rich-text editor for one page, bound to the
  mekhan Yjs WS transport at key `page/${pageId}`.

  CORRECTNESS DISCIPLINE (PLAN §5.2 — load-bearing):
   1. CLIENT-ONLY: Tiptap touches the DOM, so the editor is created behind an
      `onMount`/`browser` guard — never during SSR.
   2. SYNC-THEN-BIND: we do NOT render <EdraEditor> (which constructs the
      Tiptap Editor + binds Collaboration) until the provider has SYNCED. Binding
      Collaboration to an empty pre-sync Y.XmlFragment round-trips ProseMirror's
      empty doc into the fragment and DUPLICATES it against server content (the
      concatenation hazard documented at ws-provider.ts ~:200).
   3. `history: false` (Tiptap v3's `undoRedo: false`) is enforced in the
      extension set — y-prosemirror owns undo via Y.UndoManager.
   4. `editable` is a PROP from the parent (computed from host my_effective_role
      via roleAtLeast). This component stays ACL-dumb; the server's WS handler is
      the real enforcement. A reactive $effect threads `editable` into the editor.
   5. Fresh Y.Doc PER instance (not the graph refcount cache). Call sites use
      {#key pageId} for remount safety.
   6. TEARDOWN IN ORDER on unmount: editor.destroy() (inside EdraEditor's
      onDestroy) → provider.destroy() → doc.destroy().

  NO remote carets in v1 — awareness has no WS transport yet.
-->
<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { browser } from '$app/environment';
	import * as Y from 'yjs';
	import type { Editor } from '@tiptap/core';
	import { MekhanWsProvider } from '$lib/yjs/ws-provider';
	import { yjsWsBase } from '$lib/yjs/session';
	import { EdraEditor, EdraToolbar, EdraBubbleMenu } from '$lib/components/edra';

	let {
		pageId,
		editable,
		placeholder = 'Write something…',
		showToolbar = true,
		onReady
	}: {
		pageId: string;
		editable: boolean;
		placeholder?: string;
		/** Render the formatting toolbar above the content (default true). */
		showToolbar?: boolean;
		onReady?: (editor: Editor) => void;
	} = $props();

	// Created client-only in onMount — never during SSR.
	let doc: Y.Doc | null = null;
	let provider: MekhanWsProvider | null = null;
	let fragment = $state<Y.XmlFragment | null>(null);
	let synced = $state(false);
	let editor = $state<Editor | null>(null);

	const onSync = (s: boolean) => {
		synced = s;
	};

	onMount(() => {
		if (!browser) return;
		doc = new Y.Doc();
		// The fragment reference is stable for the doc's lifetime — capturing it
		// now is safe; we only RENDER the editor (binding Collaboration) once
		// `synced` flips true (sync-then-bind).
		fragment = doc.getXmlFragment('content');
		provider = new MekhanWsProvider(yjsWsBase(), `page/${pageId}`, doc);
		synced = provider.isSynced;
		provider.onSync(onSync);

		// Cleanup also runs here (onMount return) — ordered teardown. EdraEditor's
		// own onDestroy already destroyed the Tiptap editor by the time Svelte
		// tears the tree down, but we null our ref defensively.
		return () => {
			editor = null;
			provider?.offSync(onSync);
			provider?.destroy();
			provider = null;
			doc?.destroy();
			doc = null;
			fragment = null;
		};
	});

	// Defensive double-teardown guard for non-onMount unmount paths.
	onDestroy(() => {
		provider?.offSync(onSync);
		provider?.destroy();
		provider = null;
		doc?.destroy();
		doc = null;
	});

	function handleReady(e: Editor) {
		editor = e;
		onReady?.(e);
	}
</script>

<div class="flex h-full min-h-0 flex-col">
	{#if showToolbar && editable}
		<div class="shrink-0 pb-2">
			<EdraToolbar {editor} />
		</div>
	{/if}

	<div class="min-h-0 flex-1 overflow-y-auto">
		{#if synced && fragment}
			<!-- {#key pageId} guards against a stale editor surviving a pageId swap
			     within the same mounted component (call sites also key, belt-and-suspenders). -->
			{#key pageId}
				<EdraEditor {fragment} {editable} {placeholder} onready={handleReady} />
			{/key}
			<EdraBubbleMenu {editor} />
		{:else}
			<div class="text-muted-foreground p-4 text-sm">Connecting…</div>
		{/if}
	</div>
</div>
