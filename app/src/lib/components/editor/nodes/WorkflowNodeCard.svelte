<script lang="ts" module>
	import { type VariantProps, tv } from 'tailwind-variants';

	export type WorkflowNodeKind =
		| 'start'
		| 'end'
		| 'decision'
		| 'human-task'
		| 'loop'
		| 'scope'
		| 'parallel'
		| 'automated'
		| 'phase-update'
		| 'progress-update'
		| 'failure'
		| 'sub-workflow';

	// tailwind-variants with explicit per-kind classes — Tailwind's JIT
	// scanner can't expand `border-node-${kind}` dynamically.
	// `bg-card` underlays the alpha gradient so the canvas dot-grid never
	// bleeds through — the colored gradient sits on a solid card surface.
	export const workflowNodeCardVariants = tv({
		base: 'rounded-xl border-2 shadow-sm transition-shadow bg-card bg-linear-to-br',
		variants: {
			kind: {
				'start':       'from-node-start/10       to-node-start/25       border-node-start/60',
				'end':         'from-node-end/10         to-node-end/25         border-node-end/60',
				'decision':    'from-node-decision/10    to-node-decision/25    border-node-decision/60',
				'human-task':  'from-node-human-task/10  to-node-human-task/25  border-node-human-task/60',
				'loop':        'from-node-loop/10        to-node-loop/25        border-node-loop/60',
				'scope':       'from-node-scope/5        to-node-scope/15       border-node-scope/60',
				'parallel':    'from-node-parallel/10    to-node-parallel/25    border-node-parallel/60',
				'automated':   'from-node-automated/10   to-node-automated/25   border-node-automated/60',
				'phase-update':    'from-node-phase-update/10    to-node-phase-update/25    border-node-phase-update/60',
				'progress-update': 'from-node-progress-update/10 to-node-progress-update/25 border-node-progress-update/60',
				'failure':         'from-node-failure/10         to-node-failure/25         border-node-failure/60',
				'sub-workflow':    'from-node-sub-workflow/10    to-node-sub-workflow/25    border-node-sub-workflow/60',
			},
			selected: {
				true: 'shadow-md',
				false: '',
			},
		},
		compoundVariants: [
			{ kind: 'start',      selected: true, class: 'border-node-start' },
			{ kind: 'end',        selected: true, class: 'border-node-end' },
			{ kind: 'decision',   selected: true, class: 'border-node-decision' },
			{ kind: 'human-task', selected: true, class: 'border-node-human-task' },
			{ kind: 'loop',       selected: true, class: 'border-node-loop' },
			{ kind: 'scope',      selected: true, class: 'border-node-scope' },
			{ kind: 'parallel',   selected: true, class: 'border-node-parallel' },
			{ kind: 'automated',  selected: true, class: 'border-node-automated' },
			{ kind: 'phase-update',    selected: true, class: 'border-node-phase-update' },
			{ kind: 'progress-update', selected: true, class: 'border-node-progress-update' },
			{ kind: 'failure',         selected: true, class: 'border-node-failure' },
			{ kind: 'sub-workflow',    selected: true, class: 'border-node-sub-workflow' },
		],
		defaultVariants: {
			kind: 'start',
			selected: false,
		},
	});

	// Per-kind classes for the icon chip background. Same JIT-expansion
	// constraint applies — keep them as full literals.
	const ICON_BG: Record<WorkflowNodeKind, string> = {
		'start':       'bg-node-start',
		'end':         'bg-node-end',
		'decision':    'bg-node-decision',
		'human-task':  'bg-node-human-task',
		'loop':        'bg-node-loop',
		'scope':       'bg-node-scope',
		'parallel':    'bg-node-parallel',
		'automated':   'bg-node-automated',
		'phase-update':    'bg-node-phase-update',
		'progress-update': 'bg-node-progress-update',
		'failure':         'bg-node-failure',
		'sub-workflow':    'bg-node-sub-workflow',
	};

	const HEADER_BORDER: Record<WorkflowNodeKind, string> = {
		'start':       'border-node-start/30',
		'end':         'border-node-end/30',
		'decision':    'border-node-decision/30',
		'human-task':  'border-node-human-task/30',
		'loop':        'border-node-loop/30',
		'scope':       'border-node-scope/30',
		'parallel':    'border-node-parallel/30',
		'automated':   'border-node-automated/30',
		'phase-update':    'border-node-phase-update/30',
		'progress-update': 'border-node-progress-update/30',
		'failure':         'border-node-failure/30',
		'sub-workflow':    'border-node-sub-workflow/30',
	};

	const HANDLE_BORDER: Record<WorkflowNodeKind, string> = {
		'start':       '!border-node-start',
		'end':         '!border-node-end',
		'decision':    '!border-node-decision',
		'human-task':  '!border-node-human-task',
		'loop':        '!border-node-loop',
		'scope':       '!border-node-scope',
		'parallel':    '!border-node-parallel',
		'automated':   '!border-node-automated',
		'phase-update':    '!border-node-phase-update',
		'progress-update': '!border-node-progress-update',
		'failure':         '!border-node-failure',
		'sub-workflow':    '!border-node-sub-workflow',
	};

	export function workflowNodeIconBg(kind: WorkflowNodeKind): string {
		return ICON_BG[kind];
	}

	export function workflowNodeHandleClass(kind: WorkflowNodeKind): string {
		return `!h-3 !w-3 !border-2 !bg-card ${HANDLE_BORDER[kind]}`;
	}

	export type WorkflowNodeCardVariant = VariantProps<typeof workflowNodeCardVariants>;
</script>

<script lang="ts">
	import { cn } from '$lib/utils';
	import type { Component, Snippet } from 'svelte';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
	import NodeRuntimeBadge from '$lib/components/instances/NodeRuntimeBadge.svelte';

	let {
		kind,
		icon,
		label,
		selected = false,
		body,
		class: className,
		'data-testid': dataTestid,
		nodeId,
	}: {
		kind: WorkflowNodeKind;
		icon: Component<{ class?: string }>;
		label: string;
		selected?: boolean;
		body?: Snippet;
		class?: string;
		'data-testid'?: string;
		/// xyflow-provided node id. When set, the card subscribes to the
		/// compile-error store and adds a red ring if this node is flagged by
		/// the latest publish attempt. Reading the store here (rather than
		/// mutating the top-level nodes array from a canvas-level effect) keeps
		/// the reactivity local to the offending card.
		nodeId?: string;
	} = $props();

	const Icon = $derived(icon);
	const compileError = $derived(nodeId ? compileErrors.byNodeId.get(nodeId) : undefined);
</script>

<div
	class={cn(
		workflowNodeCardVariants({ kind, selected }),
		className,
		compileError && 'ring-2 ring-destructive ring-offset-2 ring-offset-background'
	)}
	title={compileError?.message}
	data-testid={dataTestid}
>
	<div
		class={cn(
			'flex items-center gap-2 px-3 py-2',
			body ? `border-b ${HEADER_BORDER[kind]}` : '',
		)}
	>
		<div class={cn('flex size-6 items-center justify-center rounded-md', ICON_BG[kind])}>
			<Icon class="size-3.5 text-white" />
		</div>
		<span class="text-sm font-medium text-foreground">{label}</span>
		{#if nodeId}
			<div class="ml-auto">
				<NodeRuntimeBadge {nodeId} />
			</div>
		{/if}
	</div>
	{#if body}
		<div class="px-3 py-2 text-sm text-muted-foreground">
			{@render body()}
		</div>
	{/if}
</div>
