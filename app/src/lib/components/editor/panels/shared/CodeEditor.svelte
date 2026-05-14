<script lang="ts">
	import { browser } from '$app/environment';
	import { onMount, onDestroy } from 'svelte';

	type Props = {
		value: string;
		language?: 'python' | 'json' | 'rhai';
		readonly?: boolean;
		minHeight?: string;
		maxHeight?: string;
		onchange?: (value: string) => void;
	};

	let {
		value,
		language = 'python',
		readonly = false,
		minHeight = '120px',
		maxHeight = '300px',
		onchange
	}: Props = $props();

	let containerEl: HTMLDivElement | undefined = $state();
	let view: any = $state(undefined);

	onMount(async () => {
		if (!browser || !containerEl) return;

		const { EditorView, lineNumbers, keymap } = await import('@codemirror/view');
		const { EditorState } = await import('@codemirror/state');
		const { defaultKeymap, history, historyKeymap } = await import('@codemirror/commands');
		const {
			syntaxHighlighting,
			defaultHighlightStyle,
			bracketMatching
		} = await import('@codemirror/language');
		const { oneDark } = await import('@codemirror/theme-one-dark');

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
			})
		];

		// Language extension
		if (language === 'python') {
			const { python } = await import('@codemirror/lang-python');
			extensions.push(python());
		} else if (language === 'json') {
			const { json } = await import('@codemirror/lang-json');
			extensions.push(json());
		}
		// 'rhai' uses no language extension — plain text with syntax highlighting fallback

		if (readonly) {
			extensions.push(EditorState.readOnly.of(true));
			extensions.push(EditorView.editable.of(false));
		}

		// Change listener
		extensions.push(
			EditorView.updateListener.of((update: any) => {
				if (update.docChanged && onchange) {
					onchange(update.state.doc.toString());
				}
			})
		);

		view = new EditorView({
			state: EditorState.create({
				doc: value,
				extensions
			}),
			parent: containerEl
		});
	});

	// Sync external value changes into the editor
	$effect(() => {
		if (!view) return;
		const currentDoc = view.state.doc.toString();
		if (value !== currentDoc) {
			view.dispatch({
				changes: { from: 0, to: currentDoc.length, insert: value }
			});
		}
	});

	onDestroy(() => {
		view?.destroy();
	});
</script>

<div bind:this={containerEl} class="code-editor-container" class:opacity-70={readonly}></div>

<style>
	.code-editor-container {
		width: 100%;
	}
	.code-editor-container :global(.cm-editor) {
		height: auto;
	}
</style>
