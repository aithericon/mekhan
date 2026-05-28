<script lang="ts" module>
	/** Minimal imperative API for this editor. Exposed via `onready` so a
	 *  parent (e.g. the IDE's reference panel) can insert refs at the
	 *  current cursor without owning the EditorView instance. */
	export type CodeEditorApi = {
		insertAtCursor: (text: string) => void;
	};
</script>

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
		/** Fires once the EditorView is mounted; called with `null` on destroy
		 *  so the parent can drop its stale reference. */
		onready?: (api: CodeEditorApi | null) => void;
	};

	let {
		ytext,
		language = 'python',
		readonly = false,
		awareness,
		minHeight = '150px',
		maxHeight = '400px',
		onready
	}: Props = $props();

	let containerEl: HTMLDivElement | undefined = $state();
	let view: any = $state(undefined);

	onMount(async () => {
		if (!browser || !containerEl) return;

		const { EditorView, keymap, lineNumbers } = await import('@codemirror/view');
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
			lineNumbers(),
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
				'.cm-gutters': {
					backgroundColor: 'transparent',
					borderRight: '1px solid var(--border)'
				},
				'.cm-lineNumbers .cm-gutterElement': {
					padding: '0 8px 0 6px',
					minWidth: '2ch'
				},
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

		// Expose insert-at-cursor so the IDE's reference panel can drop a
		// `token["..."]` snippet at the active selection without reaching
		// across components for the EditorView.
		onready?.({
			insertAtCursor: (text: string) => {
				if (!view || readonly) return;
				const sel = view.state.selection.main;
				view.dispatch({
					changes: { from: sel.from, to: sel.to, insert: text },
					selection: { anchor: sel.from + text.length }
				});
				view.focus();
			}
		});
	});

	onDestroy(() => {
		onready?.(null);
		view?.destroy();
	});
</script>

<div bind:this={containerEl} class="collab-code-editor-container" class:opacity-70={readonly}></div>

<style>
	.collab-code-editor-container {
		width: 100%;
		height: 100%;
	}
	.collab-code-editor-container :global(.cm-editor) {
		height: 100%;
	}
</style>
