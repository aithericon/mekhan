import { env } from '$env/dynamic/private';
import type { RequestHandler } from './$types';

const BACKEND_URL = env.MEKHAN_SERVICE_URL ?? 'http://localhost:3100';

/** Proxy all /api/* requests to the mekhan-service backend */
const handler: RequestHandler = async ({ params, request }) => {
	const path = params.path;
	const url = new URL(request.url);
	const target = `${BACKEND_URL}/api/${path}${url.search}`;

	const headers = new Headers(request.headers);
	// Remove host header so the backend gets the correct one
	headers.delete('host');

	const init: RequestInit = {
		method: request.method,
		headers
	};

	// Forward body for methods that have one (use arrayBuffer to preserve binary/multipart)
	if (request.method !== 'GET' && request.method !== 'HEAD') {
		init.body = new Uint8Array(await request.arrayBuffer());
	}

	let response: Response;
	try {
		response = await fetch(target, init);
	} catch {
		return new Response(JSON.stringify({ error: 'Backend not available' }), {
			status: 503,
			headers: { 'content-type': 'application/json' }
		});
	}

	return new Response(response.body, {
		status: response.status,
		statusText: response.statusText,
		headers: response.headers
	});
};

export const GET = handler;
export const POST = handler;
export const PUT = handler;
export const PATCH = handler;
export const DELETE = handler;
