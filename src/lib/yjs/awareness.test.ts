import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as Y from 'yjs';
import { Awareness } from 'y-protocols/awareness';
import { setLocalPresence, getRemoteUsers, onRemoteChange, COLORS } from './awareness';

describe('awareness', () => {
	let doc: Y.Doc;
	let awareness: Awareness;

	beforeEach(() => {
		doc = new Y.Doc();
		awareness = new Awareness(doc);
	});

	it('setLocalPresence sets state', () => {
		setLocalPresence(awareness, { userId: 'u1', name: 'Alice', color: '#ef4444' });

		const state = awareness.getLocalState();
		expect(state).toBeDefined();
		expect(state!.userId).toBe('u1');
		expect(state!.name).toBe('Alice');
		expect(state!.color).toBe('#ef4444');
	});

	it('setLocalPresence merges', () => {
		setLocalPresence(awareness, { userId: 'u1' });
		setLocalPresence(awareness, { name: 'Alice' });

		const state = awareness.getLocalState();
		expect(state!.userId).toBe('u1');
		expect(state!.name).toBe('Alice');
	});

	it('getRemoteUsers excludes local', () => {
		// Set local presence
		setLocalPresence(awareness, { userId: 'local', name: 'Me', color: '#000' });

		// Simulate a remote client by creating another doc + awareness
		const doc2 = new Y.Doc();
		const awareness2 = new Awareness(doc2);
		setLocalPresence(awareness2, { userId: 'remote', name: 'Bob', color: '#fff' });

		// Manually inject remote state into awareness1
		// Awareness stores state by clientID — we simulate a remote client
		const remoteClientId = awareness2.clientID;
		const remoteState = awareness2.getLocalState();
		awareness.setLocalStateField('__test', true); // ensure local state exists

		// Directly manipulate the states map to simulate remote state
		awareness.getStates().set(remoteClientId, remoteState!);

		const users = getRemoteUsers(awareness);
		expect(users.length).toBe(1);
		expect(users[0].userId).toBe('remote');
		expect(users[0].name).toBe('Bob');

		doc2.destroy();
	});

	it('getRemoteUsers empty for solo client', () => {
		setLocalPresence(awareness, { userId: 'solo', name: 'Only Me', color: '#000' });

		const users = getRemoteUsers(awareness);
		expect(users).toEqual([]);
	});

	it('onRemoteChange fires callback', () => {
		const callback = vi.fn();
		onRemoteChange(awareness, callback);

		// Trigger a change event by updating local state
		// (awareness fires 'change' on any state mutation)
		setLocalPresence(awareness, { userId: 'u1', name: 'Alice', color: '#000' });

		expect(callback).toHaveBeenCalled();
	});

	it('unsubscribe stops callback', () => {
		const callback = vi.fn();
		const unsubscribe = onRemoteChange(awareness, callback);

		setLocalPresence(awareness, { userId: 'u1', name: 'First', color: '#000' });
		const callCountAfterFirst = callback.mock.calls.length;

		unsubscribe();

		setLocalPresence(awareness, { name: 'Second' });
		expect(callback).toHaveBeenCalledTimes(callCountAfterFirst);
	});

	it('COLORS has 8 hex entries', () => {
		expect(COLORS).toHaveLength(8);
		for (const color of COLORS) {
			expect(color).toMatch(/^#[0-9a-f]{6}$/);
		}
	});
});
