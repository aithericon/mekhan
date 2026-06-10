<script lang="ts">
	// Top-level Assets page. The full browser (types + collections, scope
	// selector, folder grouping) and the create/edit modals live in
	// `$lib/components/assets/AssetList.svelte`; this route is the thin page
	// wrapper providing the surrounding chrome, matching the /resources route.
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { AssetList } from '$lib/components/assets';
</script>

<PageShell testid="assets-page">
	<PageHeader
		title="Assets"
		subtitle="Curated, user-typed static content (material parameters, scripts, reference data). Define a schema, populate records via the table builder or CSV, and bind collections into workflow nodes as ordinary staged inputs."
	/>
	<AssetList />
</PageShell>
