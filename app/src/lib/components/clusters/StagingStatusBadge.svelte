<script lang="ts">
	// Compact inline badge for a TemplateStaging status, with optional tooltip
	// for remote_ref (staged) and last_error (failed).
	//
	// Badge palette mirrors ClusterLeasesTable:
	//   staging → amber  |  staged → green  |  failed → red  |  stale → slate

	import { StatusBadge } from '$lib/components/status';
	import type { TemplateStaging } from '$lib/api/job-templates';

	type Props = {
		staging: TemplateStaging | null | undefined;
	};

	let { staging }: Props = $props();

	/** Tooltip: remote_ref for staged, last_error for failed. */
	const tooltip = $derived(
		staging?.status === 'staged' && staging.remote_ref
			? staging.remote_ref
			: staging?.status === 'failed' && staging.last_error
				? staging.last_error
				: undefined
	);

	/** Truncated remote_ref shown inline when status is staged. */
	const sublabel = $derived(
		staging?.status === 'staged' && staging.remote_ref
			? staging.remote_ref.length > 32
				? `${staging.remote_ref.slice(0, 28)}…`
				: staging.remote_ref
			: null
	);
</script>

{#if !staging}
	<span class="text-sm text-muted-foreground">—</span>
{:else}
	<div class="flex flex-col gap-0.5">
		<div class="flex items-center gap-1.5">
			<StatusBadge domain="staging" status={staging.status} title={tooltip} dot />
		</div>
		{#if sublabel}
			<span
				class="max-w-[14rem] truncate font-mono text-xs text-muted-foreground"
				title={staging.remote_ref ?? undefined}
			>
				{sublabel}
			</span>
		{/if}
		{#if staging.status === 'failed' && staging.last_error}
			<span
				class="max-w-[18rem] truncate text-xs text-destructive"
				title={staging.last_error}
			>
				{staging.last_error}
			</span>
		{/if}
	</div>
{/if}
