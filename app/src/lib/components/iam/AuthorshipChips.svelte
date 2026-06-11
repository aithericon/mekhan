<!--
  AuthorshipChips — the "who/when" footer for an object (template, instance,
  folder, …). Renders a compact "Created by <UserChip> · <relative>" line, and
  an "Updated by …" line ONLY when the last mutation is meaningfully distinct
  from creation (a different mutator, or a later timestamp). A null `updatedBy`
  with a later `updatedAt` is a System/projector mutation → rendered as the
  literal "System".

  Bare-UUID authorship fields resolve through the shared profile cache
  (`UserChip userId=…`), so a list of N rows still issues ONE batch request.
-->
<script lang="ts">
	import UserChip from './UserChip.svelte';
	import { timeAgo } from '$lib/utils';

	let {
		createdBy,
		createdAt,
		updatedBy,
		updatedAt,
		size = 'xs',
		class: className
	}: {
		createdBy?: string | null;
		createdAt?: string | null;
		updatedBy?: string | null;
		updatedAt?: string | null;
		size?: 'xs' | 'sm';
		class?: string;
	} = $props();

	// Show the "Updated" line only when it adds information: a different
	// mutator, or a later timestamp (a System/projector edit keeps `updatedBy`
	// null but advances `updatedAt`).
	const showUpdated = $derived(
		!!updatedAt &&
			((updatedBy != null && updatedBy !== createdBy) ||
				(!!createdAt && new Date(updatedAt).getTime() - new Date(createdAt).getTime() > 1000))
	);
</script>

<div
	class={['flex flex-col gap-0.5 text-xs text-muted-foreground', className]}
	data-testid="authorship-chips"
>
	{#if createdBy || createdAt}
		<span class="inline-flex items-center gap-1.5">
			<span>Created by</span>
			{#if createdBy}
				<UserChip userId={createdBy} {size} />
			{:else}
				<span class="italic">unknown</span>
			{/if}
			{#if createdAt}<span>· {timeAgo(createdAt)}</span>{/if}
		</span>
	{/if}
	{#if showUpdated}
		<span class="inline-flex items-center gap-1.5" data-testid="authorship-updated">
			<span>Updated by</span>
			{#if updatedBy}
				<UserChip userId={updatedBy} {size} />
			{:else}
				<span class="font-medium">System</span>
			{/if}
			{#if updatedAt}<span>· {timeAgo(updatedAt)}</span>{/if}
		</span>
	{/if}
</div>
