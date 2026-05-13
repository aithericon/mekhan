import createClient from 'openapi-fetch';
import type { paths } from './schema';

export const api = createClient<paths>({
	baseUrl: ''
});

// Helper types extracted from the schema
export type PetriNet = paths['/api/topology']['get']['responses']['200']['content']['application/json']['topology'];
export type Place = NonNullable<PetriNet>['places'][number];
export type Transition = NonNullable<PetriNet>['transitions'][number];
export type Arc = NonNullable<PetriNet>['arcs'][number];
export type Token = paths['/api/state']['get']['responses']['200']['content']['application/json']['marking']['tokens'][string][number];
export type PersistedEvent = paths['/api/events']['get']['responses']['200']['content']['application/json']['events'][number];
export type Marking = paths['/api/state']['get']['responses']['200']['content']['application/json']['marking'];

// Port-based model types (manually added until schema regenerates)
export interface Port {
	name: string;
	schema_ref?: string | null;
	cardinality: 'single' | 'batch';
}

// Transition status types (manually added until schema regenerates)
export type TransitionStatus =
	| { status: 'enabled' }
	| { status: 'disabled_no_tokens'; missing_place: string }
	| { status: 'disabled_guard_failed'; guard: string }
	| { status: 'disabled_guard_error'; error: string };

export type TokenColor =
	| { type: 'Unit' }
	| { type: 'Integer'; value: number }
	| { type: 'Data'; value: unknown };
