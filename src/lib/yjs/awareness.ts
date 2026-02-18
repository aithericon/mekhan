import type { Awareness } from 'y-protocols/awareness';

export type UserPresence = {
	userId: string;
	name: string;
	color: string;
	selectedNodeId?: string;
	cursor?: { nodeId: string; filename: string };
};

export const COLORS = [
	'#ef4444', // red
	'#3b82f6', // blue
	'#22c55e', // green
	'#f59e0b', // amber
	'#8b5cf6', // violet
	'#ec4899', // pink
	'#06b6d4', // cyan
	'#f97316' // orange
];

export function setLocalPresence(
	awareness: Awareness,
	presence: Partial<UserPresence>
): void {
	const current = awareness.getLocalState() ?? {};
	awareness.setLocalState({ ...current, ...presence });
}

export function getRemoteUsers(awareness: Awareness): UserPresence[] {
	const users: UserPresence[] = [];
	const localId = awareness.clientID;

	awareness.getStates().forEach((state, clientId) => {
		if (clientId === localId) return;
		if (state.userId || state.name) {
			users.push(state as UserPresence);
		}
	});

	return users;
}

export function onRemoteChange(
	awareness: Awareness,
	callback: (users: UserPresence[]) => void
): () => void {
	const handler = () => {
		callback(getRemoteUsers(awareness));
	};

	awareness.on('change', handler);
	return () => awareness.off('change', handler);
}
