<script lang="ts">
	// Thin wrapper for the `human_task` registry entry. When a `templateId` +
	// `nodeId` are present the panel is rendered inside a template editor, so
	// task-form authoring lives on a dedicated IDE route reached via a button;
	// otherwise (e.g. preview contexts) the inline HumanTaskSection is shown.
	// Keeping this branch in a wrapper lets the registry stay a flat
	// `Record<NodeKind, Component>` while preserving the original behaviour.
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Pencil from '@lucide/svelte/icons/pencil';
	import { Button } from '$lib/components/ui/button';
	import HumanTaskSection from './HumanTaskSection.svelte';
	import OfferCapacitySection from './human-task/OfferCapacitySection.svelte';

	type Props = {
		data: HumanTaskNodeData;
		readonly?: boolean;
		onchange: (data: HumanTaskNodeData) => void;
		nodeId?: string;
		templateId?: string;
		scope?: ScopeEntry[];
	};

	let { data, readonly = false, onchange, nodeId, templateId, scope = [] }: Props = $props();

	// Defensive: a freshly-dropped node may not have `steps` yet — guard so the
	// summary never throws (an unguarded `.length` blanks the whole panel).
	const stepCount = $derived(data.steps?.length ?? 0);
</script>

<div class="space-y-4">
	<!-- Capacity / consent-acceptance binding (docs/33) — always configurable,
	     independent of where the task FORM is authored. -->
	<OfferCapacitySection {data} {readonly} {onchange} />

	{#if templateId && nodeId}
		<div class="space-y-3">
			<div class="rounded-lg border border-border bg-muted/30 p-3">
				<p class="text-sm text-muted-foreground">
					{stepCount} step{stepCount !== 1 ? 's' : ''} configured
				</p>
				{#if data.taskTitle}
					<p class="mt-1 truncate text-sm font-medium text-foreground">{data.taskTitle}</p>
				{/if}
			</div>
			<Button
				variant="outline"
				size="sm"
				class="w-full"
				href="/templates/{templateId}/ide?node={nodeId}"
			>
				<Pencil class="size-3.5" />
				Edit Task Form
			</Button>
		</div>
	{:else}
		<HumanTaskSection {data} {readonly} {onchange} {scope} />
	{/if}
</div>
