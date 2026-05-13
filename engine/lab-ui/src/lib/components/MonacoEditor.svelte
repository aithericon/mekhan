<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import loader from '@monaco-editor/loader';
	import type * as Monaco from 'monaco-editor';

	interface Props {
		value: string;
		language?: string;
		readOnly?: boolean;
		height?: string;
		theme?: string;
		onChange?: (newValue: string) => void;
	}

	let { value, language = 'javascript', readOnly = true, height = '200px', theme = 'vs-dark', onChange }: Props = $props();

	let editorContainer: HTMLDivElement;
	let editor: Monaco.editor.IStandaloneCodeEditor | null = null;
	let monaco: typeof Monaco | null = null;

	onMount(async () => {
		monaco = await loader.init();

		// Register Rhai as a custom language (similar to Rust syntax)
		monaco!.languages.register({ id: 'rhai' });
		monaco!.languages.setMonarchTokensProvider('rhai', {
			tokenizer: {
				root: [
					// Comments
					[/\/\/.*$/, 'comment'],
					[/\/\*/, 'comment', '@comment'],

					// Strings
					[/"([^"\\]|\\.)*$/, 'string.invalid'],
					[/"/, 'string', '@string'],

					// Numbers
					[/\d+\.?\d*/, 'number'],

					// Keywords
					[/\b(let|const|if|else|while|for|in|loop|break|continue|return|fn|true|false|null|switch|case|throw|try|catch)\b/, 'keyword'],

					// Operators
					[/[+\-*/%=<>!&|^~]+/, 'operator'],

					// Identifiers
					[/[a-zA-Z_]\w*/, 'identifier'],

					// Brackets
					[/[{}()\[\]]/, '@brackets'],

					// Delimiters
					[/[;,.]/, 'delimiter'],
				],
				comment: [
					[/[^/*]+/, 'comment'],
					[/\*\//, 'comment', '@pop'],
					[/[/*]/, 'comment']
				],
				string: [
					[/[^\\"]+/, 'string'],
					[/\\./, 'string.escape'],
					[/"/, 'string', '@pop']
				]
			}
		});

		editor = monaco!.editor.create(editorContainer, {
			value,
			language: language === 'rhai' ? 'rhai' : language,
			readOnly,
			theme,
			minimap: { enabled: false },
			scrollBeyondLastLine: false,
			fontSize: 13,
			lineNumbers: 'on',
			automaticLayout: true,
			wordWrap: 'on',
			padding: { top: 8, bottom: 8 }
		});

		// Set up change listener if onChange callback provided
		if (onChange) {
			editor.onDidChangeModelContent(() => {
				if (editor) {
					onChange(editor.getValue());
				}
			});
		}
	});

	onDestroy(() => {
		editor?.dispose();
	});

	// Update editor value when prop changes
	$effect(() => {
		if (editor && value !== editor.getValue()) {
			editor.setValue(value);
		}
	});
</script>

<div bind:this={editorContainer} class="monaco-container" style="height: {height}; width: 100%;"></div>

<style>
	.monaco-container {
		border-radius: 6px;
		overflow: hidden;
	}
</style>
