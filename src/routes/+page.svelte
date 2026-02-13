<script lang="ts">
	import { goto } from '$app/navigation';
</script>

<div class="flex h-full items-center justify-center" data-testid="home-page">
	<div class="text-center">
		<h1 class="text-3xl font-semibold tracking-tight text-foreground">Mekhan</h1>
		<p class="mt-2 text-sm text-muted-foreground">Visual workflow editor for Petri-Lab</p>
		<div class="mt-6 flex items-center justify-center gap-3">
			<a
				href="/templates"
				class="rounded-lg bg-primary px-4 py-2 text-sm font-medium text-primary-foreground transition-colors hover:bg-primary/90"
				data-testid="btn-view-templates"
			>
				View Templates
			</a>
			<a
				href="/instances"
				class="rounded-lg border border-border px-4 py-2 text-sm font-medium text-foreground transition-colors hover:bg-accent"
				data-testid="btn-view-instances"
			>
				View Instances
			</a>
		</div>
	</div>
</div>
