// Page-shell layer — the canonical chrome every route page is built from.
// See README.md in this directory for the archetypes + copy-paste skeletons.
// Enforced by shell-conventions.test.ts (raw <h1> / ad-hoc `mx-auto max-w-*`
// containers in src/routes fail the unit lane).
export { default as PageShell, type PageWidth } from './PageShell.svelte';
export { default as PageHeader, type PageIcon } from './PageHeader.svelte';
export { default as PageTabs, type PageTab } from './PageTabs.svelte';
export { default as SideRail } from './SideRail.svelte';
export { default as FilterPills, type FilterPill } from './FilterPills.svelte';
