/**
 * Frontend-only visual metadata for each node type, keyed by the descriptor's
 * `wireName`. Pairs with the Rust `NodeDecl` registry served at
 * `/api/v1/node-types` — the server is the source of truth for *which*
 * variants exist and their text (`displayLabel`, `description`), but the
 * Lucide icon component imports and Tailwind colour swatches stay here
 * because Svelte components and CSS custom values can't flow through JSON.
 *
 * Adding a new node type: register it in `service/src/nodes/<name>.rs`, then
 * add one entry here. A `Record<string, NodePaletteMeta>` lookup falls back
 * to a sensible default if a variant lands in the registry before the
 * frontend gets an entry — the palette still renders, just with the
 * fallback icon.
 */

import type { Component } from 'svelte';
import Play from '@lucide/svelte/icons/play';
import Square from '@lucide/svelte/icons/square';
import User from '@lucide/svelte/icons/user';
import Cpu from '@lucide/svelte/icons/cpu';
import GitBranch from '@lucide/svelte/icons/git-branch';
import GitFork from '@lucide/svelte/icons/git-fork';
import GitMerge from '@lucide/svelte/icons/git-merge';
import Repeat from '@lucide/svelte/icons/repeat';
import Group from '@lucide/svelte/icons/group';
import Flag from '@lucide/svelte/icons/flag';
import Gauge from '@lucide/svelte/icons/gauge';
import OctagonX from '@lucide/svelte/icons/octagon-x';
import Zap from '@lucide/svelte/icons/zap';
import Workflow from '@lucide/svelte/icons/workflow';
import Bot from '@lucide/svelte/icons/bot';
import Box from '@lucide/svelte/icons/box';

export type NodePaletteMeta = {
	icon: Component<{ class?: string }>;
	/** Hex swatch used by the palette icon chip. */
	color: string;
	/** Cap on instances per workflow — only Start uses this today. */
	maxInstances?: number;
};

const META: Record<string, NodePaletteMeta> = {
	start: { icon: Play, color: '#22c55e', maxInstances: 1 },
	end: { icon: Square, color: '#ef4444' },
	human_task: { icon: User, color: '#3b82f6' },
	automated_step: { icon: Cpu, color: '#8b5cf6' },
	decision: { icon: GitBranch, color: '#f59e0b' },
	parallel_split: { icon: GitFork, color: '#06b6d4' },
	join: { icon: GitMerge, color: '#06b6d4' },
	loop: { icon: Repeat, color: '#ec4899' },
	scope: { icon: Group, color: '#64748b' },
	phase_update: { icon: Flag, color: '#0ea5a4' },
	progress_update: { icon: Gauge, color: '#c026d3' },
	failure: { icon: OctagonX, color: '#dc2626' },
	trigger: { icon: Zap, color: '#fbbf24' },
	sub_workflow: { icon: Workflow, color: '#14b8a6' },
	agent: { icon: Bot, color: '#f97316' }
};

const FALLBACK: NodePaletteMeta = { icon: Box, color: '#64748b' };

export function nodePaletteMeta(wireName: string): NodePaletteMeta {
	return META[wireName] ?? FALLBACK;
}
