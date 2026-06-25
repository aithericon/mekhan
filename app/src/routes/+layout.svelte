<script lang="ts">
	import './layout.css';
	import { onMount } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { TooltipProvider } from '$lib/components/ui/tooltip';
	import { ModeWatcher } from 'mode-watcher';
	import { Toaster } from '$lib/components/ui/sonner';
	import ThemeToggle from '$lib/components/ThemeToggle.svelte';
	import NavMenu, { type NavMenuItem } from '$lib/components/NavMenu.svelte';
	import WorkspacePicker from '$lib/components/WorkspacePicker.svelte';
	import DevIdentityPicker from '$lib/components/DevIdentityPicker.svelte';
	import User from '@lucide/svelte/icons/user';
	import { auth } from '$lib/auth/store.svelte';
	import { ensureAuthInitialized, requireSession } from '$lib/auth/guard';
	import { loadBackends } from '$lib/editor/backend-registry.svelte';
	import { loadNodeTypes } from '$lib/editor/node-registry.svelte';
	import { startPresenceHeartbeat } from '$lib/presence/heartbeat';

	let { children } = $props();

	// Keep the human member's `session` presence alive for the WHOLE time the app
	// is open (not just on the inbox/tasks pages) — see $lib/presence/heartbeat.
	// Re-runs when auth flips; the returned stop fn clears the interval on
	// sign-out / teardown.
	$effect(() => {
		if (!auth.isAuthenticated) return;
		return startPresenceHeartbeat();
	});

	// Data/asset views grouped out of the primary nav.
	const libraryItems: NavMenuItem[] = [
		{ href: '/library', label: 'Node Library', testid: 'nav-node-library', desc: 'Branded, reusable workflow building blocks' },
		{ href: '/data', label: 'Data', testid: 'nav-data', desc: 'Catalogued content, physical copies & file servers' },
		{ href: '/resources', label: 'Resources', testid: 'nav-resources', desc: 'Typed credentials & secrets' },
		{ href: '/assets', label: 'Assets', testid: 'nav-assets', desc: 'Curated record collections' }
	];

	// Low-traffic engine/admin views. Clusters + Capability Types are subsumed
	// by the Control Plane (Fleet): clusters are reached via Scheduler cards →
	// /clusters/[id], capability types from inside the control plane. The
	// /clusters/[id] + /admin/capability-types routes still exist.
	const internalItems = $derived<NavMenuItem[]>([
		{ href: '/nets', label: 'Engine', testid: 'nav-nets', desc: 'Raw petri nets' },
		{ href: '/processes', label: 'Processes', testid: 'nav-processes', desc: 'Raw engine processes' },
		// Platform-admin-only: NATS JetStream debug surface (PETRI_DLQ, drops, lag).
		...(auth.isPlatformAdmin
			? [
					{
						href: '/admin/jetstream',
						label: 'JetStream',
						testid: 'nav-jetstream',
						desc: 'NATS JetStream streams, consumers & DLQ peek'
					}
				]
			: [])
	]);

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

<ModeWatcher defaultMode="dark" />
<!-- Global toast outlet — every `toast.*()` call in the app renders here. -->
<Toaster richColors closeButton />
<TooltipProvider>
<div class="flex h-screen flex-col">
	<header class="flex h-12 shrink-0 items-center border-b border-border bg-card px-4" data-testid="app-header">
		<a
			href="/"
			class="-ml-4 flex self-stretch items-center bg-foreground pl-5 pr-12 text-sm font-semibold uppercase tracking-[0.18em] text-background transition-opacity hover:opacity-90"
			style="clip-path: polygon(0 0, 100% 0, calc(100% - 1.6rem) 100%, 0 100%)"
			data-testid="nav-home">Mekhan</a>
		<nav class="ml-8 flex flex-1 items-center gap-1 text-sm" data-testid="nav-bar">
			<Button variant="ghost" size="sm" href="/templates" data-testid="nav-templates">Templates</Button>
			<Button variant="ghost" size="sm" href="/folders" data-testid="nav-folders">Folders</Button>
			<Button variant="ghost" size="sm" href="/instances" data-testid="nav-instances">Instances</Button>
			<Button variant="ghost" size="sm" href="/tasks" data-testid="nav-tasks">Tasks</Button>
			<Button variant="ghost" size="sm" href="/tasks/inbox" data-testid="nav-inbox">Inbox</Button>
			<NavMenu label="Library" items={libraryItems} testid="nav-library" />
			<Button variant="ghost" size="sm" href="/fleet" data-testid="nav-fleet">Fleet</Button>
			<Button variant="ghost" size="sm" href="/models" data-testid="nav-models">Models</Button>
			<span class="mx-1 h-4 w-px bg-border" aria-hidden="true"></span>
			<NavMenu label="Internals" items={internalItems} testid="nav-internals" muted />
			<div class="ml-auto flex items-center gap-1">
				{#if auth.isAuthenticated}
					<DevIdentityPicker />
					<WorkspacePicker />
					<span class="h-4 w-px bg-border" aria-hidden="true"></span>
				{/if}
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
