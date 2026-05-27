<script lang="ts">
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';

	type Props = {
		binding: YjsGraphBinding;
		nodeId: string;
		readonly?: boolean;
	};

	let { binding, nodeId, readonly = false }: Props = $props();

	// Only show ourselves when the node's parent is an Agent. The
	// AutomatedStepSection (and future child-capable sections) renders us
	// unconditionally and lets us gate, so the parent-detection logic lives
	// in one place.
	const node = $derived(binding.graph.nodes.find((n) => n.id === nodeId));
	const parent = $derived.by(() => {
		const pid = node?.parentId;
		if (!pid) return null;
		return binding.graph.nodes.find((n) => n.id === pid) ?? null;
	});
	const parentIsAgent = $derived(parent?.type === 'agent');

	const toolName = $derived(node?.toolMeta?.toolName ?? '');
	const toolDescription = $derived(node?.toolMeta?.toolDescription ?? '');

	const TOOL_NAME_PATTERN = /^[a-z][a-z0-9_]*$/;
	const nameError = $derived.by(() => {
		const v = toolName.trim();
		if (!v) return null;
		if (!TOOL_NAME_PATTERN.test(v))
			return 'Lowercase letter, then letters/digits/underscore (e.g. lookup_invoice).';
		// Name must be unique among the agent's other tool children. The
		// compiler enforces this too; surfacing it here lets the author fix
		// it before publish.
		const dup = binding.graph.nodes.some(
			(n) =>
				n.id !== nodeId &&
				n.parentId === node?.parentId &&
				(n.toolMeta?.toolName ?? '').trim() === v
		);
		return dup ? `Another tool on this agent already uses "${v}".` : null;
	});

	function setName(v: string) {
		binding.updateNodeToolMeta(nodeId, {
			toolName: v,
			toolDescription
		});
	}

	function setDescription(v: string) {
		binding.updateNodeToolMeta(nodeId, {
			toolName,
			toolDescription: v
		});
	}

	function untag() {
		binding.updateNodeToolMeta(nodeId, null);
	}
</script>

{#if parentIsAgent}
	<div class="space-y-2 border-t border-border/40 pt-3" data-testid="tool-meta-section">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">Tool metadata</span>
			{#if !readonly && (toolName || toolDescription)}
				<button
					type="button"
					class="text-sm text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
					onclick={untag}
					data-testid="tool-meta-untag"
				>
					Untag
				</button>
			{/if}
		</div>
		<p class="text-sm text-muted-foreground">
			This node is a child of an Agent. Give it a tool name to expose it to the LLM.
		</p>
		<FormField label="Tool name" for="tool-meta-name">
			<Input
				id="tool-meta-name"
				type="text"
				value={toolName}
				placeholder="lookup_invoice"
				disabled={readonly}
				aria-invalid={nameError ? 'true' : undefined}
				oninput={(e) => setName((e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
				data-testid="tool-meta-name-input"
			/>
			{#if nameError}
				<p class="mt-1 text-sm text-destructive" data-testid="tool-meta-name-error">
					{nameError}
				</p>
			{:else}
				<p class="mt-1 text-sm text-muted-foreground">
					Rhai-identifier-safe. Unique among this agent's tools. Empty ⇒ child is inert (not a tool).
				</p>
			{/if}
		</FormField>
		<FormField label="Tool description" for="tool-meta-desc">
			<Textarea
				id="tool-meta-desc"
				value={toolDescription}
				placeholder="What this tool does, when the model should call it."
				disabled={readonly}
				oninput={(e) => setDescription((e.currentTarget as HTMLTextAreaElement).value)}
				rows={2}
				data-testid="tool-meta-desc-input"
			/>
			<p class="mt-1 text-sm text-muted-foreground">
				Shown to the LLM verbatim in the tool listing. Aim for one sentence.
			</p>
		</FormField>
	</div>
{/if}
