/**
 * Task store for Mekhan.
 *
 * Fetches tasks from the HPI proxy API and listens for SSE events
 * to trigger re-fetches for live updates.
 */

import { listTasks } from '$lib/api/client';
import { authFetch } from '$lib/auth/fetch';
import { connectSse, type SseConnection } from '$lib/net/sse';
import type { HumanTask } from '$lib/types/tasks';

const SSE_URL = '/api/tasks/stream';
const SSE_MAX_RETRIES = 5;
const SSE_INITIAL_RETRY_MS = 1000;

export function createTaskStore() {
	let tasks: HumanTask[] = $state([]);
	let total = $state(0);
	let loading = $state(true);
	let error: string | null = $state(null);

	// SSE state
	let sseConnection: SseConnection | null = null;
	let destroyed = false;

	// Current filter
	let currentStatus: string | undefined = undefined;

	// Callback for process update events (used by process pages)
	let processUpdateCallback: (() => void) | null = null;

	async function fetchTasks(status?: string) {
		try {
			const result = await listTasks({ status, limit: 100 });
			tasks = result.tasks ?? [];
			total = result.total ?? tasks.length;
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load tasks';
		} finally {
			loading = false;
		}
	}

	const TASK_EVENTS = new Set([
		'task_created',
		'task_completed',
		'task_failed',
		'task_cancelled'
	]);

	function connectSSE() {
		if (destroyed) return;
		sseConnection?.close();
		sseConnection = connectSse(SSE_URL, {
			fetchImpl: authFetch,
			maxRetries: SSE_MAX_RETRIES,
			initialRetryMs: SSE_INITIAL_RETRY_MS,
			onEvent: ({ event }) => {
				if (TASK_EVENTS.has(event)) {
					setTimeout(() => fetchTasks(currentStatus), 300);
				}
				if (event === 'process_update' && processUpdateCallback) {
					setTimeout(() => processUpdateCallback?.(), 300);
				}
			}
		});
	}

	function init(status?: string) {
		currentStatus = status;
		loading = true;
		fetchTasks(status);
		connectSSE();
	}

	function refetch(status?: string) {
		currentStatus = status;
		fetchTasks(status);
	}

	function destroy() {
		destroyed = true;
		sseConnection?.close();
		sseConnection = null;
	}

	return {
		get tasks() {
			return tasks;
		},
		get total() {
			return total;
		},
		get loading() {
			return loading;
		},
		get error() {
			return error;
		},
		init,
		refetch,
		destroy,
		onProcessUpdate(cb: () => void) {
			processUpdateCallback = cb;
		}
	};
}
