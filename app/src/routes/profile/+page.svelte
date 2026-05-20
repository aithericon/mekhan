<script lang="ts">
	import { auth } from '$lib/auth/store.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardDescription,
		CardContent
	} from '$lib/components/ui/card';
	import { Separator } from '$lib/components/ui/separator';
	import AccessTokens from '$lib/components/profile/AccessTokens.svelte';
	import User from '@lucide/svelte/icons/user';
	import LogOut from '@lucide/svelte/icons/log-out';
	import LogIn from '@lucide/svelte/icons/log-in';

	let signingOut = $state(false);

	// Reactive view of the resolved principal (null when no valid session —
	// the layout guard normally prevents this, but dev edge cases and a
	// just-expired cookie can land here, so render a sign-in fallback).
	const user = $derived(auth.session?.user ?? null);

	const initials = $derived(
		(user?.displayName ?? user?.email ?? '?')
			.trim()
			.split(/\s+/)
			.slice(0, 2)
			.map((p) => p[0]?.toUpperCase() ?? '')
			.join('') || '?'
	);

	async function signOut() {
		if (signingOut) return;
		signingOut = true;
		try {
			await auth.signOut();
		} finally {
			signingOut = false;
		}
	}
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-2xl px-6 py-8 animate-rise" data-testid="profile-page">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Profile</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Your signed-in identity, as resolved by the server from the session cookie.
			</p>
		</div>

		{#if user}
			<Card>
				<CardHeader>
					<div class="flex items-center gap-4">
						<div
							class="flex size-12 shrink-0 items-center justify-center rounded-full bg-primary/10 text-base font-semibold text-primary"
							aria-hidden="true"
						>
							{initials}
						</div>
						<div class="min-w-0">
							<CardTitle class="truncate" data-testid="profile-name">
								{user.displayName ?? user.email ?? 'Signed-in user'}
							</CardTitle>
							{#if user.email}
								<CardDescription class="truncate" data-testid="profile-email">
									{user.email}
								</CardDescription>
							{/if}
						</div>
					</div>
				</CardHeader>

				<CardContent class="space-y-5">
					<dl class="grid gap-4 sm:grid-cols-2">
						<div class="space-y-1">
							<dt class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
								Display name
							</dt>
							<dd class="text-sm text-foreground">{user.displayName ?? '—'}</dd>
						</div>
						<div class="space-y-1">
							<dt class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
								Email
							</dt>
							<dd class="text-sm text-foreground">{user.email ?? '—'}</dd>
						</div>
						<div class="space-y-1">
							<dt class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
								Organization
							</dt>
							<dd class="text-sm text-foreground" data-testid="profile-org">
								{user.orgId ?? '—'}
							</dd>
						</div>
						<div class="space-y-1 sm:col-span-2">
							<dt class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
								Subject (OIDC <code>sub</code>)
							</dt>
							<dd
								class="break-all rounded-md bg-muted px-2 py-1 font-mono text-sm text-foreground"
								data-testid="profile-subject"
							>
								{user.subject}
							</dd>
						</div>
					</dl>

					<Separator />

					<div class="space-y-2">
						<div class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
							Roles
						</div>
						{#if user.roles.length > 0}
							<div class="flex flex-wrap gap-1.5" data-testid="profile-roles">
								{#each user.roles as role (role)}
									<Badge variant="secondary">{role}</Badge>
								{/each}
							</div>
						{:else}
							<p class="text-sm text-muted-foreground" data-testid="profile-roles-empty">
								No roles assigned.
							</p>
						{/if}
					</div>
				</CardContent>
			</Card>

			<div class="mt-6 flex items-center justify-between gap-3">
				<p class="text-sm text-muted-foreground">
					Tokens never reach the browser — the session is held server-side (BFF).
				</p>
				<Button
					variant="outline"
					onclick={signOut}
					disabled={signingOut}
					data-testid="profile-signout"
				>
					<LogOut class="size-4" />
					{signingOut ? 'Signing out…' : 'Sign out'}
				</Button>
			</div>

			<AccessTokens />
		{:else}
			<Card>
				<CardContent class="flex flex-col items-center gap-4 py-12 text-center">
					<div
						class="flex size-12 items-center justify-center rounded-full bg-muted text-muted-foreground"
						aria-hidden="true"
					>
						<User class="size-6" />
					</div>
					<div>
						<p class="text-sm font-medium text-foreground">You're not signed in</p>
						<p class="mt-1 text-sm text-muted-foreground">
							Sign in to view your profile.
						</p>
					</div>
					<Button onclick={() => auth.signIn('/profile')} data-testid="profile-signin">
						<LogIn class="size-4" />
						Sign in
					</Button>
				</CardContent>
			</Card>
		{/if}
	</div>
</div>
