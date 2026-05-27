<script lang="ts">
	import './layout.css';
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { TooltipProvider } from '$lib/components/ui/tooltip';
	import { ModeWatcher } from 'mode-watcher';
	import ThemeToggle from '$lib/components/ThemeToggle.svelte';
	import User from '@lucide/svelte/icons/user';
	import { auth } from '$lib/auth/store.svelte';
	import { ensureAuthInitialized, requireSession } from '$lib/auth/guard';
	import { loadBackends } from '$lib/editor/backend-registry.svelte';
	import { loadNodeTypes } from '$lib/editor/node-registry.svelte';

	let { children } = $props();

	onMount(async () => {
		// BFF: the backend owns the OIDC callback (302s straight to the SPA),
		// so there is no client `/auth/*` route to exempt. A single session
		// probe gates every route; dev_noop always passes.
		await ensureAuthInitialized();
		await requireSession();
		// Warm the backend registry cache so the editor's "Reset to backend
		// default" resolves synchronously on first paint. Non-fatal: the
		// hardcoded TS twin in automated-ports.ts is the fallback.
		loadBackends().catch(() => { /* swallowed: TS twin remains */ });
		// Same pattern for the node-type registry; palette renders empty
		// until this resolves on first paint, which is acceptable since the
		// /templates/[id]/edit route is the only place that uses it.
		loadNodeTypes().catch(() => { /* swallowed */ });
	});

	async function signOut() {
		await auth.signOut();
	}
</script>

<ModeWatcher />
<TooltipProvider>
<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4" data-testid="app-header">
		<a href="/" class="text-sm font-semibold tracking-tight text-foreground" data-testid="nav-home">Mekhan</a>
		<nav class="ml-8 flex flex-1 items-center gap-1 text-sm" data-testid="nav-bar">
			<Button variant="ghost" size="sm" href="/templates" data-testid="nav-templates">Templates</Button>
			<Button variant="ghost" size="sm" href="/instances" data-testid="nav-instances">Instances</Button>
			<Button variant="ghost" size="sm" href="/tasks" data-testid="nav-tasks">Tasks</Button>
			<Button variant="ghost" size="sm" href="/catalogue" data-testid="nav-catalogue">Catalogue</Button>
			<Button variant="ghost" size="sm" href="/resources" data-testid="nav-resources">Resources</Button>
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
			<div class="ml-auto flex items-center gap-1">
				<ThemeToggle />
				{#if auth.isAuthenticated}
					<span class="h-4 w-px bg-border" aria-hidden="true"></span>
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
			</div>
		</nav>
	</header>
	<main class="flex-1 overflow-hidden">
		{@render children()}
	</main>
</div>
</TooltipProvider>
