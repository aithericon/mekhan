<!--
  EdraToolbar — a shadcn-styled formatting toolbar bound to a live Tiptap editor.

  Mark/node active states are kept reactive by subscribing to the editor's
  `transaction` + `selectionUpdate` events and re-reading `isActive(...)`. The
  toolbar self-disables when the editor is not editable.
-->
<script lang="ts">
	import { onDestroy, type Snippet } from 'svelte';
	import type { Editor } from '@tiptap/core';
	import Bold from '@lucide/svelte/icons/bold';
	import Italic from '@lucide/svelte/icons/italic';
	import Strikethrough from '@lucide/svelte/icons/strikethrough';
	import Heading1 from '@lucide/svelte/icons/heading-1';
	import Heading2 from '@lucide/svelte/icons/heading-2';
	import Heading3 from '@lucide/svelte/icons/heading-3';
	import ListIcon from '@lucide/svelte/icons/list';
	import ListOrdered from '@lucide/svelte/icons/list-ordered';
	import Quote from '@lucide/svelte/icons/quote';
	import LinkIcon from '@lucide/svelte/icons/link';
	import CodeIcon from '@lucide/svelte/icons/code';
	import SquareCode from '@lucide/svelte/icons/square-code';
	import TableIcon from '@lucide/svelte/icons/table';
	import Minus from '@lucide/svelte/icons/minus';
	import { Button } from '$lib/components/ui/button';
	import { Separator } from '$lib/components/ui/separator';
	import { cn } from '$lib/utils';
	import * as cmd from './commands';

	let {
		editor,
		class: className,
		actions
	}: {
		editor: Editor | null;
		class?: string;
		/** Optional host-supplied buttons rendered at the toolbar's trailing edge. */
		actions?: Snippet;
	} = $props();

	// A monotonically-bumped tick that forces the `active`/`editable` getters to
	// re-evaluate on every editor transaction/selection change.
	let tick = $state(0);
	let bound = $state<Editor | null>(null);

	function bump() {
		tick += 1;
	}

	$effect(() => {
		// (Re)bind listeners whenever the editor instance changes.
		if (bound === editor) return;
		if (bound) {
			bound.off('transaction', bump);
			bound.off('selectionUpdate', bump);
		}
		bound = editor;
		if (editor) {
			editor.on('transaction', bump);
			editor.on('selectionUpdate', bump);
			bump();
		}
	});

	onDestroy(() => {
		if (bound) {
			bound.off('transaction', bump);
			bound.off('selectionUpdate', bump);
		}
	});

	const editable = $derived.by(() => {
		void tick;
		return editor?.isEditable ?? false;
	});

	function isActive(name: string, attrs?: Record<string, unknown>): boolean {
		void tick;
		return editor?.isActive(name, attrs) ?? false;
	}
</script>

<div
	class={cn(
		'bg-card flex flex-wrap items-center gap-0.5 rounded-md border p-1',
		className
	)}
	role="toolbar"
	aria-label="Formatting"
>
	{#snippet tool(
		label: string,
		Icon: typeof Bold,
		onclick: () => void,
		active: boolean
	)}
		<Button
			type="button"
			variant={active ? 'secondary' : 'ghost'}
			size="icon-sm"
			title={label}
			aria-label={label}
			aria-pressed={active}
			disabled={!editor || !editable}
			{onclick}
		>
			<Icon />
		</Button>
	{/snippet}

	{@render tool('Bold', Bold, () => editor && cmd.toggleBold(editor), isActive('bold'))}
	{@render tool('Italic', Italic, () => editor && cmd.toggleItalic(editor), isActive('italic'))}
	{@render tool(
		'Strikethrough',
		Strikethrough,
		() => editor && cmd.toggleStrike(editor),
		isActive('strike')
	)}
	{@render tool('Inline code', CodeIcon, () => editor && cmd.toggleCode(editor), isActive('code'))}

	<Separator orientation="vertical" class="mx-1 h-6" />

	{@render tool(
		'Heading 1',
		Heading1,
		() => editor && cmd.toggleHeading(editor, 1),
		isActive('heading', { level: 1 })
	)}
	{@render tool(
		'Heading 2',
		Heading2,
		() => editor && cmd.toggleHeading(editor, 2),
		isActive('heading', { level: 2 })
	)}
	{@render tool(
		'Heading 3',
		Heading3,
		() => editor && cmd.toggleHeading(editor, 3),
		isActive('heading', { level: 3 })
	)}

	<Separator orientation="vertical" class="mx-1 h-6" />

	{@render tool(
		'Bullet list',
		ListIcon,
		() => editor && cmd.toggleBulletList(editor),
		isActive('bulletList')
	)}
	{@render tool(
		'Numbered list',
		ListOrdered,
		() => editor && cmd.toggleOrderedList(editor),
		isActive('orderedList')
	)}
	{@render tool(
		'Quote',
		Quote,
		() => editor && cmd.toggleBlockquote(editor),
		isActive('blockquote')
	)}

	<Separator orientation="vertical" class="mx-1 h-6" />

	{@render tool('Link', LinkIcon, () => editor && cmd.toggleLink(editor), isActive('link'))}
	{@render tool(
		'Code block',
		SquareCode,
		() => editor && cmd.toggleCodeBlock(editor),
		isActive('codeBlock')
	)}
	{@render tool('Table', TableIcon, () => editor && cmd.insertTable(editor), false)}
	{@render tool('Divider', Minus, () => editor && cmd.setHorizontalRule(editor), false)}

	{#if actions}
		<Separator orientation="vertical" class="mx-1 h-6" />
		{@render actions()}
	{/if}
</div>
