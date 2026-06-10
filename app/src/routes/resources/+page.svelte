<script lang="ts">
	// Top-level Resources page. The full list view + create/edit modal live
	// in `$lib/components/resources/ResourceList.svelte`; this route is the
	// thin page wrapper that gives that component its workspace context
	// and the surrounding chrome (header, padding) that the other top-level
	// routes use.
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { ResourceList } from '$lib/components/resources';
</script>

<PageShell testid="resources-page">
	{#snippet band()}
		<PageHeader
			title="Resources"
			subtitle="Typed credentials workflows bind by alias at launch (Postgres, OpenAI, S3, Slack, …). Secrets live in Vault; only public fields are listed here."
		/>
	{/snippet}
	<ResourceList />
</PageShell>
