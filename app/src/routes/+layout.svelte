<script lang="ts">
	import './layout.css';
	import { Button } from '$lib/components/ui/button';
	import { TooltipProvider } from '$lib/components/ui/tooltip';

	let { children } = $props();
</script>

<TooltipProvider>
<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4" data-testid="app-header">
		<a href="/" class="text-sm font-semibold tracking-tight text-foreground" data-testid="nav-home">Mekhan</a>
		<nav class="ml-8 flex items-center gap-1 text-sm" data-testid="nav-bar">
			<Button variant="ghost" size="sm" href="/demo" data-testid="nav-demo">Demo</Button>
			<Button variant="ghost" size="sm" href="/templates" data-testid="nav-templates">Templates</Button>
			<Button variant="ghost" size="sm" href="/instances" data-testid="nav-instances">Instances</Button>
			<Button variant="ghost" size="sm" href="/tasks" data-testid="nav-tasks">Tasks</Button>
			<Button variant="ghost" size="sm" href="/processes" data-testid="nav-processes">Processes</Button>
			<Button variant="ghost" size="sm" href="/nets" data-testid="nav-nets">Nets</Button>
			<Button variant="ghost" size="sm" href="/catalogue" data-testid="nav-catalogue">Catalogue</Button>
		</nav>
	</header>
	<main class="flex-1 overflow-hidden">
		{@render children()}
	</main>
</div>
</TooltipProvider>
