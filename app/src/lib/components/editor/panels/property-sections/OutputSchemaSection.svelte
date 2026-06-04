<script lang="ts">
	/**
	 * Collapsible "Output schema" section for the node property panel.
	 *
	 * Shows the recursive `SchemaView` type tree for a node's output contract.
	 * Accepts a `SchemaNode` directly — callers convert from a `Port` via
	 * `portToSchemaNode` or from a `TyDescriptor` via `tyDescriptorToSchemaNode`
	 * before passing in. When `node` is absent, or is a scalar/any/opaque (not
	 * worth expanding), the section renders nothing — it only shows for
	 * `object` and `array` types.
	 *
	 * This section is additive and default-collapsed; it does not replace the
	 * existing DerivedPortsSection or PortsSection affordances.
	 */
	import type { SchemaNode } from '$lib/schema/model';
	import SchemaView from '$lib/schema/SchemaView.svelte';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	type Props = {
		/** The schema node to display. When absent or scalar/any/opaque, renders nothing. */
		node: SchemaNode | undefined;
		/** Section title. Defaults to "Output schema". */
		title?: string;
	};

	let { node, title = 'Output schema' }: Props = $props();

	// Only show for structured types worth exploring.
	const shouldShow = $derived(
		node !== undefined && (node.kind === 'object' || node.kind === 'array')
	);

	// Hint shown in the collapsed header.
	const hint = $derived.by(() => {
		if (!node) return '';
		if (node.kind === 'object') {
			const n = node.fields.size;
			return `${n} field${n === 1 ? '' : 's'}`;
		}
		return 'array';
	});

	let open = $state(false);
</script>

{#if shouldShow && node !== undefined}
	<div class="space-y-1.5">
		<button
			type="button"
			class="flex w-full items-center justify-between text-sm font-medium text-muted-foreground hover:text-foreground transition-colors"
			onclick={() => (open = !open)}
			aria-expanded={open}
		>
			<span>{title}</span>
			<div class="flex items-center gap-1">
				<span class="text-xs text-muted-foreground/60">{hint}</span>
				{#if open}
					<ChevronDown class="size-3.5 shrink-0" />
				{:else}
					<ChevronRight class="size-3.5 shrink-0" />
				{/if}
			</div>
		</button>

		{#if open}
			<div class="rounded-md border border-border/60 bg-muted/10 p-2">
				<SchemaView {node} />
			</div>
		{/if}
	</div>
{/if}
