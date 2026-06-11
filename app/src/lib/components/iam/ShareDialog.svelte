<!--
  ShareDialog — ONE component parameterized by `objectType ∈ {folder,template,
  instance}`. Reads the object's FULL effective access list (direct grants +
  inherited folder grants + workspace-member floor) and lets an object-Admin
  raise or override a member's role on this object.

  Model (matches the Phase-3 resolver, `service/src/auth/grants.rs`):
    effective = max(most-specific grant, workspace_role)
    - workspace role is a FLOOR — a grant can never drop a member below it.
    - the most-specific grant wins (direct object > deeper folder > shallower).
    - workspace Owner/Admin BYPASS object ACLs entirely.

  Grants are MEMBERS-ONLY: the grantee must already be a workspace member, and
  the list already carries a row for every member (the `workspace` floor row),
  so the member list IS the picker — there is no add-by-email here. Admitting a
  NEW person is the workspace invite flow (`/invite/accept`), not a grant.

  When a more-specific direct grant is set LOWER than what the member inherits
  from a parent folder, we surface a downgrade note: the most-specific grant
  wins, so this LOWERS their access on this object rather than adding to it
  (workspace Owner/Admin remain bypass-safe regardless).

  Mutations re-read the list and fire `onChanged` so a parent that gates on
  `my_effective_role` (e.g. the Yjs editor surface) can re-fetch and drop stale
  edit affordances after the caller changes their own grant.
