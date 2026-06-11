/**
 * Profile cache — the identity seam's client half.
 *
 * Turns any user UUID (`created_by`, `author_id`, grant rows, …) into a
 * renderable `{display_name, email, avatar_url}` via the batch resolver
 * `POST /api/v1/users/profiles`, with three properties that keep scattered
 * `UserChip`s from each firing their own request:
 *
 *   - **Coalescing**: every `ensure(ids)` in a microtask is merged into ONE
 *     POST (a single flush on the next microtask).
 *   - **Dedup**: ids already cached, already resolved as missing, or already
 *     in flight are never re-requested.
 *   - **Reactive reads**: `get(id)` is a synchronous rune read, so a component
 *     that called `ensure([id])` re-renders when the batch lands.
 *
 * Seeded from the auth session so the caller's own identity (the most common
 * authorship UUID) needs no round trip.
 */
import { resolveProfiles, type UserProfileDto } from '$lib/api/client';
import { auth } from '$lib/auth/store.svelte';

class ProfileCache {
	// Resolved rows, keyed by user_id. A key present with `null` value is a
	// NEGATIVE cache entry (resolved, but no profile row) — never re-requested.
	#cache = $state<Record<string, UserProfileDto | null>>({});
	// Ids requested but not yet flushed, plus ids whose flush is in flight, so
	// overlapping `ensure` calls don't double-request.
	#pending = new Set<string>();
	#inflight = new Set<string>();
	#flushScheduled = false;

	/** Reactive read. `undefined` = not yet resolved (render a skeleton);
	 *  `null` = resolved-but-missing (render UUID/initials fallback). */
	get(id: string): UserProfileDto | null | undefined {
		return this.#cache[id];
	}

	/** Request resolution for `ids`. Cheap to call from a render `$effect` —
	 *  already-known / in-flight ids are filtered out and the rest batch on the
	 *  next microtask. */
	ensure(ids: Array<string | null | undefined>): void {
		let added = false;
		for (const id of ids) {
			if (!id) continue;
			if (id in this.#cache) continue;
			if (this.#pending.has(id) || this.#inflight.has(id)) continue;
			this.#pending.add(id);
			added = true;
		}
		if (added && !this.#flushScheduled) {
			this.#flushScheduled = true;
			queueMicrotask(() => this.#flush());
		}
	}

	/** Seed a profile we already have in hand (e.g. a denormalized member row),
	 *  so a later `UserChip` for the same UUID never re-fetches. */
	seed(profile: UserProfileDto): void {
		if (!(profile.user_id in this.#cache)) {
			this.#cache = { ...this.#cache, [profile.user_id]: profile };
		}
	}

	async #flush(): Promise<void> {
		this.#flushScheduled = false;
		const batch = [...this.#pending];
		this.#pending.clear();
		if (batch.length === 0) return;
		batch.forEach((id) => this.#inflight.add(id));

		try {
			const rows = await resolveProfiles(batch);
			const found = new Set(rows.map((r) => r.user_id));
			const next = { ...this.#cache };
			for (const row of rows) next[row.user_id] = row;
			// Negative-cache the ids that came back empty so they don't re-request.
			for (const id of batch) if (!found.has(id)) next[id] = null;
			this.#cache = next;
		} catch {
			// Leave failed ids uncached so a later `ensure` can retry; just drop
			// them from in-flight. A transient resolver blip degrades to initials.
		} finally {
			batch.forEach((id) => this.#inflight.delete(id));
		}
	}
}

export const profiles = new ProfileCache();

/** Seed the cache with the signed-in user's own identity once the session is
 *  known, so the most common authorship UUID resolves with zero round trips.
 *  Call from the root layout after `auth.init()`. */
export function seedSelfProfile(): void {
	const u = auth.session?.user;
	if (u?.userId) {
		profiles.seed({
			user_id: u.userId,
			display_name: u.displayName ?? undefined,
			email: u.email ?? undefined,
			avatar_url: u.avatarUrl ?? undefined
		});
	}
}
