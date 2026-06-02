<script lang="ts">
	// Compact inline badge for a TemplateStaging status, with optional tooltip
	// for remote_ref (staged) and last_error (failed).
	//
	// Badge palette mirrors ClusterLeasesTable:
	//   staging → amber  |  staged → green  |  failed → red  |  stale → slate

	import { Badge } from '$lib/components/ui/badge';
	import type { TemplateStaging } from '$lib/api/job-templates';

	type Props = {
		staging: TemplateStaging | null | undefined;
	};

	let { staging }: Props = $props();

	const statusClass = $derived(
		staging?.status === 'staging'
			? 'bg-amber-100 text-amber-700'
			: staging?.status === 'staged'
				? 'bg-green-100 text-green-700'
				: staging?.status === 'failed'
					? 'bg-red-100 text-red-700'
					: staging?.status === 'stale'
						? 'bg-slate-100 text-slate-600'
						: 'bg-muted text-muted-foreground'
	);

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
			<Badge
				class="{statusClass} font-normal text-xs"
				variant="secondary"
				title={tooltip}
			>
				{staging.status}
			</Badge>
			{#if staging.status === 'staging'}
				<!-- Pulse dot while staging is in progress -->
				<span
					class="inline-block size-1.5 animate-pulse rounded-full bg-amber-500"
					aria-hidden="true"
				></span>
			{/if}
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
