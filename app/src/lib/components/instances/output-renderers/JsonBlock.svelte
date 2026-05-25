<script lang="ts">
	import { Dialog } from 'bits-ui';
	import { Button } from '$lib/components/ui/button';
	import Maximize2 from '@lucide/svelte/icons/maximize-2';
	import X from '@lucide/svelte/icons/x';
	import CodeEditor from '$lib/components/editor/panels/shared/CodeEditor.svelte';
	import type { RendererProps } from './types';

	let { value }: RendererProps = $props();

	const text = $derived.by<string>(() => {
		try {
			return JSON.stringify(value, null, 2);
		} catch {
			return String(value);
		}
	});

	let maximized = $state(false);
</script>

<!-- Compact view: bounded-height JSON viewer with syntax highlighting, plus a
     maximize affordance for the common case of deeply-nested executor outputs
     that would otherwise spill the entire drawer. -->
<div class="relative">
	<CodeEditor value={text} language="json" readonly minHeight="80px" maxHeight="320px" />
	<Button
		variant="ghost"
		size="icon"
		class="absolute right-1.5 top-1.5 size-7 bg-card/80 backdrop-blur-sm hover:bg-card"
		onclick={() => (maximized = true)}
		title="Maximize"
		aria-label="Maximize JSON view"
	>
		<Maximize2 class="size-3.5" />
	</Button>
</div>

<Dialog.Root bind:open={maximized}>
	<Dialog.Portal>
		<Dialog.Overlay
			class="fixed inset-0 z-[80] bg-black/40 data-[state=open]:animate-in data-[state=open]:fade-in"
		/>
		<Dialog.Content
			class="fixed left-1/2 top-1/2 z-[90] flex max-h-[90vh] w-[min(95vw,1100px)] -translate-x-1/2 -translate-y-1/2 flex-col rounded-lg border border-border bg-card shadow-xl data-[state=open]:animate-in data-[state=open]:fade-in data-[state=open]:zoom-in-95"
		>
			<header class="flex items-center gap-3 border-b border-border px-5 py-3">
				<Dialog.Title class="flex-1 text-base font-semibold text-foreground">
					JSON
				</Dialog.Title>
				<Dialog.Description class="sr-only">
					Full-screen JSON viewer with syntax highlighting.
				</Dialog.Description>
				<Dialog.Close>
					<Button variant="ghost" size="icon" aria-label="Close">
						<X class="size-4" />
					</Button>
				</Dialog.Close>
			</header>
			<div class="flex-1 overflow-hidden p-4">
				<CodeEditor
					value={text}
					language="json"
					readonly
					minHeight="200px"
					maxHeight="calc(90vh - 8rem)"
				/>
			</div>
		</Dialog.Content>
	</Dialog.Portal>
</Dialog.Root>
