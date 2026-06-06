<script lang="ts">
	// ROUTER tab — the inference audit ledger. Inference bypasses the engine net
	// (the HTTP router meters directly), so GET /api/v1/inference/requests is the
	// only durable record of who served what, with which token counts and outcome
	// — the metering / GDPR ledger. Read-only.
	import InferenceAuditTable from '$lib/components/fleet/InferenceAuditTable.svelte';
</script>

<div class="space-y-4" data-testid="models-router">
	<div class="flex items-baseline gap-3">
		<h2 class="text-sm font-semibold tracking-tight text-foreground">Router</h2>
		<span class="text-sm text-muted-foreground">
			inference audit — newest first (the router meters directly; inference never crosses the engine net)
		</span>
	</div>
	<InferenceAuditTable />
</div>
