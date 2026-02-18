<script lang="ts">
	import './layout.css';

	let { children } = $props();
</script>

<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4" data-testid="app-header">
		<a href="/" class="text-sm font-semibold tracking-tight text-foreground" data-testid="nav-home">Mekhan</a>
		<nav class="ml-8 flex items-center gap-4 text-sm text-muted-foreground" data-testid="nav-bar">
			<a href="/demo" class="transition-colors hover:text-foreground" data-testid="nav-demo">Demo</a>
			<a href="/templates" class="transition-colors hover:text-foreground" data-testid="nav-templates">Templates</a>
			<a href="/instances" class="transition-colors hover:text-foreground" data-testid="nav-instances">Instances</a>
		</nav>
	</header>
	<main class="flex-1 overflow-hidden">
		{@render children()}
	</main>
</div>
