/**
 * Pure grant-list math for ShareDialog — kept out of the `.svelte` file so it
 * can be unit-tested without a portal/DOM round trip (mirrors how `Avatar.svelte`
 * exports `initialsFor`/`colorFor`).
 *
 * Encodes the Phase-3 resolver semantics (`service/src/auth/grants.rs`):
 *   effective = max(most-specific grant, workspace_role)
 *   - workspace role is a FLOOR (a grant never drops a member below it),
 *   - most-specific wins: direct object > deeper folder > shallower folder,
 *   - workspace Owner/Admin bypass entirely (their floor already ≥ admin).
 */
import type { GrantView } from '$lib/api/iam';

export const ROLE_RANK: Record<string, number> = { viewer: 0, editor: 1, admin: 2, owner: 3 };
export const ALL_ROLES = ['viewer', 'editor', 'admin', 'owner'] as const;
export type RoleLabel = (typeof ALL_ROLES)[number];

export interface MemberGrant {
	userId: string;
	profile: {
		user_id: string;
		display_name?: string | null;
		email?: string | null;
		avatar_url?: string | null;
	};
	/** Direct object grant — the only editable source. */
	object?: GrantView;
	/** Deepest (most-specific) inherited folder grant. */
	folder?: GrantView;
	/** Workspace-member floor row. */
	workspace?: GrantView;
}

/** Fold the up-to-three source rows per member into one row, sorted with
 *  directly-granted members first, then by name/email. */
export function consolidateGrants(grants: GrantView[]): MemberGrant[] {
	const byUser = new Map<string, MemberGrant>();
	for (const g of grants) {
		let m = byUser.get(g.user_id);
		if (!m) {
			m = {
				userId: g.user_id,
				profile: {
					user_id: g.user_id,
					display_name: g.member_display_name,
					email: g.member_email,
					avatar_url: g.avatar_url
				}
			};
			byUser.set(g.user_id, m);
		}
		if (g.source === 'object') m.object = g;
		else if (g.source === 'folder') {
			const cur = m.folder?.inherited_from_folder_path?.length ?? -1;
			const next = g.inherited_from_folder_path?.length ?? 0;
			if (!m.folder || next > cur) m.folder = g;
		} else if (g.source === 'workspace') m.workspace = g;
	}
	return [...byUser.values()].sort((a, b) => {
		if (!!a.object !== !!b.object) return a.object ? -1 : 1;
		const an = a.profile.display_name ?? a.profile.email ?? a.userId;
		const bn = b.profile.display_name ?? b.profile.email ?? b.userId;
		return an.localeCompare(bn);
	});
}

const rank = (role?: string | null): number => (role ? (ROLE_RANK[role] ?? 0) : 0);

/** Role a member would have with NO direct grant: max(folder grant, workspace). */
export function inheritedRole(m: MemberGrant): RoleLabel {
	const folder = m.folder?.role;
	const ws = (m.workspace?.role ?? 'viewer') as RoleLabel;
	return folder && rank(folder) > rank(ws) ? (folder as RoleLabel) : ws;
}

/** Current effective role: max(most-specific grant, workspace floor). */
export function effectiveRole(m: MemberGrant): RoleLabel {
	const ws = (m.workspace?.role ?? 'viewer') as RoleLabel;
	const specific = m.object?.role ?? m.folder?.role;
	return specific && rank(specific) > rank(ws) ? (specific as RoleLabel) : ws;
}

/** Where the effective role comes from (drives the row's context line). */
export function sourceOf(m: MemberGrant): 'direct' | 'folder' | 'workspace' {
	if (m.object) return 'direct';
	if (m.folder && rank(m.folder.role) > rank(m.workspace?.role ?? 'viewer')) return 'folder';
	return 'workspace';
}

/** A direct grant resolving BELOW the inherited role downgrades the member on
 *  this object (most-specific wins) — surfaced as a warning. */
export function isDowngrade(m: MemberGrant): boolean {
	return !!m.object && rank(effectiveRole(m)) < rank(inheritedRole(m));
}

/** Roles the caller may grant, capped at their own effective role (no-escalation;
 *  the server enforces it too). */
export function grantableRoles(myEffectiveRole?: string | null): RoleLabel[] {
	const cap = rank(myEffectiveRole);
	return ALL_ROLES.filter((r) => ROLE_RANK[r] <= cap);
}
