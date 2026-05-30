/**
 * config-spec/custom-registry.ts
 *
 * The ONLY file in the config-spec layer that imports .svelte files.
 * Maps string keys (used in CustomField.component) to bespoke Svelte
 * components that receive the full SectionProps context.
 *
 * Keys are namespaced `<node>.<region>` to make ownership obvious and avoid
 * collisions. specs.ts only ever references the string key — it never imports
 * from this file, keeping specs pure data / serializable.
 *
 * FieldRenderer imports resolveCustom from here, NOT individual components.
 */

// eslint-disable-next-line @typescript-eslint/no-explicit-any
import type { Component } from 'svelte';

import StartEntrypoints from '$lib/components/editor/panels/config-spec/custom/StartEntrypoints.svelte';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export const customRegistry: Record<string, Component<any>> = {
	'start.entrypoints': StartEntrypoints
};

/**
 * Look up a bespoke component by registry key.
 * Returns undefined for unknown keys — FieldRenderer renders a visible
 * dev-guard placeholder rather than silently dropping the region.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function resolveCustom(key: string): Component<any> | undefined {
	return customRegistry[key];
}
