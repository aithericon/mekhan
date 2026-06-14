<!--
  EdraEditor — the vendored editor SHELL.

  Transport-agnostic by design: it does NOT create a Y.Doc or a provider. The
  caller passes a ready `Y.XmlFragment` (already synced) + the `editable` flag,
  and this component constructs the Tiptap `Editor`, mounts its DOM into a div,
  and hands the live editor back via `onready` (so a toolbar/bubble menu can
  bind to it). Teardown of the Y.Doc/provider is the CALLER's job — see
  `$lib/components/pages/PageEditor.svelte` for the full sync-then-bind +
  ordered-teardown lifecycle.

  This component is created CLIENT-ONLY (the parent gates on `onMount`/`browser`
  before rendering it), so Tiptap's DOM access never runs during SSR.
-->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import { Editor } from '@tiptap/core';
	import type * as Y from 'yjs';
	import { pageExtensions } from './extensions';
	import { cn } from '$lib/utils';
	import './content.css';

	let {
		fragment,
		editable = true,
		placeholder,
		extraExtensions,
		class: className,
		onready
	}: {
		/** The synced Yjs fragment to bind Collaboration to. */
		fragment: Y.XmlFragment;
		editable?: boolean;
		placeholder?: string;
		/** Host-supplied extra nodes/extensions (e.g. the run-media embed). */
		extraExtensions?: Parameters<typeof pageExtensions>[0]['extraExtensions'];
		class?: string;
		/** Fired once the Editor is constructed. */
		onready?: (editor: Editor) => void;
	} = $props();

	let el = $state<HTMLDivElement | null>(null);
	let editor = $state<Editor | null>(null);

	// Construct the editor once the mount element exists. `fragment` is captured
	// once — a page editor binds to a single fragment for its whole lifetime
	// ({#key pageId} at call sites guarantees a fresh component per page).
	$effect(() => {
		if (!el || editor) return;
		const instance = new Editor({
			element: el,
			editable,
			extensions: pageExtensions({ fragment, placeholder, extraExtensions }),
			editorProps: {
				attributes: {
					// `.edra-content` is the styling hook (content.css); ProseMirror
					// adds `.ProseMirror` itself.
					class: cn('edra-content focus:outline-none', className)
				}
			}
		});
		editor = instance;
		onready?.(instance);
	});

	// Reactive read-only: a viewer still receives remote updates (live viewer
	// mode) but cannot type.
	$effect(() => {
		editor?.setEditable(editable);
	});

	onDestroy(() => {
		editor?.destroy();
		editor = null;
	});
</script>

<div bind:this={el} class="h-full"></div>
