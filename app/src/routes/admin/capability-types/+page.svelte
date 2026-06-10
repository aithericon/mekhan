<script lang="ts">
	// Thin page wrapper for the capability-types admin UI.
	// All list/create/revoke logic lives in CapabilityTypesList.
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { CapabilityTypesList } from '$lib/components/admin';
</script>

<PageShell testid="capability-types-page">
	<PageHeader title="Capability Types">
		<!-- Subtitle needs inline markup (<code>) — rendered via children. -->
		<p class="mt-1 text-sm text-muted-foreground">
			Typed schemas that lab runners advertise at enrolment. Workflow steps use
			<code>requirements</code> constraints to select a matching runner.
		</p>
	</PageHeader>
	<CapabilityTypesList />
</PageShell>
