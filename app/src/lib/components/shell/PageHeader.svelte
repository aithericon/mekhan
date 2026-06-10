<script lang="ts" module>
	import type { Component } from 'svelte';
	/** Any lucide icon (or anything else taking a `class` prop). */
	export type PageIcon = Component<{ class?: string }>;
</script>

<script lang="ts">
	import type { Snippet } from 'svelte';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import { cn } from '$lib/utils.js';

	let {
		title,
		subtitle,
		icon: Icon,
		variant = 'page',
		back,
		actions,
		headTitle,
		titleTestid,
		class: className,
		children
	}: {
		title: string;
		/** One-line muted description under the h1. Needs markup? Use `children` instead. */
		subtitle?: string;
		icon?: PageIcon;
		/**
		 * 'page' (default) → text-2xl h1, for top-level pages.
		 * 'detail' → text-lg h1, for back-linked detail pages.
		 */
		variant?: 'page' | 'detail';
		/** Back-link rendered above the title (detail pages). */
		back?: { href: string; label: string };
		/** Right-aligned action buttons. */
		actions?: Snippet;
		/**
		 * Document title. Defaults to `${title} | Mekhan`; pass a string to
		 * override or `false` to suppress (e.g. a deeper component owns it).
		 */
		headTitle?: string | false;
		titleTestid?: string;
		class?: string;
		/** Extra meta rows (badges, mono ids, counts) rendered under the subtitle. */
		children?: Snippet;
	} = $props();
</script>

<svelte:head>
	{#if headTitle !== false}
		<title>{headTitle ?? `${title} | Mekhan`}</title>
	{/if}
</svelte:head>

<header class={cn('mb-6', className)}>
	{#if back}
		<div class="mb-3">
			<a
				href={back.href}
				class="inline-flex items-center gap-1 text-sm text-muted-foreground transition-colors hover:text-foreground"
			>
				<ChevronLeft class="size-4" />
				{back.label}
			</a>
		</div>
	{/if}
	<div class="flex items-start justify-between gap-4">
		<div class="min-w-0">
			<div class="flex items-center gap-2">
				{#if Icon}
					<Icon class={variant === 'page' ? 'size-6 text-muted-foreground' : 'size-5 text-muted-foreground'} />
				{/if}
				<h1
					class="font-semibold tracking-tight text-foreground {variant === 'page'
						? 'text-2xl'
						: 'text-lg'}"
					data-testid={titleTestid}
				>
					{title}
				</h1>
			</div>
			{#if subtitle}
				<p class="mt-1 text-sm text-muted-foreground">{subtitle}</p>
			{/if}
			{#if children}
				{@render children()}
			{/if}
		</div>
		{#if actions}
			<div class="flex shrink-0 items-center gap-2">
				{@render actions()}
			</div>
		{/if}
	</div>
</header>
