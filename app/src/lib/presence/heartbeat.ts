/**
 * Global human-presence heartbeat.
 *
 * `session`-mode availability (the default human enrollment) is a TTL the server
 * renews off `human.{member}.presence` heartbeats. Tying that heartbeat to a
 * single page (the inbox/tasks/instance task-stream) meant a member went offline
 * ~45s after navigating anywhere else — even with the app open and "Available"
 * showing. This runs the heartbeat for the WHOLE authenticated session instead,
 * so presence follows "the app is open" (what the `session` mode promises),
 * regardless of which page the member is on.
 *
 * Started once from the root authenticated layout. Gated on the caller actually
 * being an enrolled human (a non-human session would just no-op on the
 * controller, but skipping spares needless pings + self-heal DB lookups).
 */
import { getMyEnrollments, sendPresenceHeartbeat } from '$lib/api/roster';

/** Ping cadence — well under the 45s `session` TTL (matches the task-stream SSE
 *  ping), so two missed pings still leave a renewal in the window. */
const HEARTBEAT_MS = 10_000;

/**
 * Begin heartbeating if the caller is an enrolled human. Returns a stop function
 * (idempotent) — call it on layout teardown / sign-out. Closing the tab also
 * stops the pings, and the server-side TTL sweep then reaps the member, which is
 * the intended "left the session" behaviour.
 */
export function startPresenceHeartbeat(): () => void {
	let stopped = false;
	let timer: ReturnType<typeof setInterval> | null = null;

	void (async () => {
		const mine = await getMyEnrollments().catch(() => []);
		if (stopped || mine.length === 0) return;
		void sendPresenceHeartbeat();
		timer = setInterval(() => void sendPresenceHeartbeat(), HEARTBEAT_MS);
	})();

	return () => {
		stopped = true;
		if (timer) {
			clearInterval(timer);
			timer = null;
		}
	};
}
