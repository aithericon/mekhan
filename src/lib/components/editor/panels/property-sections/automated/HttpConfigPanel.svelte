<script lang="ts">
	import KeyValueEditor from '../../shared/KeyValueEditor.svelte';
	import CodeEditor from '../../shared/CodeEditor.svelte';
	import { Select, SelectTrigger, SelectContent, SelectItem } from '$lib/components/ui/select';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
	};

	let { config, readonly = false, onchange }: Props = $props();

	const auth = $derived((config.auth as Record<string, unknown>) ?? null);
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
</script>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Method</span>
	<Select.Root
		type="single"
		value={(config.method as string) ?? 'GET'}
		onValueChange={(v) => { if (v) onchange({ ...config, method: v }); }}
		disabled={readonly}
	>
		<SelectTrigger disabled={readonly}>
			{methodLabels[(config.method as string) ?? 'GET'] ?? 'GET'}
		</SelectTrigger>
		<SelectContent>
			<SelectItem value="GET" label="GET" />
			<SelectItem value="POST" label="POST" />
			<SelectItem value="PUT" label="PUT" />
			<SelectItem value="PATCH" label="PATCH" />
			<SelectItem value="DELETE" label="DELETE" />
			<SelectItem value="HEAD" label="HEAD" />
			<SelectItem value="OPTIONS" label="OPTIONS" />
		</SelectContent>
	</Select.Root>
</div>

<div class="space-y-1.5">
	<label for="http-url" class="text-xs font-medium text-muted-foreground">URL</label>
	<input
		id="http-url"
		type="text"
		value={(config.url as string) ?? ''}
		placeholder={'https://api.example.com/{{id}}'}
		disabled={readonly}
		oninput={(e) => onchange({ ...config, url: (e.currentTarget as HTMLInputElement).value })}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Headers</span>
	<KeyValueEditor
		entries={(config.headers as Record<string, unknown>) ?? {}}
		{readonly}
		keyPlaceholder="Header"
		valuePlaceholder="Value"
		onchange={(headers) => onchange({ ...config, headers })}
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Query Parameters</span>
	<KeyValueEditor
		entries={(config.query as Record<string, unknown>) ?? {}}
		{readonly}
		keyPlaceholder="Param"
		valuePlaceholder="Value"
		onchange={(query) => onchange({ ...config, query })}
	/>
</div>

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Request Body (JSON)</span>
	<CodeEditor
		value={config.body ? JSON.stringify(config.body, null, 2) : ''}
		language="json"
		{readonly}
		minHeight="60px"
		maxHeight="200px"
		onchange={(val) => {
			try {
				onchange({ ...config, body: val ? JSON.parse(val) : null });
			} catch {
				// Keep raw string if not valid JSON yet
			}
		}}
	/>
</div>

<!-- Auth -->
<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Authentication</span>
	<Select.Root
		type="single"
		value={authType}
		onValueChange={(v) => {
			if (!v) return;
			if (v === 'none') {
				const { auth: _, ...rest } = config;
				onchange(rest);
			} else {
				onchange({ ...config, auth: { type: v } });
			}
		}}
		disabled={readonly}
	>
		<SelectTrigger disabled={readonly}>
			{authLabels[authType] ?? 'None'}
		</SelectTrigger>
		<SelectContent>
			<SelectItem value="none" label="None" />
			<SelectItem value="bearer" label="Bearer Token" />
			<SelectItem value="basic" label="Basic Auth" />
			<SelectItem value="header" label="Custom Header" />
		</SelectContent>
	</Select.Root>
</div>

{#if auth && authType === 'bearer'}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<input
			type="text"
			value={(auth.token as string) ?? ''}
			placeholder="Token (or leave empty for env var)"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, token: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 font-mono text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		<input
			type="text"
			value={(auth.token_env as string) ?? ''}
			placeholder="Env var name (e.g. API_TOKEN)"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, token_env: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{:else if auth && authType === 'basic'}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<input
			type="text"
			value={(auth.username as string) ?? ''}
			placeholder="Username"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, username: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		<input
			type="text"
			value={(auth.password as string) ?? ''}
			placeholder="Password (or leave empty for env var)"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, password: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		<input
			type="text"
			value={(auth.password_env as string) ?? ''}
			placeholder="Password env var"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, password_env: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{:else if auth && authType === 'header'}
	<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
		<input
			type="text"
			value={(auth.name as string) ?? ''}
			placeholder="Header name (e.g. X-API-Key)"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, name: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		<input
			type="text"
			value={(auth.value as string) ?? ''}
			placeholder="Value (or leave empty for env var)"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, value: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		<input
			type="text"
			value={(auth.value_env as string) ?? ''}
			placeholder="Value env var"
			disabled={readonly}
			oninput={(e) =>
				onchange({ ...config, auth: { ...auth, value_env: (e.currentTarget as HTMLInputElement).value } })}
			class="w-full rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
	</div>
{/if}

<div class="space-y-1.5">
	<span class="text-xs font-medium text-muted-foreground">Response Mode</span>
	<Select.Root
		type="single"
		value={(config.response_mode as string) ?? 'auto'}
		onValueChange={(v) => { if (v) onchange({ ...config, response_mode: v }); }}
		disabled={readonly}
	>
		<SelectTrigger disabled={readonly}>
			{responseModeLabels[(config.response_mode as string) ?? 'auto'] ?? 'Auto'}
		</SelectTrigger>
		<SelectContent>
			<SelectItem value="auto" label="Auto" />
			<SelectItem value="json" label="JSON" />
			<SelectItem value="text" label="Text" />
			<SelectItem value="discard" label="Discard" />
		</SelectContent>
	</Select.Root>
</div>

<div class="space-y-1.5">
	<label for="http-timeout" class="text-xs font-medium text-muted-foreground"
		>Timeout (seconds)</label
	>
	<input
		id="http-timeout"
		type="number"
		min={1}
		value={(config.timeout_secs as number) ?? ''}
		placeholder="Default"
		disabled={readonly}
		oninput={(e) => {
			const val = parseInt((e.currentTarget as HTMLInputElement).value);
			onchange({ ...config, timeout_secs: isNaN(val) ? undefined : val });
		}}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<label class="flex items-center gap-1.5 text-xs text-muted-foreground">
	<input
		type="checkbox"
		checked={(config.follow_redirects as boolean) ?? true}
		disabled={readonly}
		onchange={(e) =>
			onchange({ ...config, follow_redirects: (e.currentTarget as HTMLInputElement).checked })}
		class="size-3.5 disabled:cursor-default disabled:opacity-70"
	/>
	Follow redirects
</label>
