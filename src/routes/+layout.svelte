<script lang="ts">
	import './layout.css';

	let { children } = $props();
</script>

<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4">
		<a href="/" class="text-sm font-semibold tracking-tight text-foreground">Mekhan</a>
		<nav class="ml-8 flex items-center gap-4 text-sm text-muted-foreground">
			<a href="/templates" class="transition-colors hover:text-foreground">Templates</a>
			<a href="/instances" class="transition-colors hover:text-foreground">Instances</a>
		</nav>
	</header>
	<main class="flex-1 overflow-hidden">
		{@render children()}
	</main>
</div>
