<script lang="ts">
	// Thin page wrapper for the capability-types admin UI.
	// All list/create/revoke logic lives in CapabilityTypesList.
	import { CapabilityTypesList } from '$lib/components/admin';
</script>

<div class="h-full overflow-y-auto" data-testid="capability-types-page">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Capability Types</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Typed schemas that lab runners advertise at enrolment. Workflow steps use
				<code>requirements</code> constraints to select a matching runner.
			</p>
		</div>
		<CapabilityTypesList />
	</div>
</div>
