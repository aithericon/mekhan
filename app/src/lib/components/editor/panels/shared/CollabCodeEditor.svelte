<script lang="ts">
	import { browser } from '$app/environment';
	import { onMount, onDestroy } from 'svelte';
	import type * as Y from 'yjs';
	import type { Awareness } from 'y-protocols/awareness';

	type Props = {
		ytext: Y.Text;
		language?: 'python' | 'json' | 'dockerfile' | 'text';
		readonly?: boolean;
		awareness?: Awareness;
		minHeight?: string;
		maxHeight?: string;
	};

	let {
		ytext,
		language = 'python',
		readonly = false,
		awareness,
		minHeight = '150px',
		maxHeight = '400px'
	}: Props = $props();

	let containerEl: HTMLDivElement | undefined = $state();
	let view: any = $state(undefined);

	onMount(async () => {
		if (!browser || !containerEl) return;

		const { EditorView, keymap } = await import('@codemirror/view');
		const { EditorState } = await import('@codemirror/state');
		const { defaultKeymap, history, historyKeymap } = await import('@codemirror/commands');
		const {
			syntaxHighlighting,
			defaultHighlightStyle,
			bracketMatching
		} = await import('@codemirror/language');
		const { oneDark } = await import('@codemirror/theme-one-dark');
		const { yCollab } = await import('y-codemirror.next');

		const extensions: any[] = [
			EditorView.lineWrapping,
			syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
			bracketMatching(),
			history(),
			keymap.of([...defaultKeymap, ...historyKeymap]),
			oneDark,
			EditorView.theme({
				'&': {
					fontSize: '12px',
					minHeight,
					maxHeight,
					border: '1px solid var(--border)',
					borderRadius: '0.375rem'
				},
				'.cm-scroller': {
					overflow: 'auto',
					fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace'
				},
				'.cm-content': { padding: '8px 0' },
				'.cm-gutters': { display: 'none' },
				'&.cm-focused': { outline: '2px solid var(--ring)', outlineOffset: '-1px' }
			}),
			yCollab(ytext, awareness ?? null)
		];

		if (language === 'python') {
			const { python } = await import('@codemirror/lang-python');
			extensions.push(python());
		} else if (language === 'json') {
			const { json } = await import('@codemirror/lang-json');
			extensions.push(json());
		}

		if (readonly) {
			extensions.push(EditorState.readOnly.of(true));
			extensions.push(EditorView.editable.of(false));
		}

		view = new EditorView({
			state: EditorState.create({ doc: ytext.toString(), extensions }),
			parent: containerEl
		});
	});

	onDestroy(() => {
		view?.destroy();
	});
</script>

<div bind:this={containerEl} class="collab-code-editor-container" class:opacity-70={readonly}></div>

<style>
	.collab-code-editor-container {
		width: 100%;
	}
	.collab-code-editor-container :global(.cm-editor) {
		height: auto;
	}
</style>
