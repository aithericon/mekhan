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
import Bot from '@lucide/svelte/icons/bot';
import Box from '@lucide/svelte/icons/box';

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
	phase_update: { icon: Flag, label: 'Phase update',   iconClass: 'text-white', chipClass: 'bg-node-phase-update' },
	progress_update: { icon: Gauge, label: 'Progress update', iconClass: 'text-white', chipClass: 'bg-node-progress-update' },
	trigger: { icon: Zap,      label: 'Trigger',         iconClass: 'text-white', chipClass: 'bg-node-trigger' },
	scope: { icon: Box,        label: 'Scope',           iconClass: 'text-white', chipClass: 'bg-node-scope' }
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
