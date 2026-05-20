<script lang="ts">
	import { Sheet, SheetContent, SheetTitle, SheetDescription, SheetClose } from '$lib/components/ui/sheet';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Label } from '$lib/components/ui/label';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import * as FileDropZone from '$lib/components/ui/file-drop-zone';
	import X from '@lucide/svelte/icons/x';
	import { getTemplate, createInstance, uploadFile } from '$lib/api/client';
	import type { WorkflowGraph, WorkflowNodeData, StartNodeData } from '$lib/types/editor';
	import type { components } from '$lib/api/schema';

	type Port = components['schemas']['Port'];
	type PortField = components['schemas']['PortField'];
	type StartToken = components['schemas']['StartToken'];

	/** Shape stored in the start token for a `file` field. `url` is the
	 *  relative download path so a template author can reference it directly
	 *  from an image/pdf/download block in a downstream human task. */
	type StartFileValue = {
		key: string;
		url: string;
		filename: string;
		content_type: string;
		size: number;
	};

	type Props = {
		open: boolean;
		templateId: string | null;
		oncreated: (instanceId: string) => void;
	};

	let { open = $bindable(), templateId, oncreated }: Props = $props();

	function close() {
		open = false;
	}

	// Per-Start, per-field values. Outer key is start_block_id, inner is field name.
	let values = $state<Record<string, Record<string, unknown>>>({});
	let starts = $state<{ id: string; label: string; initial: Port }[]>([]);
	let loadingTemplate = $state(false);
	let submitting = $state(false);
	let error = $state<string | null>(null);

	$effect(() => {
		if (!open || !templateId) {
			starts = [];
			values = {};
			error = null;
			return;
		}
		void loadTemplate(templateId);
	});

	async function loadTemplate(id: string) {
		loadingTemplate = true;
		error = null;
		try {
			const tmpl = await getTemplate(id);
			const graph = tmpl.graph as WorkflowGraph;
			const startNodes: { id: string; label: string; initial: Port }[] = [];
			const seed: Record<string, Record<string, unknown>> = {};
			for (const node of graph.nodes ?? []) {
				if ((node.data as WorkflowNodeData)?.type !== 'start') continue;
				const data = node.data as StartNodeData;
				const initial: Port = data.initial ?? { id: 'in', label: 'Input', fields: [] };
				startNodes.push({
					id: node.id,
					label: data.label ?? node.id,
					initial
				});
				const fieldSeed: Record<string, unknown> = {};
				for (const f of initial.fields ?? []) {
					fieldSeed[f.name] = defaultForKind(f);
				}
				seed[node.id] = fieldSeed;
			}
			starts = startNodes;
			values = seed;

			// No Start has typed fields → skip dialog entirely.
			const anyFields = startNodes.some((s) => (s.initial.fields ?? []).length > 0);
			if (!anyFields) {
				await submitDirect();
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load template';
		} finally {
			loadingTemplate = false;
		}
	}

	function defaultForKind(f: PortField): unknown {
		switch (f.kind) {
			case 'number':
				return 0;
			case 'bool':
				return false;
			case 'file':
				return null;
			default:
				return '';
		}
	}

	function updateValue(startId: string, fieldName: string, v: unknown) {
		values = {
			...values,
			[startId]: { ...(values[startId] ?? {}), [fieldName]: v }
		};
	}

	// Per-field upload state, keyed by `${startId}:${fieldName}`.
	let uploadError = $state<Record<string, string>>({});

	async function handleFileUpload(startId: string, fieldName: string, files: File[]) {
		const file = files[0];
		if (!file || !templateId) return;
		const errKey = `${startId}:${fieldName}`;
		try {
			// The Start node id doubles as the upload's node scope, so the S3
			// key lands under this template/node like any other staged asset.
			const resp = await uploadFile(templateId, startId, file);
			const value: StartFileValue = {
				key: resp.key,
				url: `/api/files/${resp.key}`,
				filename: resp.filename,
				content_type: resp.content_type,
				size: resp.size
			};
			updateValue(startId, fieldName, value);
			const { [errKey]: _, ...rest } = uploadError;
			uploadError = rest;
		} catch (e) {
			uploadError = {
				...uploadError,
				[errKey]: e instanceof Error ? e.message : 'Upload failed'
			};
		}
	}

	function clearFile(startId: string, fieldName: string) {
		updateValue(startId, fieldName, null);
	}

	// Fallback when a `file` field doesn't declare `accept`.
	const DEFAULT_FILE_ACCEPT =
		'image/*,application/pdf,.csv,.json,.txt,.zip,.docx,.xlsx,.pptx';

	// Turn an HTML `accept` list into a short human label, e.g.
	// "image/png,image/jpeg,.pdf" -> "PNG, JPG, PDF".
	function formatAccept(accept: string): string {
		const seen = new Set<string>();
		const labels: string[] = [];
		for (const raw of accept.split(',')) {
			const t = raw.trim().toLowerCase();
			if (!t) continue;
			let label: string;
			if (t === 'image/*') label = 'images';
			else if (t === 'video/*') label = 'video';
			else if (t === 'audio/*') label = 'audio';
			else if (t.includes('/')) {
				const sub = t.split('/')[1] ?? t;
				label = sub === 'jpeg' ? 'JPG' : sub.split('+')[0].toUpperCase();
			} else {
				const ext = t.replace(/^\./, '');
				label = ext === 'jpeg' ? 'JPG' : ext.toUpperCase();
			}
			const key = label.toLowerCase();
			if (seen.has(key)) continue;
			seen.add(key);
			labels.push(label);
		}
		return labels.join(', ');
	}

	function buildStartTokens(): StartToken[] {
		return starts.map((s) => ({
			start_block_id: s.id,
			token: { ...(values[s.id] ?? {}) }
		}));
	}

	async function submit() {
		if (!templateId) return;
		submitting = true;
		error = null;
		try {
			const instance = await createInstance({
				template_id: templateId,
				start_tokens: buildStartTokens()
			});
			oncreated(instance.id);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create instance';
		} finally {
			submitting = false;
		}
	}

	async function submitDirect() {
		// Same as submit() but used for the no-typed-fields fast path. The dialog
		// closes itself once oncreated fires.
		await submit();
	}
</script>

<Sheet.Root bind:open>
	<SheetContent class="w-[480px] sm:max-w-[480px]">
		<div class="flex items-center justify-between border-b border-border px-5 py-4">
			<div>
				<SheetTitle class="text-lg font-semibold">Create instance</SheetTitle>
				<SheetDescription class="text-sm text-muted-foreground">
					Provide initial token values for each Start block.
				</SheetDescription>
			</div>
			<SheetClose>
				<X class="size-4" />
			</SheetClose>
		</div>

		<div class="flex-1 overflow-y-auto px-5 py-4">
			{#if loadingTemplate}
				<p class="text-sm text-muted-foreground">Loading template…</p>
			{:else if error}
				<p class="text-sm text-destructive">{error}</p>
			{:else if starts.length === 0}
				<p class="text-sm text-muted-foreground">No Start blocks found in this template.</p>
			{:else}
				<div class="space-y-5">
					{#each starts as start (start.id)}
						<div class="rounded-md border border-border/50 p-3">
							<h3 class="mb-2 text-sm font-medium">{start.label}</h3>
							{#if (start.initial.fields ?? []).length === 0}
								<p class="text-sm text-muted-foreground">
									This Start has no declared fields — will seed with an empty token.
								</p>
							{:else}
								<div class="space-y-3">
									{#each start.initial.fields ?? [] as field (field.name)}
										<FormField label={field.label} description={field.description ?? undefined}>
											{#if field.kind === 'textarea' || field.kind === 'json'}
												<Textarea
													value={String(values[start.id]?.[field.name] ?? '')}
													oninput={(e) =>
														updateValue(
															start.id,
															field.name,
															(e.currentTarget as HTMLTextAreaElement).value
														)}
												/>
											{:else if field.kind === 'number'}
												<Input
													type="number"
													value={String(values[start.id]?.[field.name] ?? 0)}
													oninput={(e) =>
														updateValue(
															start.id,
															field.name,
															Number((e.currentTarget as HTMLInputElement).value)
														)}
												/>
											{:else if field.kind === 'bool'}
												<Checkbox
													checked={Boolean(values[start.id]?.[field.name])}
													onCheckedChange={(v) =>
														updateValue(start.id, field.name, v === true)}
												/>
											{:else if field.kind === 'select'}
												{@const selected = String(
													values[start.id]?.[field.name] ?? ''
												)}
												<Select.Root
													type="single"
													value={selected}
													onValueChange={(v) =>
														updateValue(start.id, field.name, v ?? '')}
												>
													<Select.Trigger class="w-full">
														{selected || '— select —'}
													</Select.Trigger>
													<Select.Content>
														{#each field.options ?? [] as opt (opt)}
															<Select.Item value={opt} label={opt} />
														{/each}
													</Select.Content>
												</Select.Root>
											{:else if field.kind === 'file'}
												{@const fileVal = (values[start.id]?.[field.name] ??
													null) as StartFileValue | null}
												{@const errKey = `${start.id}:${field.name}`}
												{@const acceptSpec = field.accept || DEFAULT_FILE_ACCEPT}
												<FileDropZone.Root
													accept={acceptSpec}
													maxFiles={1}
													fileCount={fileVal ? 1 : 0}
													onUpload={(files) =>
														handleFileUpload(start.id, field.name, files)}
													onFileRejected={({ reason }) =>
														(uploadError = { ...uploadError, [errKey]: reason })}
												>
													<FileDropZone.Trigger />
												</FileDropZone.Root>
												<p class="mt-1 text-sm text-muted-foreground">
													Accepted formats: {formatAccept(acceptSpec)}
												</p>
												{#if fileVal}
													<div
														class="mt-2 flex items-center gap-2 text-sm text-muted-foreground"
													>
														<span class="truncate">{fileVal.filename}</span>
														<Button
															variant="ghost"
															size="sm"
															type="button"
															class="h-auto px-1 py-0 text-sm text-destructive hover:text-destructive hover:underline"
															onclick={() => clearFile(start.id, field.name)}
														>
															remove
														</Button>
													</div>
												{/if}
												{#if uploadError[errKey]}
													<p class="mt-1 text-sm text-destructive">
														{uploadError[errKey]}
													</p>
												{/if}
											{:else}
												<Input
													type="text"
													value={String(values[start.id]?.[field.name] ?? '')}
													oninput={(e) =>
														updateValue(
															start.id,
															field.name,
															(e.currentTarget as HTMLInputElement).value
														)}
												/>
											{/if}
										</FormField>
									{/each}
								</div>
							{/if}
						</div>
					{/each}
				</div>
			{/if}
		</div>

		<div class="flex items-center justify-end gap-2 border-t border-border px-5 py-3">
			<Button variant="outline" onclick={close}>Cancel</Button>
			<Button onclick={submit} disabled={submitting || loadingTemplate}>
				{submitting ? 'Creating…' : 'Create instance'}
			</Button>
		</div>
	</SheetContent>
</Sheet.Root>
