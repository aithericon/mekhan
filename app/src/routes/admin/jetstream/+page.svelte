<script lang="ts">
	// JetStream introspection — a platform-admin read-only window onto the NATS
	// JetStream store behind mekhan/engine/executor. Two panes: the stream list
	// (left, polled) and the selected stream's consumers + a non-destructive
	// message peek (right). Built for debugging stuck nets / dead-letter queues
	// (MEKHAN_SILENT_DROPS, runner-jobs_dlq) from the browser instead of `nats`.
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import * as Table from '$lib/components/ui/table';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Database from '@lucide/svelte/icons/database';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import { auth } from '$lib/auth/store.svelte';
	import { createPolledState } from '$lib/stores/remote.svelte';
	import {
		listStreams,
		getStream,
		peekMessages,
		type JsStreamDetail,
		type JsMessage
	} from '$lib/api/jetstream';

	const isAdmin = $derived(auth.isPlatformAdmin);

	// Stream list polled every 8s — cheap (counts only), and keeps the row
	// metrics live while staring at a draining/stuck stream.
	const streams = createPolledState(listStreams, 8000, {
		errorFallback: 'Failed to list streams'
	});
	const streamList = $derived(streams.data ?? []);

	let selected = $state<string | null>(null);
	let detail = $state<JsStreamDetail | null>(null);
	let detailError = $state<string | null>(null);
	let detailLoading = $state(false);

	let messages = $state<JsMessage[]>([]);
	let msgError = $state<string | null>(null);
	let msgLoading = $state(false);
	let nextBefore = $state<number | null>(null);
	let firstSeq = $state(0);
	let lastSeq = $state(0);

	let expanded = $state<Record<number, boolean>>({});

	// Dead-letter / drop streams get a destructive badge so they're easy to spot.
	function isDlq(name: string): boolean {
		const n = name.toLowerCase();
		return n.includes('dlq') || n.includes('silent_drop') || n.includes('dead');
	}

	async function selectStream(name: string) {
		selected = name;
		detail = null;
		detailError = null;
		detailLoading = true;
		messages = [];
		msgError = null;
		nextBefore = null;
		expanded = {};
		try {
			detail = await getStream(name);
		} catch (e) {
			detailError = e instanceof Error ? e.message : 'Failed to load stream';
		} finally {
			detailLoading = false;
		}
		await loadMessages(true);
	}

	async function loadMessages(reset: boolean) {
		if (!selected) return;
		msgLoading = true;
		msgError = null;
		try {
			const before = reset ? undefined : (nextBefore ?? undefined);
			const r = await peekMessages(selected, { beforeSeq: before, limit: 50 });
			firstSeq = r.first_seq;
			lastSeq = r.last_seq;
			nextBefore = r.next_before_seq ?? null;
			messages = reset ? r.messages : [...messages, ...r.messages];
		} catch (e) {
			msgError = e instanceof Error ? e.message : 'Failed to peek messages';
		} finally {
			msgLoading = false;
		}
	}

	function toggle(seq: number) {
		expanded[seq] = !expanded[seq];
	}

	function pretty(m: JsMessage): string {
		if (m.payload_json !== undefined && m.payload_json !== null) {
			try {
				return JSON.stringify(m.payload_json, null, 2);
			} catch {
				/* fall through to raw text */
			}
		}
		return m.payload_text;
	}

	function fmtBytes(n: number): string {
		if (n < 1024) return `${n} B`;
		if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
		return `${(n / (1024 * 1024)).toFixed(1)} MiB`;
	}

	function fmtNum(n: number): string {
		return n.toLocaleString();
	}

	function fmtTime(iso: string): string {
		if (!iso) return '—';
		const d = new Date(iso);
		return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
	}
</script>