-->
<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import * as Dialog from '$lib/components/ui/dialog';
	import UserChip from './UserChip.svelte';
	import LoaderCircle from '@lucide/svelte/icons/loader-circle';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import { listGrants, putGrant, deleteGrant, roleAtLeast, type GrantView, type ObjectType } from '$lib/api/iam';
	import {
		consolidateGrants,
		inheritedRole,
		effectiveRole,
		sourceOf,
		isDowngrade,
		grantableRoles as grantableRolesFor,
		ALL_ROLES,
		type MemberGrant
	} from './share-grants';

	let {
		open = $bindable(),
		objectType,
		objectId,
		objectName,
		myEffectiveRole,
		onChanged
	}: {
		open: boolean;
		objectType: ObjectType;
		objectId: string;
		objectName?: string;
		/** The caller's effective role on this object — caps what they can grant
		 *  (no-escalation) and disables controls below `admin`. */
		myEffectiveRole?: string | null;
		onChanged?: () => void;
	} = $props();

	let grants = $state<GrantView[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let busyUser = $state<string | null>(null);

	// Object-Admins manage; the no-escalation cap is the caller's own effective
	// role (the server enforces it too). Roles strictly above the caller are
	// hidden from the picker.
	const canManage = $derived(roleAtLeast(myEffectiveRole, 'admin'));
	const grantableRoles = $derived(grantableRolesFor(myEffectiveRole));
	const rows = $derived(consolidateGrants(grants));

	async function load() {
		loading = true;
		error = null;
		try {
			grants = await listGrants(objectType, objectId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load access list';
			grants = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open && objectId) load();
	});

	async function applyRole(m: MemberGrant, role: string) {
		if (busyUser || role === effectiveRole(m)) return;
		busyUser = m.userId;
		error = null;
		try {
			// Setting a member's direct grant equal to what they already inherit is
			// just clutter — clear the redundant direct grant instead (reset).
			if (m.object && role === inheritedRole(m)) {
				await deleteGrant(objectType, objectId, m.userId);
			} else {
				await putGrant(objectType, objectId, m.userId, role);
			}
			await load();
			onChanged?.();
		} catch (e) {
			error = friendly(e);
		} finally {
			busyUser = null;
		}
	}

	async function resetToInherited(m: MemberGrant) {
		if (busyUser || !m.object) return;
		busyUser = m.userId;
		error = null;
		try {
			await deleteGrant(objectType, objectId, m.userId);
			await load();
			onChanged?.();
		} catch (e) {
			error = friendly(e);
		} finally {
			busyUser = null;
		}
	}

	function friendly(e: unknown): string {
		const msg = e instanceof Error ? e.message : String(e);
		if (msg.includes('409')) return 'That person must be a workspace member first — invite them instead.';
		if (msg.includes('403')) return "You can't grant a role higher than your own on this object.";
		return msg;
	}

	const kindLabel = $derived(
		objectType === 'folder' ? 'folder' : objectType === 'template' ? 'template' : 'run'
	);
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="max-w-lg">
		<Dialog.Header>
			<Dialog.Title>Share {objectName ? `“${objectName}”` : `this ${kindLabel}`}</Dialog.Title>
			<Dialog.Description>
				Workspace members and their effective role on this {kindLabel}. The
				most-specific grant wins; the workspace role is a floor. Add new people
				from the workspace Invites panel.
			</Dialog.Description>
		</Dialog.Header>

		{#if error}
			<div
				class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
				data-testid="share-error"
			>
				{error}
			</div>
		{/if}

		{#if loading}
			<div class="flex items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
				<LoaderCircle class="size-4 animate-spin" /> Loading access…
			</div>
		{:else}
			<ul class="max-h-[55vh] divide-y divide-border overflow-y-auto rounded-md border border-border" data-testid="share-grant-list">
				{#each rows as m (m.userId)}
					{@const eff = effectiveRole(m)}
					{@const src = sourceOf(m)}
					<li class="flex flex-col gap-1 px-3 py-2 text-sm" data-testid={`share-row-${m.userId}`}>
						<div class="flex items-center justify-between gap-3">
							<div class="min-w-0 flex-1">
								<UserChip userId={m.userId} profile={m.profile} showEmail />
							</div>

							{#if busyUser === m.userId}
								<LoaderCircle class="size-4 animate-spin text-muted-foreground" />
							{/if}

							{#if canManage}
								<select
									value={eff}
									disabled={busyUser !== null}
									onchange={(e) => applyRole(m, (e.currentTarget as HTMLSelectElement).value)}
									class="rounded-md border border-input bg-background px-2 py-1 text-sm"
									data-testid={`share-role-${m.userId}`}
									aria-label="Role on this object"
								>
									{#each grantableRoles as r (r)}
										<option value={r}>{r}</option>
									{/each}
									<!-- The current effective role may exceed what the caller can
									     grant (e.g. an owner row seen by an admin) — keep it
									     selectable-as-display so the select isn't blank. -->
									{#if !grantableRoles.includes(eff as (typeof ALL_ROLES)[number])}
										<option value={eff}>{eff}</option>
									{/if}
								</select>
								{#if m.object}
									<button
										type="button"
										class="text-xs text-muted-foreground underline hover:text-foreground"
										onclick={() => resetToInherited(m)}
										disabled={busyUser !== null}
										data-testid={`share-reset-${m.userId}`}
									>
										Reset
									</button>
								{/if}
							{:else}
								<Badge variant="secondary">{eff}</Badge>
							{/if}
						</div>

						<!-- Source / inheritance context -->
						<div class="flex items-center gap-2 pl-7 text-xs text-muted-foreground">
							{#if src === 'direct'}
								<span data-testid={`share-source-${m.userId}`}>Granted on this {kindLabel}</span>
							{:else if src === 'folder'}
								<span data-testid={`share-source-${m.userId}`}>
									Inherited from folder
									<span class="font-mono">{m.folder?.inherited_from_folder_path}</span>
								</span>
							{:else}
								<span data-testid={`share-source-${m.userId}`}>Workspace {m.workspace?.role ?? 'member'}</span>
							{/if}
						</div>

						{#if isDowngrade(m)}
							<div
								class="ml-7 flex items-start gap-1.5 rounded bg-amber-50 px-2 py-1 text-xs text-amber-800"
								data-testid={`share-downgrade-${m.userId}`}
							>
								<TriangleAlert class="mt-0.5 size-3.5 shrink-0" />
								<span>
									Lower than the <strong>{inheritedRole(m)}</strong> role inherited from
									a parent folder. The most-specific grant wins, so this lowers their
									access here. Reset to restore the inherited role.
								</span>
							</div>
						{/if}
					</li>
				{:else}
					<li class="px-3 py-6 text-center text-sm text-muted-foreground">No members.</li>
				{/each}
			</ul>

			{#if !canManage}
				<p class="text-xs text-muted-foreground">
					You need the <strong>admin</strong> role on this {kindLabel} to change access.
				</p>
			{/if}
		{/if}

		<Dialog.Footer>
			<Button variant="outline" onclick={() => (open = false)} data-testid="share-close">Done</Button>
		</Dialog.Footer>
	</Dialog.Content>
</Dialog.Root>
