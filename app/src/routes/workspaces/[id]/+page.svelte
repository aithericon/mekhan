<script lang="ts">
	import { page } from '$app/stores';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import Mail from '@lucide/svelte/icons/mail';
	import FolderKanban from '@lucide/svelte/icons/folder-kanban';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import UserChip from '$lib/components/iam/UserChip.svelte';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription
	} from '$lib/components/ui/card';
	import {
		getWorkspace,
		listWorkspaceMembers,
		addWorkspaceMember,
		removeWorkspaceMember,
		resolveUserByEmail,
		type WorkspaceSummary,
		type WorkspaceMember
	} from '$lib/api/client';
	import {
		listInvites,
		createInvite,
		resendInvite,
		revokeInvite,
		type InviteSummary
	} from '$lib/api/invites';
	import { updateMemberRole } from '$lib/api/iam';
	import { auth } from '$lib/auth/store.svelte';

	const workspaceId = $derived($page.params.id ?? '');

	// Workspace Owner/Admin gates every member/invite mutation. The server is
	// authoritative; this just hides affordances the caller can't use.
	const canAdmin = $derived(auth.isWorkspaceAdmin);

	let workspace = $state<WorkspaceSummary | null>(null);
	let members = $state<WorkspaceMember[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Add-member form state
	let newMemberEmail = $state('');
	let newMemberRole = $state<'viewer' | 'editor' | 'admin' | 'owner'>('editor');
	let addingMember = $state(false);
	let addError = $state<string | null>(null);

	// Invite form + pending-invite list
	let invites = $state<InviteSummary[]>([]);
	let inviteEmail = $state('');
	let inviteRole = $state<'viewer' | 'editor' | 'admin' | 'owner'>('editor');
	let inviting = $state(false);
	let inviteError = $state<string | null>(null);

	async function load() {
		loading = true;
		error = null;
		try {
			[workspace, members] = await Promise.all([
				getWorkspace(workspaceId),
				listWorkspaceMembers(workspaceId)
			]);
			// Invites are Admin-only; tolerate a 403 for non-admins.
			invites = await listInvites(workspaceId).catch(() => []);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load workspace';
		} finally {
			loading = false;
		}
	}

	async function sendInvite(e: Event) {
		e.preventDefault();
		const email = inviteEmail.trim();
		if (!email) return;
		inviting = true;
		inviteError = null;
		try {
			await createInvite(workspaceId, { email, role: inviteRole });
			inviteEmail = '';
			invites = await listInvites(workspaceId);
		} catch (e) {
			inviteError = e instanceof Error ? e.message : 'Failed to send invite';
		} finally {
			inviting = false;
		}
	}

	async function resend(inviteId: string) {
		try {
			await resendInvite(workspaceId, inviteId);
			invites = await listInvites(workspaceId);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to resend invite');
		}
	}

	async function revoke(inviteId: string) {
		if (!confirm('Revoke this invite?')) return;
		try {
			await revokeInvite(workspaceId, inviteId);
			invites = invites.filter((i) => i.id !== inviteId);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to revoke invite');
		}
	}

	$effect(() => {
		if (workspaceId) load();
	});

	async function addMember(e: Event) {
		e.preventDefault();
		const email = newMemberEmail.trim();
		if (!email) return;
		addingMember = true;
		addError = null;
		try {
			const resolved = await resolveUserByEmail(email);
			await addWorkspaceMember(workspaceId, {
				subject: resolved.subject,
				role: newMemberRole
			});
			newMemberEmail = '';
			members = await listWorkspaceMembers(workspaceId);
		} catch (e) {
			addError = e instanceof Error ? e.message : 'Failed to add member';
		} finally {
			addingMember = false;
		}
	}

	async function removeMember(userId: string) {
		if (!confirm('Remove this member?')) return;
		try {
			await removeWorkspaceMember(workspaceId, userId);
			members = members.filter((m) => m.user_id !== userId);
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to remove member');
		}
	}

	// A workspace can never be left without an owner: the sole remaining owner's
	// role select + remove are disabled (the server enforces this too — we
	// handle a 409 from a concurrent race gracefully below).
	const ownerCount = $derived(members.filter((m) => m.role === 'owner').length);
	const isLastOwner = (m: WorkspaceMember) => m.role === 'owner' && ownerCount <= 1;

	let roleBusy = $state<string | null>(null);
	async function changeRole(m: WorkspaceMember, role: string) {
		if (role === m.role || roleBusy) return;
		roleBusy = m.user_id;
		try {
			const updated = await updateMemberRole(workspaceId, m.user_id, role);
			members = members.map((x) => (x.user_id === m.user_id ? { ...x, role: updated.role } : x));
		} catch (e) {
			const msg = e instanceof Error ? e.message : 'Failed to change role';
			alert(msg.includes('409') ? 'A workspace must keep at least one owner.' : msg);
		} finally {
			roleBusy = null;
		}
	}

</script>

<PageShell testid="workspace-detail">
	{#snippet band()}
		<PageHeader
			title={workspace?.display_name ?? 'Workspace'}
			subtitle={workspace
				? `${workspace.slug} · ${members.length} ${members.length === 1 ? 'member' : 'members'}`
				: undefined}
		>
			{#snippet actions()}
				{#if workspace?.is_system}
					<Badge variant="secondary">system</Badge>
				{/if}
			{/snippet}
		</PageHeader>
	{/snippet}
	{#if loading}
		<div class="text-sm text-muted-foreground">Loading workspace…</div>
	{:else if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{:else if workspace}
		<div class="grid gap-6 md:grid-cols-2">
			<!-- Members -->
			<Card data-testid="members-card">
				<CardHeader>
					<CardTitle>Members</CardTitle>
					<CardDescription>
						Owners and admins can add/remove members. Workspace can never be
						left without an owner.
					</CardDescription>
				</CardHeader>
				<CardContent class="space-y-4">
					{#if canAdmin}
						<form onsubmit={addMember} class="space-y-2">
							<div class="flex gap-2">
								<Input
									type="email"
									placeholder="email@corp.com"
									bind:value={newMemberEmail}
									data-testid="input-new-member-email"
									class="flex-1"
								/>
								<select
									bind:value={newMemberRole}
									class="rounded-md border border-input bg-background px-2 text-sm"
									data-testid="select-new-member-role"
								>
									<option value="viewer">Viewer</option>
									<option value="editor">Editor</option>
									<option value="admin">Admin</option>
									<option value="owner">Owner</option>
								</select>
								<Button type="submit" disabled={addingMember} data-testid="btn-add-member">
									<UserPlus class="size-4" />
									Add
								</Button>
							</div>
							{#if addError}
								<div class="text-xs text-destructive">{addError}</div>
							{/if}
						</form>
					{/if}

					<ul class="divide-y divide-border rounded-md border border-border">
						{#each members as m (m.user_id)}
							{@const lastOwner = isLastOwner(m)}
							<li
								class="flex items-center justify-between gap-3 px-3 py-2 text-sm"
								data-testid={`member-row-${m.user_id}`}
							>
								<div class="min-w-0 flex-1">
									<UserChip
										userId={m.user_id}
										profile={{
											user_id: m.user_id,
											display_name: m.display_name,
											email: m.email,
											avatar_url: m.avatar_url
										}}
										showEmail
									/>
								</div>
								{#if canAdmin}
									<select
										value={m.role}
										disabled={roleBusy !== null || lastOwner}
										title={lastOwner ? 'A workspace must keep at least one owner' : undefined}
										onchange={(e) => changeRole(m, (e.currentTarget as HTMLSelectElement).value)}
										class="rounded-md border border-input bg-background px-2 py-1 text-sm disabled:opacity-60"
										data-testid={`member-role-${m.user_id}`}
										aria-label="Member role"
									>
										<option value="viewer">viewer</option>
										<option value="editor">editor</option>
										<option value="admin">admin</option>
										<option value="owner">owner</option>
									</select>
									<button
										type="button"
										class="text-muted-foreground hover:text-destructive disabled:opacity-40"
										onclick={() => removeMember(m.user_id)}
										disabled={lastOwner}
										title={lastOwner ? 'A workspace must keep at least one owner' : 'Remove member'}
										data-testid={`btn-remove-member-${m.user_id}`}
										aria-label="Remove member"
									>
										<Trash2 class="size-3.5" />
									</button>
								{:else}
									<Badge variant="secondary">{m.role}</Badge>
								{/if}
							</li>
						{/each}
					</ul>
				</CardContent>
			</Card>

			<!-- Pending invites -->
			<Card data-testid="invites-card">
				<CardHeader>
					<CardTitle>Invites</CardTitle>
					<CardDescription>
						Invite someone by email. They get an accept link; on accept they're
						added at the chosen role (created in the IdP if new).
					</CardDescription>
				</CardHeader>
				<CardContent class="space-y-4">
					{#if canAdmin}
					<form onsubmit={sendInvite} class="space-y-2">
						<div class="flex gap-2">
							<Input
								type="email"
								placeholder="invitee@corp.com"
								bind:value={inviteEmail}
								data-testid="input-invite-email"
								class="flex-1"
							/>
							<select
								bind:value={inviteRole}
								class="rounded-md border border-input bg-background px-2 text-sm"
								data-testid="select-invite-role"
							>
								<option value="viewer">Viewer</option>
								<option value="editor">Editor</option>
								<option value="admin">Admin</option>
								<option value="owner">Owner</option>
							</select>
							<Button type="submit" disabled={inviting} data-testid="btn-send-invite">
								<Mail class="size-4" />
								Invite
							</Button>
						</div>
						{#if inviteError}
							<div class="text-xs text-destructive">{inviteError}</div>
						{/if}
					</form>
					{/if}

					{#if invites.length === 0}
						<p class="text-sm text-muted-foreground">No invites yet.</p>
					{:else}
						<ul class="divide-y divide-border rounded-md border border-border">
							{#each invites as inv (inv.id)}
								<li
									class="flex items-center justify-between gap-3 px-3 py-2 text-sm"
									data-testid={`invite-row-${inv.id}`}
								>
									<div class="min-w-0 flex-1 truncate">{inv.email}</div>
									<Badge variant="secondary">{inv.role}</Badge>
									<Badge
										variant={inv.status === 'pending' ? 'outline' : 'secondary'}
										data-testid={`invite-status-${inv.id}`}>{inv.status}</Badge
									>
									{#if inv.status === 'pending' && canAdmin}
										<button
											type="button"
											class="text-xs text-muted-foreground hover:text-foreground"
											onclick={() => resend(inv.id)}
											data-testid={`btn-resend-${inv.id}`}>Resend</button
										>
										<button
											type="button"
											class="text-xs text-muted-foreground hover:text-destructive"
											onclick={() => revoke(inv.id)}
											data-testid={`btn-revoke-${inv.id}`}>Revoke</button
										>
									{/if}
								</li>
							{/each}
						</ul>
					{/if}
				</CardContent>
			</Card>

			<!-- Folders (managed top-level, scoped to the active workspace) -->
			<Card data-testid="folders-card">
				<CardHeader>
					<CardTitle>Folders</CardTitle>
					<CardDescription>
						Organize templates into a hierarchy. Each folder gets its own
						per-webhook OpenAPI bundle.
					</CardDescription>
				</CardHeader>
				<CardContent>
					<a
						href="/folders"
						class="flex items-center gap-3 rounded-md border border-border bg-card/50 p-3 text-sm hover:bg-accent/50"
						data-testid="link-folders"
					>
						<FolderKanban class="size-5 text-muted-foreground" />
						<div class="min-w-0 flex-1">
							<div class="font-medium">Manage folders</div>
							<div class="text-sm text-muted-foreground">
								Create, organize templates, and view API contracts
							</div>
						</div>
						<ArrowRight class="size-4 text-muted-foreground" />
					</a>
				</CardContent>
			</Card>
		</div>
	{/if}
</PageShell>

