<!--
  StatusBadge — the one renderer for a status pill across the whole surface.

  Feed it a `domain` + `status` and it resolves the tone, label and pulse from
  the shared status registry. Use `dot` for a leading status dot (the in-flight
  states pulse it), and `size="sm"` where the surrounding text is text-sm (e.g.
  the instance header, which forbids sub-text-sm prose) — the default `xs` keeps
  the text-xs footprint the old hand-rolled badges had.

  Replaces ~12 bespoke `statusColors`/`statusConfig` maps. See status-registry.ts.
-->
<script lang="ts">
	import type { Component } from 'svelte';
	import { cn } from '$lib/utils';
	import { resolveStatus, type StatusDomain } from './status-registry';

	let {
		status,
		domain,
		size = 'xs',
		dot = false,
		/** Leading icon (e.g. a lucide component) — alternative to `dot`. */
		icon,
		/** Override the registry label (rarely needed). */
		label,
		/** Capitalize the label — for domains whose statuses are stored lowercase. */
		capitalize = false,
		class: className,
		title
	}: {
		status: string | null | undefined;
		domain: StatusDomain;
		size?: 'xs' | 'sm';
		dot?: boolean;
		icon?: Component<{ class?: string }>;
		label?: string;
		capitalize?: boolean;
		class?: string;
		title?: string;
	} = $props();

	const resolved = $derived(resolveStatus(domain, status));
	const Icon = $derived(icon);
</script>

<span
	class={cn(
		'inline-flex w-fit shrink-0 items-center gap-1.5 rounded-full font-medium whitespace-nowrap',
		size === 'sm' ? 'px-2.5 py-0.5 text-sm' : 'px-2 py-0.5 text-xs',
		capitalize && 'capitalize',
		resolved.style.pill,
		className
	)}
	data-slot="status-badge"
	data-status={status}
	{title}
>
	{#if dot}
		<span
			class={cn('size-1.5 rounded-full', resolved.style.dot, resolved.pulse && 'animate-pulse')}
			aria-hidden="true"
		></span>
	{:else if Icon}
		<Icon class="size-3" />
	{/if}
	{label ?? resolved.label}
</span>
