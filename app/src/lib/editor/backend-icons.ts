// Per-executor glyphs for built-in AutomatedStep backends.
//
// The WHICH-icon decision is owned SERVER-SIDE: every backend ships an icon
// NAME on its `BackendMeta` (shared/backends/src/registry.rs), surfaced via
// `GET /api/v1/backends` → `BackendDescriptor.icon`. This module only resolves
// that name string to a concrete Svelte component — icon components and their
// imports can't travel through JSON. One source of truth for the choice; the
// Rust registry and the editor can't drift on it.
//
// Two glyph families resolve through the same name map:
//   • Backends with a recognizable product mark use their real BRAND logo
//     (python→Python, docker→Docker, postgresql→PostgreSQL, prometheus→
//     Prometheus, grafana→Grafana [the Loki backend], ros→ROS). These are
//     vendored single-path SVGs under `./brand-icons` (sourced from Simple
//     Icons, CC0), rendered in `currentColor` so they tint like any glyph.
//   • The rest fall back to a generic Lucide glyph (http→globe, smtp→mail,
//     process→terminal, llm→sparkles, file_ops→folder, …) — no brand exists.
//
// Pairs with `node-palette-meta.ts` (which keys the GENERIC palette glyph by
// node type): there the automated-step PRIMITIVE shows a single neutral chip,
// because no backend is chosen at drop time. Once a backend is selected, the
// canvas card and the backend picker resolve through here instead.

import type { Component } from 'svelte';
import { getCachedBackend, type ExecutionBackendType } from './backend-registry.svelte';

import Cpu from '@lucide/svelte/icons/cpu';
import Code from '@lucide/svelte/icons/code';
import Terminal from '@lucide/svelte/icons/terminal';
import Container from '@lucide/svelte/icons/container';
import Globe from '@lucide/svelte/icons/globe';
import Sparkles from '@lucide/svelte/icons/sparkles';
import FolderOpen from '@lucide/svelte/icons/folder-open';
import FileSearch from '@lucide/svelte/icons/file-search';
import ScanText from '@lucide/svelte/icons/scan-text';
import Mail from '@lucide/svelte/icons/mail';
import DatabaseZap from '@lucide/svelte/icons/database-zap';
import Database from '@lucide/svelte/icons/database';
import ScrollText from '@lucide/svelte/icons/scroll-text';
import Activity from '@lucide/svelte/icons/activity';
import Bot from '@lucide/svelte/icons/bot';

import Python from './brand-icons/Python.svelte';
import Docker from './brand-icons/Docker.svelte';
import Postgresql from './brand-icons/Postgresql.svelte';
import Prometheus from './brand-icons/Prometheus.svelte';
import Grafana from './brand-icons/Grafana.svelte';
import Ros from './brand-icons/Ros.svelte';

type IconComponent = Component<{ class?: string }>;

// Icon name (as emitted by the Rust backend registry) → bundled component.
// Keep in sync with the `icon:` fields in shared/backends/src/registry.rs; an
// unknown name degrades to the Cpu fallback rather than rendering nothing.
const BY_NAME: Record<string, IconComponent> = {
	// Brand marks (vendored from Simple Icons, CC0).
	python: Python,
	docker: Docker,
	postgresql: Postgresql,
	prometheus: Prometheus,
	grafana: Grafana,
	ros: Ros,
	// Generic Lucide glyphs for backends with no recognizable brand.
	code: Code,
	terminal: Terminal,
	container: Container,
	globe: Globe,
	sparkles: Sparkles,
	'folder-open': FolderOpen,
	'file-search': FileSearch,
	'scan-text': ScanText,
	mail: Mail,
	'database-zap': DatabaseZap,
	database: Database,
	'scroll-text': ScrollText,
	activity: Activity,
	bot: Bot,
	cpu: Cpu
};

// Generic automated-step glyph — the historical icon, used while the backend
// registry is still loading or for a backend whose icon name isn't bundled yet.
const FALLBACK: IconComponent = Cpu;

/**
 * Resolve a server-published icon NAME (brand mark or generic Lucide glyph) to
 * a component, falling back to Cpu for empty/unknown names.
 */
export function iconByName(name: string | null | undefined): IconComponent {
	if (!name) return FALLBACK;
	return BY_NAME[name] ?? FALLBACK;
}

/**
 * The glyph for an automated step's executor backend. Reads the icon name the
 * server published for that backend (cached via `loadBackends()` in the root
 * layout) and resolves it to a component. Returns Cpu until the registry
 * resolves or for an unknown backend.
 *
 * Reads the reactive registry `$state`, so calling this inside a `$derived`
 * re-runs it once `loadBackends()` resolves — the canvas swaps from the Cpu
 * placeholder to the real backend glyph without an explicit refresh.
 */
export function backendIcon(
	backendType: ExecutionBackendType | string | null | undefined
): IconComponent {
	if (!backendType) return FALLBACK;
	const desc = getCachedBackend(backendType as ExecutionBackendType);
	return iconByName(desc?.icon);
}
