import { createYjsSession, type YjsSession } from './session';

type SessionEntry = {
	session: YjsSession;
	refcount: number;
};

const sessions = new Map<string, SessionEntry>();

export function getSession(templateId: string): YjsSession {
	let entry = sessions.get(templateId);
	if (entry) {
		entry.refcount++;
		return entry.session;
	}

	const session = createYjsSession(templateId);
	sessions.set(templateId, { session, refcount: 1 });
	return session;
}

export function releaseSession(templateId: string): void {
	const entry = sessions.get(templateId);
	if (!entry) return;

	entry.refcount--;
	if (entry.refcount <= 0) {
		entry.session.destroy();
		sessions.delete(templateId);
	}
}
