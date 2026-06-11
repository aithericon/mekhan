<!--
  Avatar — thin wrapper over the bits-ui v2 Avatar primitive.

  Renders the IdP `picture` (when present) and falls back to deterministic
  initials on a stable per-identity background colour when the image is
  absent or fails to load. `referrerpolicy="no-referrer"` keeps the mekhan
  origin out of requests to the external IdP photo host (referrer hygiene).
-->
<script lang="ts" module>
	// Stable, readable fallback palette. The chosen swatch is a pure function
	// of the seed, so a given user always gets the same colour everywhere.
	const PALETTE = [
		'bg-rose-500',
		'bg-orange-500',
		'bg-amber-500',
		'bg-emerald-500',
		'bg-teal-500',
		'bg-sky-500',
		'bg-blue-500',
		'bg-indigo-500',
		'bg-violet-500',
		'bg-fuchsia-500'
	];

	function hash(seed: string): number {
		let h = 0;
		for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) | 0;
		return Math.abs(h);
	}

	export function initialsFor(name?: string | null, email?: string | null, userId?: string | null): string {
		const n = name?.trim();
		if (n) {
			const parts = n.split(/\s+/).filter(Boolean);
			if (parts.length >= 2) return (parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
			if (parts[0].length >= 2) return parts[0].slice(0, 2).toUpperCase();
			return parts[0][0].toUpperCase();
		}
		const local = email?.split('@')[0];
		if (local && local.length >= 2) return local.slice(0, 2).toUpperCase();
		if (local) return local[0].toUpperCase();
		if (userId) return userId.replace(/-/g, '').slice(0, 2).toUpperCase();
		return '?';
	}

	export function colorFor(seed?: string | null): string {
		if (!seed) return 'bg-muted-foreground';
		return PALETTE[hash(seed) % PALETTE.length];
	}
</script>

<script lang="ts">
	import { Avatar as AvatarPrimitive } from 'bits-ui';
	import { cn } from '$lib/utils';

	let {
		src,
		name,
		email,
		userId,
		size = 'md',
		class: className
	}: {
		src?: string | null;
		name?: string | null;
		email?: string | null;
		userId?: string | null;
		size?: 'xs' | 'sm' | 'md' | 'lg';
		class?: string;
	} = $props();

	const sizeClass = {
		xs: 'size-5 text-[9px]',
		sm: 'size-6 text-[10px]',
		md: 'size-8 text-xs',
		lg: 'size-10 text-sm'
	};

	const initials = $derived(initialsFor(name, email, userId));
	const bg = $derived(colorFor(userId ?? name ?? email));
</script>

<AvatarPrimitive.Root
	class={cn(
		'relative inline-flex shrink-0 select-none items-center justify-center overflow-hidden rounded-full',
		sizeClass[size],
		className
	)}
>
	{#if src}
		<AvatarPrimitive.Image
			{src}
			alt={name ?? email ?? 'avatar'}
			referrerpolicy="no-referrer"
			class="aspect-square size-full object-cover"
		/>
	{/if}
	<AvatarPrimitive.Fallback
		class={cn('flex size-full items-center justify-center font-medium text-white', bg)}
	>
		{initials}
	</AvatarPrimitive.Fallback>
</AvatarPrimitive.Root>
