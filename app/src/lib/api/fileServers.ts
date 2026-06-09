/**
 * File-server entity client (docs/32 §4.1).
 *
 * First-class storage backends the platform tracks files on. A server is now
 * identity-only — its access methods live in N child *endpoints*. Each endpoint
 * carries its own transport (`access_method`), `root`, optional `resource_ref`
 * (connection + secrets in the referenced workspace `resource`), and
 * status/verification. Derived rollups (file count/size) hang off the entity.
 * Thin wrappers over the JSON contract — types flow from the generated OpenAPI
 * schema.
 */
import { authFetch } from '$lib/auth/fetch';
import { rawJson, ApiError } from './client';
import type { components } from './schema';

export type FileServer = components['schemas']['FileServer'];
export type FileServerView = components['schemas']['FileServerView'];
export type FileServerEndpoint = components['schemas']['FileServerEndpoint'];
export type UnregisteredServer = components['schemas']['UnregisteredServer'];
export type FileServersResponse = components['schemas']['FileServersResponse'];
export type CreateFileServerRequest = components['schemas']['CreateFileServerRequest'];
export type UpdateFileServerRequest = components['schemas']['UpdateFileServerRequest'];
export type CreateEndpointRequest = components['schemas']['CreateEndpointRequest'];
export type UpdateEndpointRequest = components['schemas']['UpdateEndpointRequest'];

const BASE = '/api/v1/file-servers';

/** Registered servers (with endpoints + rollups) + unregistered inventory keys (adopt candidates). */
export async function listFileServers(): Promise<FileServersResponse> {
	return rawJson('/file-servers');
}

/** One server with endpoints + rollups. */
export async function getFileServer(key: string): Promise<FileServerView> {
	return rawJson(`/file-servers/${encodeURIComponent(key)}`);
}

async function send<T>(method: string, path: string, body?: unknown): Promise<T> {
	const res = await authFetch(`${BASE}${path}`, {
		method,
		headers: body !== undefined ? { 'content-type': 'application/json' } : undefined,
		body: body !== undefined ? JSON.stringify(body) : undefined
	});
	if (!res.ok) {
		throw new ApiError(res.status, await res.text());
	}
	// DELETE returns 204 with no body.
	if (res.status === 204) return undefined as T;
	return res.json() as Promise<T>;
}

/** Register a new file server (optionally with a first inline endpoint). */
export async function createFileServer(req: CreateFileServerRequest): Promise<FileServer> {
	return send<FileServer>('POST', '', req);
}

/** Promote an unregistered inventory `file_server_id` into a real entity. */
export async function adoptFileServer(req: CreateFileServerRequest): Promise<FileServer> {
	return send<FileServer>('POST', '/adopt', req);
}

/** Update mutable identity fields of a server. */
export async function updateFileServer(
	key: string,
	req: UpdateFileServerRequest
): Promise<FileServer> {
	return send<FileServer>('PUT', `/${encodeURIComponent(key)}`, req);
}

/** Delete a server entity (endpoints cascade; inventory rows are untouched — they revert to unregistered). */
export async function deleteFileServer(key: string): Promise<void> {
	return send<void>('DELETE', `/${encodeURIComponent(key)}`);
}

// --- Endpoint sub-resource --------------------------------------------------

/** List a server's endpoints (the access methods onto the backend). */
export async function listEndpoints(key: string): Promise<FileServerEndpoint[]> {
	return rawJson(`/file-servers/${encodeURIComponent(key)}/endpoints`);
}

/** Add an endpoint to a server. */
export async function createEndpoint(
	key: string,
	req: CreateEndpointRequest
): Promise<FileServerEndpoint> {
	return send<FileServerEndpoint>('POST', `/${encodeURIComponent(key)}/endpoints`, req);
}

/** Update one endpoint of a server. */
export async function updateEndpoint(
	key: string,
	endpointId: string,
	req: UpdateEndpointRequest
): Promise<FileServerEndpoint> {
	return send<FileServerEndpoint>(
		'PUT',
		`/${encodeURIComponent(key)}/endpoints/${encodeURIComponent(endpointId)}`,
		req
	);
}

/** Remove one endpoint of a server. */
export async function deleteEndpoint(key: string, endpointId: string): Promise<void> {
	return send<void>(
		'DELETE',
		`/${encodeURIComponent(key)}/endpoints/${encodeURIComponent(endpointId)}`
	);
}
