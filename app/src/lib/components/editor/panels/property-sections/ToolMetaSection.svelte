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

	// Only show ourselves when the node is wired as a tool — i.e. it's the
	// target of an edge from some agent's `tools` source handle. The
	// AutomatedStepSection (and future tool-capable sections) renders us
	// unconditionally and lets us gate, so the binding-detection logic
	// lives in one place. parent_id is no longer the binding mechanism.
	const node = $derived(binding.graph.nodes.find((n) => n.id === nodeId));
	const owningAgentId = $derived.by(() => {
		if (!node) return null;
		const e = binding.graph.edges.find(
			(e) => e.target === nodeId && e.sourceHandle === 'tools'
		);
		return e ? e.source : null;
	});
	const owningAgent = $derived.by(() => {
		if (!owningAgentId) return null;
		return binding.graph.nodes.find((n) => n.id === owningAgentId) ?? null;
	});
	const isAgentTool = $derived(owningAgent?.type === 'agent');

	const toolName = $derived(node?.toolMeta?.toolName ?? '');
	const toolDescription = $derived(node?.toolMeta?.toolDescription ?? '');

	const TOOL_NAME_PATTERN = /^[a-z][a-z0-9_]*$/;
	const nameError = $derived.by(() => {
		const v = toolName.trim();
		if (!v) return null;
		if (!TOOL_NAME_PATTERN.test(v))
			return 'Lowercase letter, then letters/digits/underscore (e.g. lookup_invoice).';
		// Name must be unique among the owning agent's other connected tools.
		// The compiler enforces this too; surfacing it here lets the author
		// fix it before publish. Sibling tools = other nodes the same agent
		// reaches via its `tools` handle.
		if (!owningAgentId) return null;
		const siblingIds = new Set(
			binding.graph.edges
				.filter((e) => e.source === owningAgentId && e.sourceHandle === 'tools')
				.map((e) => e.target)
		);
		const dup = binding.graph.nodes.some(
			(n) =>
				n.id !== nodeId &&
				siblingIds.has(n.id) &&
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

{#if isAgentTool}
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
			This node is wired to an Agent's <code>tools</code> handle. The LLM picks tools to call by
			name — without a name + description here, the LLM can't see this node, so the agent will
			never invoke it.
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
					The identifier the model calls in its tool_use turn (e.g. <code>lookup_invoice</code>).
					Lowercase + underscores; must be unique among this agent's tools.
				</p>
			{/if}
		</FormField>
		<FormField label="Tool description" for="tool-meta-desc">
			<Textarea
				id="tool-meta-desc"
				value={toolDescription}
				placeholder="One sentence: what it does and when to call it. E.g. 'Look up an invoice by id.'"
				disabled={readonly}
				aria-invalid={isAgentTool && !toolDescription.trim() ? 'true' : undefined}
				oninput={(e) => setDescription((e.currentTarget as HTMLTextAreaElement).value)}
				rows={2}
				data-testid="tool-meta-desc-input"
			/>
			<p class="mt-1 text-sm text-muted-foreground">
				The model reads this verbatim to decide when to call the tool — make it concrete (what
				it does + when to use it). One sentence is usually enough.
			</p>
		</FormField>
	</div>
{/if}
