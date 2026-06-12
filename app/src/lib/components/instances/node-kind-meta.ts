/**
 * Per-node-kind visual metadata used by the instance-view drawer to mirror
 * the canvas's node card without re-importing every editor node component.
 *
 * `kindClass` mirrors the colour tokens declared in `tailwind.config` and
 * used by `WorkflowNodeCard` — keep in sync if a new node kind lands.
 */
import type { Component } from 'svelte';
import Play from '@lucide/svelte/icons/play';
import Square from '@lucide/svelte/icons/square';
import Cpu from '@lucide/svelte/icons/cpu';
import User from '@lucide/svelte/icons/user';
import GitBranch from '@lucide/svelte/icons/git-branch';
import Repeat from '@lucide/svelte/icons/repeat';
import GitFork from '@lucide/svelte/icons/git-fork';
import GitMerge from '@lucide/svelte/icons/git-merge';
import Workflow from '@lucide/svelte/icons/workflow';
import OctagonX from '@lucide/svelte/icons/octagon-x';
import Flag from '@lucide/svelte/icons/flag';
import Gauge from '@lucide/svelte/icons/gauge';
import Zap from '@lucide/svelte/icons/zap';
import Timer from '@lucide/svelte/icons/timer';
import TimerOff from '@lucide/svelte/icons/timer-off';
import Bot from '@lucide/svelte/icons/bot';
import Box from '@lucide/svelte/icons/box';
import Server from '@lucide/svelte/icons/server';
import RadioTower from '@lucide/svelte/icons/radio-tower';
import SatelliteDish from '@lucide/svelte/icons/satellite-dish';

export type NodeKindMeta = {
	/** Lucide icon component. */
	icon: Component<{ class?: string }>;
	/** Human-readable label rendered next to the icon. */
	label: string;
	/** Tailwind text colour class for the icon. */
	iconClass: string;
	/** Tailwind background colour class for the icon chip. */
	chipClass: string;
};

const KIND_META: Record<string, NodeKindMeta> = {
	start: { icon: Play,       label: 'Start',           iconClass: 'text-white', chipClass: 'bg-node-start' },
	end: { icon: Square,       label: 'End',             iconClass: 'text-white', chipClass: 'bg-node-end' },
	automated_step: { icon: Cpu, label: 'Automated step', iconClass: 'text-white', chipClass: 'bg-node-automated' },
	agent: { icon: Bot, label: 'Agent', iconClass: 'text-white', chipClass: 'bg-node-agent' },
	human_task: { icon: User,  label: 'Human task',      iconClass: 'text-white', chipClass: 'bg-node-human-task' },
	decision: { icon: GitBranch, label: 'Decision',      iconClass: 'text-white', chipClass: 'bg-node-decision' },
	loop: { icon: Repeat,      label: 'Loop',            iconClass: 'text-white', chipClass: 'bg-node-loop' },
	parallel_split: { icon: GitFork, label: 'Parallel split', iconClass: 'text-white', chipClass: 'bg-node-parallel' },
	join: { icon: GitMerge, label: 'Join', iconClass: 'text-white', chipClass: 'bg-node-parallel' },
	sub_workflow: { icon: Workflow, label: 'Sub-workflow', iconClass: 'text-white', chipClass: 'bg-node-sub-workflow' },
	failure: { icon: OctagonX, label: 'Failure',         iconClass: 'text-white', chipClass: 'bg-node-failure' },
	delay: { icon: Timer,      label: 'Delay',           iconClass: 'text-white', chipClass: 'bg-node-delay' },
	timeout: { icon: TimerOff, label: 'Timeout',         iconClass: 'text-white', chipClass: 'bg-node-timeout' },
	phase_update: { icon: Flag, label: 'Phase update',   iconClass: 'text-white', chipClass: 'bg-node-phase-update' },
	progress_update: { icon: Gauge, label: 'Progress update', iconClass: 'text-white', chipClass: 'bg-node-progress-update' },
	trigger: { icon: Zap,      label: 'Trigger',         iconClass: 'text-white', chipClass: 'bg-node-trigger' },
	scope: { icon: Box,        label: 'Scope',           iconClass: 'text-white', chipClass: 'bg-node-scope' },
	scheduled: { icon: Timer,  label: 'Scheduled',       iconClass: 'text-white', chipClass: 'bg-node-delay' },
	lease_scope: { icon: Server, label: 'Lease scope',   iconClass: 'text-white', chipClass: 'bg-node-scope' },
	stream_source: { icon: RadioTower, label: 'Stream source', iconClass: 'text-white', chipClass: 'bg-node-stream' },
	stream_sink: { icon: SatelliteDish, label: 'Stream sink',  iconClass: 'text-white', chipClass: 'bg-node-stream' }
};

const DEFAULT: NodeKindMeta = {
	icon: Box,
	label: 'Node',
	iconClass: 'text-white',
	chipClass: 'bg-muted'
};

export function nodeKindMeta(kind: string): NodeKindMeta {
	return KIND_META[kind] ?? DEFAULT;
}

/**
 * Normalize the several node-kind discriminants that exist across the two
 * inspector surfaces into a single lookup key:
 *   - the editor binds `WorkflowNodeData.type` (the canvas-side kind),
 *   - the instance drawer reads `StepExecution.node_kind` (the projection's
 *     kind) and falls back to `WorkflowNode.type` (the AIR-side kind).
 * All three are snake_case strings, but they arrive as `string | null |
 * undefined` from different code paths. This coerces any of them to a stable
 * meta key, defaulting to `'unknown'` (→ the DEFAULT chip) when absent. It does
 * NOT narrow to the `NodeKind` union — runtime kinds like `scheduled` /
 * `unknown` exist in the projection that are not editable node types — it just
 * produces the key `nodeKindMeta()` consumes.
 */
export function normalizeNodeKind(kind: string | null | undefined): string {
	return kind && kind.length > 0 ? kind : 'unknown';
}