<PageShell width="wide" testid="jetstream-page">
	{#snippet band()}
		<PageHeader
			title="JetStream"
			subtitle="NATS JetStream introspection — streams, consumers & a non-destructive message peek"
		>
			{#snippet actions()}
				<Button
					variant="outline"
					size="sm"
					onclick={() => void streams.poll()}
					data-testid="jetstream-refresh"
				>
					<RefreshCw class="size-3.5" />
					Refresh
				</Button>
			{/snippet}
		</PageHeader>
	{/snippet}

	{#if !isAdmin}
		<div
			class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200"
		>
			JetStream introspection requires platform admin.
		</div>
	{:else}
		{#if streams.error}
			<div
				class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900/50 dark:bg-amber-950/30 dark:text-amber-200"
			>
				{streams.error}
			</div>
		{/if}

		<div class="grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,22rem)_minmax(0,1fr)]">
			<!-- ── Stream list ──────────────────────────────────────────── -->
			<div class="rounded-lg border border-border bg-card">
				<div
					class="flex items-center gap-2 border-b border-border px-3 py-2 text-sm font-medium"
				>
					<Database class="size-4 text-muted-foreground" />
					Streams
					<Badge variant="muted" size="xs">{streamList.length}</Badge>
				</div>
				{#if streamList.length === 0}
					<div class="px-3 py-8 text-center text-sm text-muted-foreground">
						{streams.data === null ? 'Loading…' : 'No streams'}
					</div>
				{:else}
					<ul class="max-h-[70vh] divide-y divide-border overflow-auto">
						{#each streamList as s (s.name)}
							<li>
								<button
									type="button"
									class="flex w-full flex-col gap-1 px-3 py-2 text-left transition-colors hover:bg-muted/50 {selected ===
									s.name
										? 'bg-muted'
										: ''}"
									onclick={() => void selectStream(s.name)}
									data-testid="jetstream-stream-row"
								>
									<div class="flex items-center justify-between gap-2">
										<span class="truncate font-mono text-xs font-medium">{s.name}</span>
										{#if isDlq(s.name)}
											<Badge variant="destructive" size="xs">DLQ</Badge>
										{/if}
									</div>
									<div class="flex items-center gap-2 text-[11px] text-muted-foreground">
										<span>{fmtNum(s.messages)} msgs</span>
										<span>·</span>
										<span>{fmtBytes(s.bytes)}</span>
										<span>·</span>
										<span>{s.consumer_count} cons</span>
									</div>
								</button>
							</li>
						{/each}
					</ul>
				{/if}
			</div>

			<!-- ── Selected stream detail ───────────────────────────────── -->
			<div class="min-w-0">
				{#if !selected}
					<div
						class="flex h-40 items-center justify-center rounded-lg border border-dashed border-border text-sm text-muted-foreground"
					>
						Select a stream to inspect its consumers and messages.
					</div>
				{:else}
					<div class="space-y-4">
						<div class="flex items-center gap-2">
							<h2 class="font-mono text-base font-semibold">{selected}</h2>
							{#if isDlq(selected)}
								<Badge variant="destructive" size="sm">
									<TriangleAlert class="size-3" />
									dead-letter
								</Badge>
							{/if}
							<Button
								variant="ghost"
								size="sm"
								class="ml-auto"
								onclick={() => void selectStream(selected!)}
							>
								<RefreshCw class="size-3.5" />
								Reload
							</Button>
						</div>

						{#if detailError}
							<div
								class="rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-3 text-sm text-destructive"
							>
								{detailError}
							</div>
						{/if}

						<!-- Stream stats + consumers -->
						{#if detail}
							{@const st = detail}
							<div class="rounded-lg border border-border bg-card p-4">
								<div class="grid grid-cols-2 gap-x-6 gap-y-2 text-sm sm:grid-cols-4">
									<div>
										<div class="text-xs text-muted-foreground">Messages</div>
										<div class="font-medium">{fmtNum(st.messages)}</div>
									</div>
									<div>
										<div class="text-xs text-muted-foreground">Bytes</div>
										<div class="font-medium">{fmtBytes(st.bytes)}</div>
									</div>
									<div>
										<div class="text-xs text-muted-foreground">Sequences</div>
										<div class="font-medium">{fmtNum(st.first_seq)} – {fmtNum(st.last_seq)}</div>
									</div>
									<div>
										<div class="text-xs text-muted-foreground">Subjects</div>
										<div class="font-medium">{fmtNum(st.subjects_count)}</div>
									</div>
								</div>
								{#if st.subjects.length > 0}
									<div class="mt-3 flex flex-wrap gap-1">
										{#each st.subjects as subj (subj)}
											<Badge variant="outline" size="xs"><span class="font-mono">{subj}</span></Badge>
										{/each}
									</div>
								{/if}
							</div>

							<div class="rounded-lg border border-border bg-card">
								<div class="border-b border-border px-3 py-2 text-sm font-medium">
									Consumers
									<Badge variant="muted" size="xs">{detail.consumers.length}</Badge>
								</div>
								{#if detail.consumers.length === 0}
									<div class="px-3 py-6 text-center text-sm text-muted-foreground">
										No consumers bound.
									</div>
								{:else}
									<div class="overflow-x-auto">
										<Table.Root>
											<Table.Header>
												<Table.Row>
													<Table.Head>Name</Table.Head>
													<Table.Head class="text-right">Pending</Table.Head>
													<Table.Head class="text-right">Ack pending</Table.Head>
													<Table.Head class="text-right">Redelivered</Table.Head>
													<Table.Head class="text-right">Waiting</Table.Head>
													<Table.Head class="text-right">Delivered seq</Table.Head>
													<Table.Head class="text-right">Ack floor</Table.Head>
												</Table.Row>
											</Table.Header>
											<Table.Body>
												{#each detail.consumers as c (c.name)}
													<Table.Row>
														<Table.Cell>
															<span class="font-mono text-xs">{c.name}</span>
															{#if !c.durable}
																<Badge variant="muted" size="xs">ephemeral</Badge>
															{/if}
														</Table.Cell>
														<Table.Cell class="text-right">
															{#if c.num_pending > 0}
																<Badge variant="warning" size="xs">{fmtNum(c.num_pending)}</Badge>
															{:else}
																<span class="text-muted-foreground">0</span>
															{/if}
														</Table.Cell>
														<Table.Cell class="text-right">
															{#if c.num_ack_pending > 0}
																<Badge variant="destructive" size="xs"
																	>{fmtNum(c.num_ack_pending)}</Badge
																>
															{:else}
																<span class="text-muted-foreground">0</span>
															{/if}
														</Table.Cell>
														<Table.Cell class="text-right">{fmtNum(c.num_redelivered)}</Table.Cell>
														<Table.Cell class="text-right">{fmtNum(c.num_waiting)}</Table.Cell>
														<Table.Cell class="text-right font-mono text-xs"
															>{fmtNum(c.delivered_stream_seq)}</Table.Cell
														>
														<Table.Cell class="text-right font-mono text-xs"
															>{fmtNum(c.ack_floor_stream_seq)}</Table.Cell
														>
													</Table.Row>
												{/each}
											</Table.Body>
										</Table.Root>
									</div>
								{/if}
							</div>
						{/if}

						<!-- Message peek -->
						<div class="rounded-lg border border-border bg-card">
							<div class="flex items-center gap-2 border-b border-border px-3 py-2 text-sm font-medium">
								Messages
								<span class="text-xs font-normal text-muted-foreground">newest first</span>
								{#if msgLoading}
									<span class="text-xs font-normal text-muted-foreground">loading…</span>
								{/if}
							</div>

							{#if msgError}
								<div class="px-3 py-3 text-sm text-destructive">{msgError}</div>
							{:else if messages.length === 0 && !msgLoading}
								<div class="px-3 py-8 text-center text-sm text-muted-foreground">
									Stream is empty.
								</div>
							{:else}
								<ul class="divide-y divide-border">
									{#each messages as m (m.seq)}
										<li>
											<button
												type="button"
												class="flex w-full items-start gap-2 px-3 py-2 text-left transition-colors hover:bg-muted/50"
												onclick={() => toggle(m.seq)}
											>
												{#if expanded[m.seq]}
													<ChevronDown class="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
												{:else}
													<ChevronRight class="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
												{/if}
												<div class="min-w-0 flex-1">
													<div class="flex items-center gap-2">
														<span class="font-mono text-xs text-muted-foreground">#{m.seq}</span>
														<span class="truncate font-mono text-xs">{m.subject}</span>
													</div>
													<div class="text-[11px] text-muted-foreground">
														{fmtTime(m.time)} · {fmtBytes(m.size)}
													</div>
												</div>
											</button>
											{#if expanded[m.seq]}
												<div class="space-y-2 px-3 pb-3 pl-8">
													{#if m.headers.length > 0}
														<div class="flex flex-wrap gap-1">
															{#each m.headers as h (h.name)}
																<Badge variant="muted" size="xs">
																	<span class="font-mono">{h.name}: {h.value}</span>
																</Badge>
															{/each}
														</div>
													{/if}
													<pre
														class="max-h-96 overflow-auto rounded-md bg-muted/60 p-3 font-mono text-xs leading-relaxed">{pretty(
															m
														)}</pre>
													{#if m.truncated}
														<div class="text-[11px] text-muted-foreground">
															payload truncated for preview ({fmtBytes(m.size)} total)
														</div>
													{/if}
												</div>
											{/if}
										</li>
									{/each}
								</ul>

								{#if nextBefore !== null}
									<div class="border-t border-border px-3 py-2 text-center">
										<Button
											variant="ghost"
											size="sm"
											disabled={msgLoading}
											onclick={() => void loadMessages(false)}
										>
											Load older
										</Button>
									</div>
								{/if}
							{/if}
						</div>
					</div>
				{/if}
			</div>
		</div>
	{/if}
</PageShell>
