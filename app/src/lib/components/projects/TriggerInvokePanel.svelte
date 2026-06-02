<script lang="ts">
	import type { ManualEndpoint } from '$lib/api/openapi-bundle';
	import { fireTrigger, invokeTrigger, type TriggerCallResult } from '$lib/api/client';
	import { ApiError } from '$lib/api/client';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Spinner } from '$lib/components/ui/spinner';
	import Play from '@lucide/svelte/icons/play';
	import Send from '@lucide/svelte/icons/send';

	type Props = { endpoint: ManualEndpoint };
	let { endpoint }: Props = $props();

	// One value slot per non-file field; one File slot per file field.
	let values = $state<Record<string, unknown>>({});
	let files = $state<Record<string, File | undefined>>({});

	let busy = $state<null | 'fire' | 'invoke'>(null);
	let result = $state<TriggerCallResult | null>(null);
	let errorMsg = $state<string | null>(null);

	function setValue(name: string, v: unknown) {
		values = { ...values, [name]: v };
	}
	function setFile(name: string, f: File | undefined) {
		files = { ...files, [name]: f };
	}

	/** Coerce a raw input value to its declared JSON type. */
	function coerce(field: ManualEndpoint['fields'][number], raw: unknown): unknown {
		if (field.type === 'number') {
			const n = Number(raw);
			return Number.isFinite(n) ? n : raw;
		}
		if (field.type === 'boolean') return !!raw;
		if (field.type === 'object') {
			if (typeof raw !== 'string' || raw.trim() === '') return undefined;
			try {
				return JSON.parse(raw);
			} catch {
				return raw; // let the server reject it with a real message
			}
		}
		return raw;
	}

	function buildPayload(): Record<string, unknown> {
		const out: Record<string, unknown> = {};
		for (const f of endpoint.fields) {
			if (f.isFile) continue; // files travel as multipart parts
			const raw = values[f.name];
			if (f.type === 'boolean') {
				out[f.name] = !!raw;
				continue;
			}
			if (raw === undefined || raw === '') continue; // omit empties
			out[f.name] = coerce(f, raw);
		}
		return out;
	}

	function buildFiles(): Record<string, File> {
		const out: Record<string, File> = {};
		for (const [name, f] of Object.entries(files)) if (f) out[name] = f;
		return out;
	}

	async function run(kind: 'fire' | 'invoke') {
		busy = kind;
		result = null;
		errorMsg = null;
		try {
			const payload = buildPayload();
			const fileMap = buildFiles();
			const call = kind === 'fire' ? fireTrigger : invokeTrigger;
			result = await call(endpoint.nodeId, payload, fileMap);
		} catch (e) {
			if (e instanceof ApiError) {
				const body = e.body as Record<string, unknown> | string;
				errorMsg =
					typeof body === 'object' && body && 'error' in body
						? String((body as Record<string, unknown>).error)
						: `HTTP ${e.status}`;
			} else {
				errorMsg = e instanceof Error ? e.message : 'Request failed';
			}
		} finally {
			busy = null;
		}
	}

	const resultLabel = $derived.by(() => {
		if (!result) return '';
		if (result.status === 200) return 'Completed (200)';
		if (result.status === 202) return 'Accepted — still running (202)';
		return `HTTP ${result.status}`;
	});
</script>

<div class="space-y-3">
	{#if endpoint.fields.length === 0}
		<p class="text-sm text-muted-foreground">No declared input fields — send an empty body.</p>
	{/if}

	{#each endpoint.fields as field (field.name)}
		<div class="space-y-1">
			<label class="flex items-center gap-1.5 text-sm font-medium" for={`f-${endpoint.nodeId}-${field.name}`}>
				{field.name}
				<span class="text-xs font-normal text-muted-foreground">
					({field.isFile ? 'file' : field.type}{#if field.format}, {field.format}{/if})
				</span>
				{#if field.required}<span class="text-destructive">*</span>{/if}
			</label>

			{#if field.isFile}
				<input
					id={`f-${endpoint.nodeId}-${field.name}`}
					type="file"
					class="block w-full text-sm text-muted-foreground file:mr-3 file:rounded-md file:border-0 file:bg-muted file:px-3 file:py-1.5 file:text-sm file:font-medium hover:file:bg-muted/80"
					onchange={(e) => setFile(field.name, (e.currentTarget as HTMLInputElement).files?.[0])}
				/>
			{:else if field.type === 'boolean'}
				<div class="flex items-center gap-2">
					<Checkbox
						id={`f-${endpoint.nodeId}-${field.name}`}
						checked={!!values[field.name]}
						onCheckedChange={(v) => setValue(field.name, v)}
					/>
				</div>
			{:else if field.enum && field.enum.length > 0}
				<select
					id={`f-${endpoint.nodeId}-${field.name}`}
					class="h-9 w-full rounded-md border border-input bg-background px-3 text-sm"
					value={(values[field.name] as string) ?? ''}
					onchange={(e) => setValue(field.name, (e.currentTarget as HTMLSelectElement).value)}
				>
					<option value="" disabled>Select…</option>
					{#each field.enum as opt (opt)}
						<option value={opt}>{opt}</option>
					{/each}
				</select>
			{:else}
				<Input
					id={`f-${endpoint.nodeId}-${field.name}`}
					type={field.type === 'number' ? 'number' : 'text'}
					placeholder={field.type === 'object' ? '{ }  (JSON)' : field.description ?? ''}
					value={(values[field.name] as string) ?? ''}
					oninput={(e) => setValue(field.name, (e.currentTarget as HTMLInputElement).value)}
				/>
			{/if}
		</div>
	{/each}

	<div class="flex flex-wrap gap-2 pt-1">
		{#if endpoint.invokePath}
			<Button size="sm" disabled={busy !== null} onclick={() => run('invoke')}>
				{#if busy === 'invoke'}<Spinner class="size-3.5" />{:else}<Send class="size-3.5" />{/if}
				Invoke (sync)
			</Button>
		{/if}
		{#if endpoint.firePath}
			<Button size="sm" variant="outline" disabled={busy !== null} onclick={() => run('fire')}>
				{#if busy === 'fire'}<Spinner class="size-3.5" />{:else}<Play class="size-3.5" />{/if}
				Fire (async)
			</Button>
		{/if}
	</div>

	{#if errorMsg}
		<div class="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
			{errorMsg}
		</div>
	{/if}

	{#if result}
		<div class="space-y-1">
			<span class="text-xs font-medium uppercase tracking-wide text-muted-foreground">
				{resultLabel}
			</span>
			<pre class="max-h-64 overflow-auto rounded-md border border-border/60 bg-muted/20 p-2 font-mono text-sm leading-relaxed">{JSON.stringify(result.body, null, 2)}</pre>
		</div>
	{/if}
</div>
