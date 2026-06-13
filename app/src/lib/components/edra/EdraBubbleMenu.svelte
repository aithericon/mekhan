<!--
  EdraBubbleMenu — a floating selection bubble menu.

  Lean, self-contained (no `@tiptap/extension-bubble-menu` dependency): it shows
  over a non-empty text selection and positions itself off the selection's
  viewport rect. Hidden when the editor is read-only or the selection is empty.
-->
<script lang="ts">
	import { onDestroy } from 'svelte';
	import type { Editor } from '@tiptap/core';
	import Bold from '@lucide/svelte/icons/bold';
	import Italic from '@lucide/svelte/icons/italic';
	import Strikethrough from '@lucide/svelte/icons/strikethrough';
	import LinkIcon from '@lucide/svelte/icons/link';
	import CodeIcon from '@lucide/svelte/icons/code';
	import { Button } from '$lib/components/ui/button';
	import * as cmd from './commands';

	let {
		editor
	}: {
		editor: Editor | null;
	} = $props();

	let visible = $state(false);
	let top = $state(0);
	let left = $state(0);
	let tick = $state(0);
	let bound = $state<Editor | null>(null);

	function recompute() {
		tick += 1;
		const ed = editor;
		if (!ed || !ed.isEditable) {
			visible = false;
			return;
		}
		const { from, to, empty } = ed.state.selection;
		if (empty || from === to) {
			visible = false;
			return;
		}
		// Coordinates of the selection's start/end in viewport space.
		try {
			const start = ed.view.coordsAtPos(from);
			const end = ed.view.coordsAtPos(to);
			top = Math.min(start.top, end.top) - 44;
			left = (start.left + end.left) / 2;
			visible = true;
		} catch {
			visible = false;
		}
	}

	$effect(() => {
		if (bound === editor) return;
		if (bound) {
			bound.off('transaction', recompute);
			bound.off('selectionUpdate', recompute);
			bound.off('blur', hide);
		}
		bound = editor;
		if (editor) {
			editor.on('transaction', recompute);
			editor.on('selectionUpdate', recompute);
			editor.on('blur', hide);
		}
	});

	function hide() {
		// Defer: clicking a bubble button blurs the editor, so we only hide once
		// the selection truly collapses (next transaction re-evaluates).
		setTimeout(() => {
			if (editor && editor.state.selection.empty) visible = false;
		}, 100);
	}

	onDestroy(() => {
		if (bound) {
			bound.off('transaction', recompute);
			bound.off('selectionUpdate', recompute);
			bound.off('blur', hide);
		}
	});

	function isActive(name: string): boolean {
		void tick;
		return editor?.isActive(name) ?? false;
	}
</script>

{#if visible && editor}
	<div
		class="bg-popover text-popover-foreground fixed z-50 flex -translate-x-1/2 items-center gap-0.5 rounded-md border p-1 shadow-md"
		style="top: {top}px; left: {left}px;"
		role="toolbar"
		aria-label="Selection formatting"
	>
		<Button
			type="button"
			variant={isActive('bold') ? 'secondary' : 'ghost'}
			size="icon-sm"
			title="Bold"
			aria-label="Bold"
			onclick={() => cmd.toggleBold(editor)}
		>
			<Bold />
		</Button>
		<Button
			type="button"
			variant={isActive('italic') ? 'secondary' : 'ghost'}
			size="icon-sm"
			title="Italic"
			aria-label="Italic"
			onclick={() => cmd.toggleItalic(editor)}
		>
			<Italic />
		</Button>
		<Button
			type="button"
			variant={isActive('strike') ? 'secondary' : 'ghost'}
			size="icon-sm"
			title="Strikethrough"
			aria-label="Strikethrough"
			onclick={() => cmd.toggleStrike(editor)}
		>
			<Strikethrough />
		</Button>
		<Button
			type="button"
			variant={isActive('code') ? 'secondary' : 'ghost'}
			size="icon-sm"
			title="Inline code"
			aria-label="Inline code"
			onclick={() => cmd.toggleCode(editor)}
		>
			<CodeIcon />
		</Button>
		<Button
			type="button"
			variant={isActive('link') ? 'secondary' : 'ghost'}
			size="icon-sm"
			title="Link"
			aria-label="Link"
			onclick={() => cmd.toggleLink(editor)}
		>
			<LinkIcon />
		</Button>
	</div>
{/if}
