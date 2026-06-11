import { describe, it, expect } from 'vitest';
import {
	consolidateGrants,
	inheritedRole,
	effectiveRole,
	sourceOf,
	isDowngrade,
	grantableRoles,
	type MemberGrant
} from './share-grants';
import type { GrantView } from '$lib/api/iam';

// Minimal GrantView factory — only the fields the math reads.
function gv(p: Partial<GrantView> & Pick<GrantView, 'user_id' | 'role' | 'source'>): GrantView {
	return { ...p } as GrantView;
}

describe('consolidateGrants', () => {
	it('folds the three source rows of one member into a single row', () => {
		const rows = consolidateGrants([
			gv({ user_id: 'u1', role: 'viewer', source: 'workspace', member_display_name: 'Alice' }),
			gv({
				user_id: 'u1',
				role: 'editor',
				source: 'folder',
				inherited_from_folder_path: '/research'
			}),
			gv({ user_id: 'u1', role: 'admin', source: 'object' })
		]);
		expect(rows).toHaveLength(1);
		expect(rows[0].workspace?.role).toBe('viewer');
		expect(rows[0].folder?.role).toBe('editor');
		expect(rows[0].object?.role).toBe('admin');
		expect(rows[0].profile.display_name).toBe('Alice');
	});

	it('keeps the deepest (most-specific) folder grant', () => {
		const rows = consolidateGrants([
			gv({ user_id: 'u1', role: 'admin', source: 'folder', inherited_from_folder_path: '/r' }),
			gv({
				user_id: 'u1',
				role: 'viewer',
				source: 'folder',
				inherited_from_folder_path: '/r/q3/deep'
			})
		]);
		expect(rows[0].folder?.role).toBe('viewer'); // deeper path wins
	});

	it('sorts directly-granted members first', () => {
		const rows = consolidateGrants([
			gv({ user_id: 'b', role: 'viewer', source: 'workspace', member_display_name: 'Bob' }),
			gv({ user_id: 'a', role: 'admin', source: 'object', member_display_name: 'Zed' })
		]);
		expect(rows[0].userId).toBe('a'); // has a direct grant
	});
});

const member = (parts: Partial<MemberGrant>): MemberGrant => ({
	userId: 'u',
	profile: { user_id: 'u' },
	...parts
});

describe('effectiveRole / inheritedRole — floor + most-specific', () => {
	it('workspace role is a floor a grant cannot drop below', () => {
		const m = member({
			workspace: gv({ user_id: 'u', role: 'editor', source: 'workspace' }),
			object: gv({ user_id: 'u', role: 'viewer', source: 'object' })
		});
		// most-specific is viewer, but the ws floor (editor) wins.
		expect(effectiveRole(m)).toBe('editor');
	});

	it('a direct grant raises a member above their workspace floor', () => {
		const m = member({
			workspace: gv({ user_id: 'u', role: 'viewer', source: 'workspace' }),
			object: gv({ user_id: 'u', role: 'admin', source: 'object' })
		});
		expect(effectiveRole(m)).toBe('admin');
		expect(inheritedRole(m)).toBe('viewer');
	});

	it('inherited folder grant applies with no direct grant', () => {
		const m = member({
			workspace: gv({ user_id: 'u', role: 'viewer', source: 'workspace' }),
			folder: gv({ user_id: 'u', role: 'editor', source: 'folder', inherited_from_folder_path: '/r' })
		});
		expect(effectiveRole(m)).toBe('editor');
		expect(sourceOf(m)).toBe('folder');
	});

	it('bare workspace member has the floor as effective', () => {
		const m = member({ workspace: gv({ user_id: 'u', role: 'viewer', source: 'workspace' }) });
		expect(effectiveRole(m)).toBe('viewer');
		expect(sourceOf(m)).toBe('workspace');
	});
});

describe('isDowngrade', () => {
	it('flags a direct grant set below a higher inherited folder role', () => {
		const m = member({
			workspace: gv({ user_id: 'u', role: 'viewer', source: 'workspace' }),
			folder: gv({ user_id: 'u', role: 'admin', source: 'folder', inherited_from_folder_path: '/r' }),
			object: gv({ user_id: 'u', role: 'editor', source: 'object' })
		});
		// inherited would be admin; direct (most-specific) → editor → downgrade.
		expect(inheritedRole(m)).toBe('admin');
		expect(effectiveRole(m)).toBe('editor');
		expect(isDowngrade(m)).toBe(true);
	});

	it('does not flag an elevating direct grant', () => {
		const m = member({
			workspace: gv({ user_id: 'u', role: 'viewer', source: 'workspace' }),
			object: gv({ user_id: 'u', role: 'admin', source: 'object' })
		});
		expect(isDowngrade(m)).toBe(false);
	});

	it('does not flag when the floor absorbs a below-floor grant', () => {
		const m = member({
			workspace: gv({ user_id: 'u', role: 'editor', source: 'workspace' }),
			object: gv({ user_id: 'u', role: 'viewer', source: 'object' })
		});
		// effective stays editor (floor) === inherited editor → no downgrade.
		expect(isDowngrade(m)).toBe(false);
	});
});

describe('grantableRoles — no-escalation cap', () => {
	it('an admin can grant up to admin, not owner', () => {
		expect(grantableRoles('admin')).toEqual(['viewer', 'editor', 'admin']);
	});
	it('an owner can grant any role', () => {
		expect(grantableRoles('owner')).toEqual(['viewer', 'editor', 'admin', 'owner']);
	});
	it('a viewer/null can grant nothing meaningful', () => {
		expect(grantableRoles('viewer')).toEqual(['viewer']);
		expect(grantableRoles(null)).toEqual(['viewer']); // rank 0 cap
	});
});
