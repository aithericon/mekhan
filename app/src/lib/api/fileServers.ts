/**
 * File-server entity client (docs/32 §4.1).
 *
 * First-class storage backends the platform tracks files on. Identity + derived
 * rollups live on the entity; connection + secrets live in the referenced
 * workspace `resource`. Thin wrappers over the JSON contract — types flow from
 * the generated OpenAPI schema.
 */
import { authFetch } from '$lib/auth/fetch';
import { rawJson, ApiError } from './client';
import type { components } from './schema';

export type FileServer = components['schemas']['FileServer'];
export type FileServerView = components['schemas']['FileServerView'];
export type UnregisteredServer = components['schemas']['UnregisteredServer'];
export type FileServersResponse = components['schemas']['FileServersResponse'];
export type CreateFileServerRequest = components['schemas']['CreateFileServerRequest'];
export type UpdateFileServerRequest = components['schemas']['UpdateFileServerRequest'];

const BASE = '/api/v1/file-servers';

/** Registered servers (with rollups) + unregistered inventory keys (adopt candidates). */
export async function listFileServers(): Promise<FileServersResponse> {
	return rawJson('/file-servers');
}

/** One server with rollups. */
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

/** Register a new file server. */
export async function createFileServer(req: CreateFileServerRequest): Promise<FileServer> {
	return send<FileServer>('POST', '', req);
}

/** Promote an unregistered inventory `file_server_id` into a real entity. */
export async function adoptFileServer(req: CreateFileServerRequest): Promise<FileServer> {
	return send<FileServer>('POST', '/adopt', req);
}

/** Update mutable fields of a server. */
export async function updateFileServer(
	key: string,
	req: UpdateFileServerRequest
): Promise<FileServer> {
	return send<FileServer>('PUT', `/${encodeURIComponent(key)}`, req);
}

/** Delete a server entity (inventory rows are untouched — they revert to unregistered). */
export async function deleteFileServer(key: string): Promise<void> {
	return send<void>('DELETE', `/${encodeURIComponent(key)}`);
}
