<script lang="ts">
	// Top-level Resources page. The full list view + create/edit modal live
	// in `$lib/components/resources/ResourceList.svelte`; this route is the
	// thin page wrapper that gives that component its workspace context
	// and the surrounding chrome (header, padding) that the other top-level
	// routes use.
	import { ResourceList } from '$lib/components/resources';
</script>

<div class="h-full overflow-y-auto" data-testid="resources-page">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Resources</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Typed credentials workflows bind by alias at launch (Postgres, OpenAI, S3, Slack, …). Secrets live in Vault; only public fields are listed here.
			</p>
		</div>
		<ResourceList />
	</div>
</div>
