import { describe, it, expect } from 'vitest';

import { compileApiErrorFromBody, CompileApiError } from './client';

// The one pure seam shared by `publishTemplate` and `createInstance` (draft
// dev-run): lifting the backend's `compile_errors` envelope into a
// `CompileApiError` the editor routes to its canvas-highlight plumbing.
describe('compileApiErrorFromBody', () => {
	it('lifts a populated compile_errors envelope', () => {
		const err = compileApiErrorFromBody({
			error: 'compilation failed: no Start node',
			code: 'compile-failed',
			compile_errors: [{ kind: 'compilation', message: 'no Start node' }]
		});
		expect(err).toBeInstanceOf(CompileApiError);
		expect(err?.message).toBe('compilation failed: no Start node');
		expect(err?.compileErrors).toHaveLength(1);
	});

	it('falls back to a generic message when `error` is absent', () => {
		const err = compileApiErrorFromBody({
			compile_errors: [{ kind: 'compilation', message: 'boom', node_id: 'n1' }]
		});
		expect(err?.message).toBe('compilation failed');
	});

	it('returns null for empty diagnostics, plain errors, and non-objects', () => {
		// An empty array is NOT a compile failure — the generic ApiError path
		// must keep handling it (e.g. a 400 from start-token validation).
		expect(compileApiErrorFromBody({ error: 'x', compile_errors: [] })).toBeNull();
		expect(compileApiErrorFromBody({ error: 'template version is not published' })).toBeNull();
		expect(compileApiErrorFromBody({ error: 'x', compile_errors: null })).toBeNull();
		expect(compileApiErrorFromBody(undefined)).toBeNull();
		expect(compileApiErrorFromBody('plain string body')).toBeNull();
	});
});
