// Named icon registry for library / vendor nodes (decision 9).
//
// A library node's `presentation.icon` is a STRING key resolved here to a
// concrete Svelte icon component — never raw SVG (XSS). Unknown keys fall back
// to a generic workflow glyph so a stale/typo'd key degrades gracefully rather
// than rendering nothing. Custom asset-backed icons (an uploaded logo via the
// asset system, rendered as <img>) are a later phase; this registry is the
// safe, bundled baseline.
//
// Keys are intentionally vendor/domain-flavored (`openfoam`, `mumax3`, `cfd`)
// but currently map onto curated Lucide glyphs. As real vendor logo components
// are vendored in, swap the mapping value — callers keep using the same key.

import type { Component } from 'svelte';
import Workflow from '@lucide/svelte/icons/workflow';
import FlaskConical from '@lucide/svelte/icons/flask-conical';
import Wind from '@lucide/svelte/icons/wind';
import Magnet from '@lucide/svelte/icons/magnet';
import Atom from '@lucide/svelte/icons/atom';
import Waypoints from '@lucide/svelte/icons/waypoints';
import HandHelping from '@lucide/svelte/icons/hand-helping';
import Boxes from '@lucide/svelte/icons/boxes';
import Cpu from '@lucide/svelte/icons/cpu';
import Sigma from '@lucide/svelte/icons/sigma';
import Microscope from '@lucide/svelte/icons/microscope';
import Beaker from '@lucide/svelte/icons/beaker';

type IconComponent = Component<{ class?: string }>;

const REGISTRY: Record<string, IconComponent> = {
	// Generic / fallback-adjacent
	workflow: Workflow,
	waypoints: Waypoints,
	boxes: Boxes,
	cpu: Cpu,
	sigma: Sigma,
	'hand-helping': HandHelping,
	// Domain / vendor flavored (mapped to curated Lucide glyphs for now)
	cfd: Wind,
	openfoam: Wind,
	micromagnetics: Magnet,
	mumax3: Magnet,
	physics: Atom,
	chemistry: FlaskConical,
	lab: Beaker,
	microscopy: Microscope
};

const FALLBACK: IconComponent = Workflow;

/** Resolve a presentation icon key to a component, falling back to a generic
 *  glyph for unknown/empty keys. */
export function resolveNodeIcon(key: string | null | undefined): IconComponent {
	if (!key) return FALLBACK;
	return REGISTRY[key] ?? FALLBACK;
}

/** Whether a key is a known registry entry (for authoring UIs to flag typos). */
export function isKnownIcon(key: string): boolean {
	return key in REGISTRY;
}

/** Sorted list of registry keys — feeds the (future) icon picker in the
 *  promote/presentation editor. */
export function iconRegistryKeys(): string[] {
	return Object.keys(REGISTRY).sort();
}
