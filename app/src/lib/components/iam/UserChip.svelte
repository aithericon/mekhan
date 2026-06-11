<!--
  UserChip — avatar + human name for a user UUID.

  Two ways to feed it, so the "denormalize on the row vs. resolve scattered
  UUIDs through the cache" split can't cause a double-fetch:

    - `profile`  — a denormalized row already in hand (member/roster lists).
                   Rendered directly; also seeded into the cache.
    - `userId`   — a bare UUID (authorship fields). Resolved lazily via the
                   `profiles` cache, which coalesces concurrent chips into one
                   batch request. Shows a skeleton until it lands, then the
                   resolved identity (or a shortened UUID if unknown).
-->
<script lang="ts">
	import Avatar from './Avatar.svelte';
	import { profiles } from '$lib/stores/profiles.svelte';
	import type { UserProfileDto } from '$lib/api/client';

	let {
		userId,
		profile,
		size = 'sm',
		showEmail = false,
		class: className
	}: {
		userId?: string | null;
		profile?: UserProfileDto | null;
		size?: 'xs' | 'sm' | 'md' | 'lg';
		showEmail?: boolean;
		class?: string;
	} = $props();

	// A denormalized profile wins; otherwise resolve the bare UUID through the
	// cache. `ensure` is a no-op when already cached/in-flight, so calling it in
	// an effect on every render is cheap.
	$effect(() => {
		if (!profile && userId) profiles.ensure([userId]);
		else if (profile) profiles.seed(profile);
	});

	const resolved = $derived<UserProfileDto | null | undefined>(
		profile ?? (userId ? profiles.get(userId) : undefined)
	);
	const id = $derived(profile?.user_id ?? userId ?? undefined);
	const loading = $derived(!profile && userId != null && resolved === undefined);

	function short(uuid?: string | null): string {
		return uuid ? uuid.slice(0, 8) : 'unknown';
	}

	const displayName = $derived(
		resolved?.display_name ?? resolved?.email ?? short(id)
	);
	const title = $derived(
		[resolved?.display_name, resolved?.email, id].filter(Boolean).join(' · ')
	);
</script>

<span class={['inline-flex min-w-0 items-center gap-1.5', className]} {title}>
	{#if loading}
		<span class="size-6 shrink-0 animate-pulse rounded-full bg-muted"></span>
		<span class="h-3 w-20 animate-pulse rounded bg-muted"></span>
	{:else}
		<Avatar
			{size}
			src={resolved?.avatar_url}
			name={resolved?.display_name}
			email={resolved?.email}
			userId={id}
		/>
		<span class="flex min-w-0 flex-col leading-tight">
			<span class="truncate text-sm">{displayName}</span>
			{#if showEmail && resolved?.email && resolved.email !== displayName}
				<span class="truncate text-xs text-muted-foreground">{resolved.email}</span>
			{/if}
		</span>
	{/if}
</span>
