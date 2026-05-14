<script lang="ts">
	import './layout.css';
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { Button } from '$lib/components/ui/button';
	import { TooltipProvider } from '$lib/components/ui/tooltip';
	import { findOrCreateShowcaseTemplate } from '$lib/templates/showcase';
	import { auth } from '$lib/auth/store.svelte';
	import { ensureAuthInitialized, requireSession } from '$lib/auth/guard';

	let { children } = $props();
	let openingDemo = $state(false);

	onMount(async () => {
		await ensureAuthInitialized();
		// The /auth/callback route runs the redirect handshake — never gate it.
		if (page.url.pathname.startsWith('/auth/')) return;
		await requireSession();
	});

	async function openDemo() {
		if (openingDemo) return;
		openingDemo = true;
		try {
			const template = await findOrCreateShowcaseTemplate();
			await goto(`/templates/${template.id}`);
		} finally {
			openingDemo = false;
		}
	}
</script>

<TooltipProvider>
<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4" data-testid="app-header">
		<a href="/" class="text-sm font-semibold tracking-tight text-foreground" data-testid="nav-home">Mekhan</a>
		<nav class="ml-8 flex items-center gap-1 text-sm" data-testid="nav-bar">
			<Button variant="ghost" size="sm" data-testid="nav-demo" disabled={openingDemo} onclick={openDemo}>
				{openingDemo ? 'Opening…' : 'Demo'}
			</Button>
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
