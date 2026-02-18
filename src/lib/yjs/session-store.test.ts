import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as Y from 'yjs';

const mockDestroy = vi.fn();
const mockCreateYjsSession = vi.fn((_templateId: string) => ({
	doc: new Y.Doc(),
	provider: { on: vi.fn(), off: vi.fn(), disconnect: vi.fn(), destroy: vi.fn(), awareness: {} },
	awareness: {},
	destroy: mockDestroy
}));

vi.mock('./session', () => ({
	createYjsSession: (templateId: string) => mockCreateYjsSession(templateId)
}));

describe('session-store', () => {
	let getSession: typeof import('./session-store').getSession;
	let releaseSession: typeof import('./session-store').releaseSession;

	beforeEach(async () => {
		vi.resetModules();
		mockCreateYjsSession.mockClear();
		mockDestroy.mockClear();

		const store = await import('./session-store');
		getSession = store.getSession;
		releaseSession = store.releaseSession;
	});

	it('creates new session on first call', () => {
		getSession('template-1');
		expect(mockCreateYjsSession).toHaveBeenCalledTimes(1);
		expect(mockCreateYjsSession).toHaveBeenCalledWith('template-1');
	});

	it('returns same session on second call', () => {
		const s1 = getSession('template-1');
		const s2 = getSession('template-1');
		expect(s1).toBe(s2);
		expect(mockCreateYjsSession).toHaveBeenCalledTimes(1);
	});

	it('release at refcount>0 does not destroy', () => {
		getSession('template-1');
		getSession('template-1');
		releaseSession('template-1');
		expect(mockDestroy).not.toHaveBeenCalled();
	});

	it('release at refcount=0 destroys', () => {
		getSession('template-1');
		releaseSession('template-1');
		expect(mockDestroy).toHaveBeenCalledTimes(1);
	});

	it('release unknown templateId is safe', () => {
		expect(() => releaseSession('nonexistent')).not.toThrow();
	});

	it('after destroy, get creates fresh session', () => {
		getSession('template-1');
		releaseSession('template-1');
		expect(mockCreateYjsSession).toHaveBeenCalledTimes(1);

		getSession('template-1');
		expect(mockCreateYjsSession).toHaveBeenCalledTimes(2);
	});
});
