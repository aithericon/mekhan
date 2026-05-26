<script lang="ts">
	import type * as Y from 'yjs';
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import InsertRefButton from '../InsertRefButton.svelte';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
		templateId?: string;
		scope?: ScopeEntry[];
	};

	let {
		config,
		readonly = false,
		onchange,
		binding,
		nodeId,
		templateId,
		scope = []
	}: Props = $props();

	const auth = $derived((config.auth as Record<string, unknown> | undefined) ?? null);
	const authType = $derived((auth?.type as string) ?? 'none');

	const methodLabels: Record<string, string> = {
		GET: 'GET',
		POST: 'POST',
		PUT: 'PUT',
		PATCH: 'PATCH',
		DELETE: 'DELETE',
		HEAD: 'HEAD',
		OPTIONS: 'OPTIONS'
	};

	const authLabels: Record<string, string> = {
		none: 'None',
		bearer: 'Bearer Token',
		basic: 'Basic Auth',
		header: 'Custom Header'
	};

	const responseModeLabels: Record<string, string> = {
		auto: 'Auto',
		json: 'JSON',
		text: 'Text',
		discard: 'Discard'
	};

	function patch(updates: Record<string, unknown>) {
		onchange({ ...config, ...updates });
	}

	function setAuthField(field: string, value: string) {
		const next = { ...(auth ?? { type: authType }), [field]: value };
		onchange({ ...config, auth: next });
	}

	function appendToUrl(snippet: string) {
		const curr = (config.url as string | undefined) ?? '';
		onchange({ ...config, url: curr ? `${curr}${snippet}` : snippet });
	}

	// --- Body file (IDE-edited) ---
	// Body editing routes through the collaborative IDE: the request body
	// lives as a node-attached file referenced by `body_from_input`, so the
	// user gets syntax highlighting, a full viewport, and ref insertion via
	// the same surface they author Python in. Inline `body` is not exposed
	// in the editor (the executor still accepts it, but mutual exclusion
	// with `body_from_input` is enforced by the compiler).

	const bodyFile = $derived((config.body_from_input as string | undefined) ?? '');

	const nodeFiles: Map<string, Y.Text> = $derived(
		binding && nodeId ? binding.getNodeFiles(nodeId) : new Map()
	);

	const bodyFileMissing = $derived(
		Boolean(bodyFile) && nodeFiles.size > 0 && !nodeFiles.has(bodyFile)
	);

	function bodyFileHref(filename: string): string | null {
		if (!templateId || !nodeId) return null;
		const params = new URLSearchParams({
			node: nodeId,
			file: `${nodeId}:${filename}`
		});
		return `/templates/${templateId}/ide?${params.toString()}`;
	}

	function handleAddBody() {
		if (!binding || !nodeId) return;
		const name = prompt('Body file name:', 'body.json');
		if (!name) return;
		if (nodeFiles.has(name)) {
			alert(`File ${name} already exists on this node.`);
			return;
		}
		binding.createFile(nodeId, name, '');
		// Drop any leftover inline body when switching to file-backed — the
		// compiler rejects both being set simultaneously.
		const next: Record<string, unknown> = { ...config, body_from_input: name };
		delete next.body;
		onchange(next);
	}

	function handleRemoveBody() {
		if (!bodyFile) return;
		if (!confirm(`Detach ${bodyFile} as the request body?`)) return;
		const next: Record<string, unknown> = { ...config };
		delete next.body_from_input;
		onchange(next);
		// The file itself is left in place — the user can delete it from the
		// IDE FileTree if they want it gone.
	}
</script>

