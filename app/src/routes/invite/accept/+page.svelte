<script lang="ts">
	import { page } from '$app/stores';
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription,
		CardFooter
	} from '$lib/components/ui/card';
	import { previewInvite, acceptInvite, type InvitePreview } from '$lib/api/invites';

	const token = $derived($page.url.searchParams.get('token') ?? '');

	let preview = $state<InvitePreview | null>(null);
	let loading = $state(true);
	let invalid = $state(false);
	let accepting = $state(false);
	let acceptError = $state<string | null>(null);
	let accepted = $state(false);

	async function load() {
		loading = true;
		invalid = false;
		preview = null;
		if (!token) {
			invalid = true;
			loading = false;
			return;
		}
		try {
			preview = await previewInvite(token);
		} catch {
			// Generic: unknown / expired / revoked / accepted all look the same.
			invalid = true;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (token !== undefined) load();
	});

	/** Send the invitee to login, returning to this accept page afterwards. */
	function signInAndReturn() {
		const here = window.location.pathname + window.location.search;
		window.location.assign(`/api/auth/login?return_to=${encodeURIComponent(here)}`);
	}

	async function accept() {
		accepting = true;
		acceptError = null;
		try {
			// Accept is authed: the logged-in session IS the joining identity. If
			// there's no session, bounce through login and come back here.
			const probe = await fetch('/api/auth/session', {
				headers: { Accept: 'application/json' },
				credentials: 'same-origin'
			});
			if (!probe.ok) {
				signInAndReturn();
				return;
			}
			await acceptInvite(token);
			accepted = true;
			// Joined — land in the app on the active workspace.
			goto('/');
		} catch (e) {
			acceptError = e instanceof Error ? e.message : 'Failed to accept invite';
		} finally {
			accepting = false;
		}
	}
</script>

<div class="mx-auto flex min-h-[60vh] max-w-md items-center px-4">
	<Card class="w-full" data-testid="invite-accept">
		<CardHeader>
			<CardTitle>You've been invited</CardTitle>
			<CardDescription>
				{#if loading}Checking your invite…{:else if invalid}This invite link is no longer valid.{:else if preview}Join
					<strong>{preview.workspace_display_name}</strong>.{/if}
			</CardDescription>
		</CardHeader>
		<CardContent class="space-y-3">
			{#if loading}
				<p class="text-sm text-muted-foreground">Loading…</p>
			{:else if invalid}
				<p class="text-sm text-muted-foreground" data-testid="invite-invalid">
					The invite may have expired, been revoked, or already been used. Ask whoever invited you
					to send a fresh link.
				</p>
			{:else if preview}
				<div class="flex items-center justify-between text-sm">
					<span class="text-muted-foreground">Email</span>
					<span class="font-medium">{preview.email}</span>
				</div>
				<div class="flex items-center justify-between text-sm">
					<span class="text-muted-foreground">Role</span>
					<Badge variant="secondary">{preview.role}</Badge>
				</div>
				{#if acceptError}
					<p class="text-sm text-destructive" data-testid="invite-error">{acceptError}</p>
				{/if}
			{/if}
		</CardContent>
		{#if !loading && !invalid && preview}
			<CardFooter>
				<Button class="w-full" onclick={accept} disabled={accepting || accepted} data-testid="invite-accept-btn">
					{accepting ? 'Accepting…' : accepted ? 'Accepted' : 'Accept invite'}
				</Button>
			</CardFooter>
		{/if}
	</Card>
</div>
