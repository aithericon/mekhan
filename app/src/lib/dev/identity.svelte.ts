/**
 * Dev-identity store — the acting-user switcher for `dev_noop`.
 *
 * Counterpart to the workspace store: caches the seeded dev roster the
 * `NoopAuthenticator` can impersonate. The roster is EMPTY under any real auth
 * mode (the server gates it on `auth.mode == dev_noop`), which is the signal the
 * picker uses to hide itself entirely outside local dev.
 */
import { listDevIdentities, setDevIdentity, type DevIdentity } from '$lib/api/client';

class DevIdentityStore {
	#identities = $state<DevIdentity[]>([]);
	#loaded = $state(false);
	#loading = $state(false);

	get identities(): DevIdentity[] {
		return this.#identities;
	}

	get loaded(): boolean {
		return this.#loaded;
	}

	/** True only when there's a real choice to make (dev_noop, >1 identity). */
	get enabled(): boolean {
		return this.#identities.length > 1;
	}

	get active(): DevIdentity | null {
		return this.#identities.find((i) => i.active) ?? null;
	}

	/** Idempotent: safe to call from the layout on every navigation. */
	async load(): Promise<void> {
		if (this.#loaded || this.#loading) return;
		this.#loading = true;
		try {
			this.#identities = await listDevIdentities();
			this.#loaded = true;
		} catch {
			// Quiet failure — picker stays hidden, navigation continues.
			this.#identities = [];
		} finally {
			this.#loading = false;
		}
	}

	/**
	 * Switch the acting dev user, then hard-reload so every workspace-keyed
	 * store refetches under the new identity. The server also clears the
	 * active-workspace cookie, so the new user lands in its own default org.
	 */
	async switchTo(subject: string): Promise<void> {
		if (this.active?.subject === subject) return;
		await setDevIdentity(subject);
		if (typeof window !== 'undefined') {
			window.location.reload();
		}
	}
}

export const devIdentity = new DevIdentityStore();