<div class="space-y-1.5">
	<FormField label="Method" for="http-method">
		<Select.Root
			type="single"
			value={(config.method as string) ?? 'GET'}
			onValueChange={(v) => {
				if (v) patch({ method: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="http-method">
				{methodLabels[(config.method as string) ?? 'GET'] ?? 'GET'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="GET" label="GET" />
				<Select.Item value="POST" label="POST" />
				<Select.Item value="PUT" label="PUT" />
				<Select.Item value="PATCH" label="PATCH" />
				<Select.Item value="DELETE" label="DELETE" />
				<Select.Item value="HEAD" label="HEAD" />
				<Select.Item value="OPTIONS" label="OPTIONS" />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<div class="space-y-1.5">
	<FormField label="URL" for="http-url">
		<Input
			id="http-url"
			type="text"
			value={(config.url as string) ?? ''}
			placeholder={'https://api.example.com/{{ slug.field }}'}
			disabled={readonly}
			oninput={(e) => patch({ url: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			placeholder="Insert ref into URL…"
			oninsert={(s) => appendToUrl(s)}
		/>
	{/if}
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Headers</span>
	<KeyValueEditor
		entries={(config.headers as Record<string, unknown>) ?? {}}
		{readonly}
		{scope}
		keyPlaceholder="Header"
		valuePlaceholder="Value"
		onchange={(headers) => patch({ headers })}
	/>
</div>

<div class="space-y-1.5">
	<span class="text-sm font-medium text-muted-foreground">Query Parameters</span>
	<KeyValueEditor
		entries={(config.query as Record<string, unknown>) ?? {}}
		{readonly}
		{scope}
		keyPlaceholder="Param"
		valuePlaceholder="Value"
		onchange={(query) => patch({ query })}
	/>
</div>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Request Body</span>
		{#if !readonly && !bodyFile && binding && nodeId}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
				onclick={handleAddBody}
				data-testid="http-add-body"
			>
				<Plus class="size-3" />
				Add body file
			</button>
		{/if}
	</div>

	{#if bodyFile}
		{@const href = bodyFileHref(bodyFile)}
		<div
			class="flex items-center gap-1 rounded border border-border px-2 py-1 text-sm"
			class:border-destructive={bodyFileMissing}
		>
			{#if href}
				<a
					{href}
					class="flex flex-1 items-center gap-1 truncate font-mono text-foreground hover:underline"
					title="Open {bodyFile} in IDE"
					data-testid="http-body-link"
				>
					<span class="truncate">{bodyFile}</span>
					<ExternalLink class="size-3 shrink-0" />
				</a>
			{:else}
				<span class="flex-1 truncate font-mono" data-testid="http-body-name">{bodyFile}</span>
			{/if}
			{#if !readonly}
				<button
					type="button"
					class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
					onclick={handleRemoveBody}
					title="Detach body file"
					data-testid="http-body-detach"
				>
					<Trash2 class="size-3" />
				</button>
			{/if}
		</div>
		{#if bodyFileMissing}
			<p class="text-sm text-destructive">
				Body file <code class="font-mono">{bodyFile}</code> isn't attached to this
				node. Detach and re-add, or create the file from the IDE FileTree.
			</p>
		{:else}
			<p class="text-sm italic text-muted-foreground">
				Edited in the IDE — full editor, syntax highlighting, and ref insertion.
			</p>
		{/if}
	{:else if !binding || !nodeId}
		<p class="text-sm italic text-muted-foreground">
			Body editing requires the collaborative graph binding.
		</p>
	{:else}
		<p class="text-sm italic text-muted-foreground">
			No body attached. POST/PUT/PATCH typically need one — click <em>Add body file</em>.
		</p>
	{/if}
</div>

<!-- Auth -->
<div class="space-y-1.5">
	<FormField label="Authentication" for="http-auth-type">
		<Select.Root
			type="single"
			value={authType}
			onValueChange={(v) => {
				if (!v) return;
				if (v === 'none') {
					const next: Record<string, unknown> = { ...config };
					delete next.auth;
					onchange(next);
				} else {
					// Switching scheme resets auth-specific fields so stale data
					// (e.g. basic.username) doesn't leak into the next config.
					onchange({ ...config, auth: { type: v } });
				}
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="http-auth-type">
				{authLabels[authType] ?? 'None'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="none" label="None" />
				<Select.Item value="bearer" label="Bearer Token" />
				<Select.Item value="basic" label="Basic Auth" />
				<Select.Item value="header" label="Custom Header" />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

{#if auth && authType === 'bearer'}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<FormField label="Token" for="http-auth-bearer-token">
			<Input
				id="http-auth-bearer-token"
				type="password"
				value={(auth.token as string) ?? ''}
				placeholder="Leave empty to use env var"
				disabled={readonly}
				oninput={(e) => setAuthField('token', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
			/>
		</FormField>
		<FormField label="Or env var name" for="http-auth-bearer-env">
			<Input
				id="http-auth-bearer-env"
				type="text"
				value={(auth.token_env as string) ?? ''}
				placeholder="e.g. API_TOKEN"
				disabled={readonly}
				oninput={(e) => setAuthField('token_env', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
			/>
		</FormField>
	</div>
{:else if auth && authType === 'basic'}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<FormField label="Username" for="http-auth-basic-user">
			<Input
				id="http-auth-basic-user"
				type="text"
				value={(auth.username as string) ?? ''}
				disabled={readonly}
				oninput={(e) => setAuthField('username', (e.currentTarget as HTMLInputElement).value)}
			/>
		</FormField>
		<FormField label="Password" for="http-auth-basic-pass">
			<Input
				id="http-auth-basic-pass"
				type="password"
				value={(auth.password as string) ?? ''}
				placeholder="Leave empty to use env var"
				disabled={readonly}
				oninput={(e) => setAuthField('password', (e.currentTarget as HTMLInputElement).value)}
			/>
		</FormField>
		<FormField label="Or password env var" for="http-auth-basic-pass-env">
			<Input
				id="http-auth-basic-pass-env"
				type="text"
				value={(auth.password_env as string) ?? ''}
				placeholder="e.g. API_PASSWORD"
				disabled={readonly}
				oninput={(e) =>
					setAuthField('password_env', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
			/>
		</FormField>
	</div>
{:else if auth && authType === 'header'}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<FormField label="Header name" for="http-auth-header-name">
			<Input
				id="http-auth-header-name"
				type="text"
				value={(auth.name as string) ?? ''}
				placeholder="X-API-Key"
				disabled={readonly}
				oninput={(e) => setAuthField('name', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
			/>
		</FormField>
		<FormField label="Value" for="http-auth-header-value">
			<Input
				id="http-auth-header-value"
				type="password"
				value={(auth.value as string) ?? ''}
				placeholder="Leave empty to use env var"
				disabled={readonly}
				oninput={(e) => setAuthField('value', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
			/>
		</FormField>
		<FormField label="Or value env var" for="http-auth-header-env">
			<Input
				id="http-auth-header-env"
				type="text"
				value={(auth.value_env as string) ?? ''}
				placeholder="e.g. API_KEY"
				disabled={readonly}
				oninput={(e) => setAuthField('value_env', (e.currentTarget as HTMLInputElement).value)}
				class="font-mono"
			/>
		</FormField>
	</div>
{/if}

<div class="space-y-1.5">
	<FormField label="Response Mode" for="http-response-mode">
		<Select.Root
			type="single"
			value={(config.response_mode as string) ?? 'auto'}
			onValueChange={(v) => {
				if (v) patch({ response_mode: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="http-response-mode">
				{responseModeLabels[(config.response_mode as string) ?? 'auto'] ?? 'Auto'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="auto" label="Auto" />
				<Select.Item value="json" label="JSON" />
				<Select.Item value="text" label="Text" />
				<Select.Item value="discard" label="Discard" />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<FormField label="Timeout (seconds)" for="http-timeout">
	<Input
		id="http-timeout"
		type="number"
		min={1}
		value={(config.timeout_secs as number) ?? ''}
		placeholder="Default"
		disabled={readonly}
		oninput={(e) => {
			const val = parseInt((e.currentTarget as HTMLInputElement).value);
			patch({ timeout_secs: isNaN(val) ? undefined : val });
		}}
	/>
</FormField>

<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
	<Checkbox
		checked={(config.follow_redirects as boolean) ?? true}
		disabled={readonly}
		onCheckedChange={(v) => patch({ follow_redirects: v })}
	/>
	Follow redirects
</label>
