// Shared state for the /data Entries query surface. The query rail lives in
// the page-level sidebar snippet while EntriesTab lives inside a Tabs.Content
// panel — both need the applied query (facets scope to it), the QueryBar's
// draft text (the rail's field reference inserts into it), and paging. One
// instance is created per /data page mount (NOT a module singleton — a
// singleton would go stale against ?q= on re-navigation) and handed to both
// sides; the rail snippet captures it lexically.
import { browser } from '$app/environment';
import { addTerm } from './query-language';

export class EntriesQueryState {
	/** The executed query text — drives the result list and facet scoping. */
	applied = $state('');
	/** The QueryBar input draft — applied on Enter / Apply. */
	draft = $state('');
	/** Result page; reset by every apply. */
	page = $state(0);

	/** When false, `apply` does NOT push the query into `?q=` on the page URL.
	 *  The /data browser wants the sync (deep-linkable queries); embedded uses
	 *  like the trigger-node filter editor own their own persistence and must
	 *  not clobber the editor page URL. */
	private syncUrlEnabled: boolean;

	constructor(initialQ = '', syncUrl = true) {
		this.applied = initialQ;
		this.draft = initialQ;
		this.syncUrlEnabled = syncUrl;
	}

	/** Apply new query text: reset paging + sync ?q= (same pattern as ?inspect). */
	apply(text: string) {
		this.applied = text;
		this.draft = text;
		this.page = 0;
		this.syncUrl(text);
	}

	/** Add a complete term (facet bucket, ArtifactCard chip) and apply. */
	addTerm(term: string) {
		this.apply(addTerm(this.applied, term));
	}

	/** Append to the draft only — e.g. a field-reference `format:` stub the
	 *  user still has to complete; nothing executes until they apply. */
	insertDraft(term: string) {
		this.draft = addTerm(this.draft, term);
	}

	// URL sync is event-driven (inside apply), NOT an $effect — an effect
	// would also fire replaceState once on deep-link hydration.
	private syncUrl(text: string) {
		if (!browser || !this.syncUrlEnabled) return;
		const url = new URL(window.location.href);
		if (text.trim()) url.searchParams.set('q', text);
		else url.searchParams.delete('q');
		history.replaceState(null, '', url.toString());
	}
}
