<script lang="ts">
	import './layout.css';
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { TooltipProvider } from '$lib/components/ui/tooltip';
	import User from '@lucide/svelte/icons/user';
	import { auth } from '$lib/auth/store.svelte';
	import { ensureAuthInitialized, requireSession } from '$lib/auth/guard';

	let { children } = $props();

	onMount(async () => {
		// BFF: the backend owns the OIDC callback (302s straight to the SPA),
		// so there is no client `/auth/*` route to exempt. A single session
		// probe gates every route; dev_noop always passes.
		await ensureAuthInitialized();
		await requireSession();
	});

	async function signOut() {
		await auth.signOut();
	}
</script>

<TooltipProvider>
<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4" data-testid="app-header">
		<a href="/" class="text-sm font-semibold tracking-tight text-foreground" data-testid="nav-home">Mekhan</a>
		<nav class="ml-8 flex flex-1 items-center gap-1 text-sm" data-testid="nav-bar">
			<Button variant="ghost" size="sm" href="/templates" data-testid="nav-templates">Templates</Button>
			<Button variant="ghost" size="sm" href="/instances" data-testid="nav-instances">Instances</Button>
			<Button variant="ghost" size="sm" href="/tasks" data-testid="nav-tasks">Tasks</Button>
			<Button variant="ghost" size="sm" href="/catalogue" data-testid="nav-catalogue">Catalogue</Button>
			<span class="mx-1 h-4 w-px bg-border" aria-hidden="true"></span>
			<Button
				variant="ghost"
				size="sm"
				href="/nets"
				data-testid="nav-nets"
				class="text-muted-foreground"
				title="Engine debug: raw petri nets"
			>
				Engine
			</Button>
			<Button
				variant="ghost"
				size="sm"
				href="/processes"
				data-testid="nav-processes"
				class="text-muted-foreground"
				title="Engine debug: raw processes (usually accessed via an instance)"
			>
				Processes
			</Button>
			{#if auth.isAuthenticated}
				<span class="ml-auto h-4 w-px bg-border" aria-hidden="true"></span>
				<Button
					variant="ghost"
					size="sm"
					href="/profile"
					data-testid="nav-user"
					class="gap-1.5 text-muted-foreground"
					title="View profile"
				>
					<User class="size-3.5" />
					{auth.session?.user.displayName ?? auth.session?.user.email ?? 'Profile'}
				</Button>
				<Button
					variant="ghost"
					size="sm"
					data-testid="nav-logout"
					class="text-muted-foreground"
					onclick={signOut}
				>
					Sign out
				</Button>
			{/if}
		</nav>
	</header>
	<main class="flex-1 overflow-hidden">
		{@render children()}
	</main>
</div>
</TooltipProvider>
