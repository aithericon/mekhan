/**
 * Task store for Mekhan.
 *
 * Fetches tasks from the HPI proxy API and listens for SSE events
 * to trigger re-fetches for live updates.
 */

import { listTasks } from '$lib/api/client';
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
	let sseAbortController: AbortController | null = null;
	let sseRetryCount = 0;
	let destroyed = false;

	// Current filter
	let currentStatus: string | undefined = undefined;

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

	function connectSSE() {
		if (destroyed) return;
		sseAbortController?.abort();
		const controller = new AbortController();
		sseAbortController = controller;

		(async () => {
			try {
				const resp = await fetch(SSE_URL, { signal: controller.signal });
				if (!resp.ok || !resp.body) {
					throw new Error(`SSE connect failed: ${resp.status}`);
				}

				sseRetryCount = 0;
				const reader = resp.body.getReader();
				const decoder = new TextDecoder();
				let buffer = '';

				while (true) {
					const { done, value } = await reader.read();
					if (done) break;

					buffer += decoder.decode(value, { stream: true });
					const lines = buffer.split('\n');
					buffer = lines.pop() ?? '';

					let shouldRefetch = false;
					for (const line of lines) {
						if (
							line.startsWith('event: task_created') ||
							line.startsWith('event: task_completed') ||
							line.startsWith('event: task_failed') ||
							line.startsWith('event: task_cancelled')
						) {
							shouldRefetch = true;
						}
					}

					if (shouldRefetch) {
						// Debounce: wait a small bit for HPI to process, then re-fetch
						setTimeout(() => fetchTasks(currentStatus), 300);
					}
				}
			} catch (e) {
				if (controller.signal.aborted) return;
				console.warn('SSE error:', e);
			}

			// Reconnect with exponential backoff
			if (!destroyed && sseRetryCount < SSE_MAX_RETRIES) {
				const delay = SSE_INITIAL_RETRY_MS * Math.pow(2, sseRetryCount);
				sseRetryCount++;
				setTimeout(() => connectSSE(), delay);
			}
		})();
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
		sseAbortController?.abort();
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
		destroy
	};
}
