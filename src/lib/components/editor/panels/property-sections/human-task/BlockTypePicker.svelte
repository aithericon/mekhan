<script lang="ts">
	import type { TaskBlockConfig } from '$lib/types/editor';
	import Plus from '@lucide/svelte/icons/plus';

	type Props = {
		onadd: (block: TaskBlockConfig) => void;
	};

	let { onadd }: Props = $props();

	let open = $state(false);

	function addInput() {
		onadd({
			type: 'input',
			field: {
				name: `field_${Date.now()}`,
				label: 'New Field',
				kind: 'text',
				required: false
			}
		});
		open = false;
	}

	function addMdsvex() {
		onadd({ type: 'mdsvex', content: '' });
		open = false;
	}

	function addCallout() {
		onadd({ type: 'callout', severity: 'info', content: '' });
		open = false;
	}

	function addDivider() {
		onadd({ type: 'divider' });
		open = false;
	}
</script>

<div class="relative">
	<button
		type="button"
		class="flex w-full items-center justify-center gap-1 rounded border border-dashed border-border py-1 text-[10px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
		onclick={() => (open = !open)}
	>
		<Plus class="size-3" />
		Add Block
	</button>

	{#if open}
		<!-- svelte-ignore a11y_click_events_have_key_events -->
		<!-- svelte-ignore a11y_no_static_element_interactions -->
		<div
			class="fixed inset-0 z-40"
			onclick={() => (open = false)}
		></div>
		<div
			class="absolute left-0 right-0 top-full z-50 mt-1 rounded-md border border-border bg-popover p-1 shadow-md"
		>
			<button
				type="button"
				class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[10px] text-foreground transition-colors hover:bg-accent"
				onclick={addInput}
			>
				<span class="size-2 rounded-sm bg-blue-400"></span>
				Input Field
			</button>
			<button
				type="button"
				class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[10px] text-foreground transition-colors hover:bg-accent"
				onclick={addMdsvex}
			>
				<span class="size-2 rounded-sm bg-purple-400"></span>
				Markdown Content
			</button>
			<button
				type="button"
				class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[10px] text-foreground transition-colors hover:bg-accent"
				onclick={addCallout}
			>
				<span class="size-2 rounded-sm bg-amber-400"></span>
				Callout
			</button>
			<button
				type="button"
				class="flex w-full items-center gap-2 rounded px-2 py-1.5 text-left text-[10px] text-foreground transition-colors hover:bg-accent"
				onclick={addDivider}
			>
				<span class="size-2 rounded-sm bg-gray-400"></span>
				Divider
			</button>
		</div>
	{/if}
</div>
