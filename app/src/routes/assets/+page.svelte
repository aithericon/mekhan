<script lang="ts">
	// Top-level Assets page. The full browser (types + collections, scope
	// selector, folder grouping) and the create/edit modals live in
	// `$lib/components/assets/AssetList.svelte`; this route is the thin page
	// wrapper providing the surrounding chrome, matching the /resources route.
	import { AssetList } from '$lib/components/assets';
</script>

<div class="h-full overflow-y-auto" data-testid="assets-page">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Assets</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Curated, user-typed static content (material parameters, scripts, reference data). Define a
				schema, populate records via the table builder or CSV, and bind collections into workflow
				nodes as ordinary staged inputs.
			</p>
		</div>
		<AssetList />
	</div>
</div>
