<script lang="ts">
	// Render the SMTP backend's `outputs` map — the `{outcome, subject,
	// body_text_preview?, body_html_preview?}` shape `executor-smtp` produces.
	// Predicate registration lives in `./index.ts::matchesSentEmail`.
	//
	// Failure modes carry a structured `outcome.reason` (template_render,
	// connect_failed, auth_failed, recipient_rejected, tls_failed,
	// server_error, timeout, invalid_address, invalid_config,
	// attachment_error) — each gets a tailored detail block so operators
	// don't have to read the raw outcome JSON to understand what went wrong.

	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Mail from '@lucide/svelte/icons/mail';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import type { RendererProps } from './types';

	type SmtpOutcome = {
		type: string;
		message_id?: string | null;
		recipients?: string[];
		server_response?: string | null;
		dry_run?: boolean;
		file?: string;
		error?: string;
		host?: string;
		port?: number;
		field?: string;
		value?: string;
		message?: string;
		failed_recipients?: string[];
		code?: number;
		filename?: string;
	};

	type SmtpOutputs = {
		outcome: SmtpOutcome;
		subject?: string;
		body_text_preview?: string;
		body_html_preview?: string;
	};

	let { value }: RendererProps = $props();
	const outputs = $derived(value as SmtpOutputs);
	const outcome = $derived(outputs.outcome);

	const isSuccess = $derived(outcome.type === 'success');
	const isDryRun = $derived(outcome.type === 'success' && outcome.dry_run === true);

	let bodyMode = $state<'text' | 'html'>('text');
	let showRawOutcome = $state(false);
	let showHtmlSource = $state(false);

	const hasText = $derived(typeof outputs.body_text_preview === 'string');
	const hasHtml = $derived(typeof outputs.body_html_preview === 'string');

	// Default body mode: prefer html when present, fall back to text.
	$effect(() => {
		if (!hasText && hasHtml) bodyMode = 'html';
	});

	function reasonLabel(reason: string): string {
		switch (reason) {
			case 'success':
				return 'Sent';
			case 'template_render':
				return 'Template render error';
			case 'invalid_address':
				return 'Invalid address';
			case 'invalid_config':
				return 'Invalid config';
			case 'connect_failed':
				return 'Connect failed';
			case 'tls_failed':
				return 'TLS handshake failed';
			case 'auth_failed':
				return 'Authentication failed';
			case 'recipient_rejected':
				return 'Recipient rejected';
			case 'server_error':
				return 'Server error';
			case 'timeout':
				return 'Timed out';
			case 'attachment_error':
				return 'Attachment error';
			default:
				return reason;
		}
	}
</script>

<div class="space-y-3 rounded-lg border border-border bg-card p-3">
	<header class="flex items-start gap-2">
		<Mail class="mt-0.5 size-5 shrink-0 text-muted-foreground" />
		<div class="min-w-0 flex-1">
			<div class="flex flex-wrap items-center gap-2">
				{#if isSuccess}
					<Badge variant="default" class="gap-1 bg-emerald-600 text-white hover:bg-emerald-600">
						<CheckCircle2 class="size-3" />
						{isDryRun ? 'Rendered (dry run)' : 'Sent'}
					</Badge>
				{:else}
					<Badge variant="destructive" class="gap-1">
						<XCircle class="size-3" />
						{reasonLabel(outcome.type)}
					</Badge>
				{/if}
				{#if outcome.message_id}
					<span class="truncate font-mono text-sm text-muted-foreground" title="Message ID">
						{outcome.message_id}
					</span>
				{/if}
			</div>
			{#if outputs.subject}
				<p class="mt-1 truncate text-sm font-medium text-foreground" data-testid="smtp-rendered-subject">
					{outputs.subject}
				</p>
			{/if}
		</div>
	</header>

	{#if outcome.recipients && outcome.recipients.length > 0}
		<div class="space-y-1">
			<span class="text-sm font-medium text-muted-foreground">Recipients</span>
			<div class="flex flex-wrap gap-1">
				{#each outcome.recipients as r (r)}
					<Badge variant="secondary" class="font-mono text-sm">{r}</Badge>
				{/each}
			</div>
		</div>
	{/if}

	{#if hasText || hasHtml}
		<div class="space-y-1.5">
			<div class="flex items-center justify-between">
				<span class="text-sm font-medium text-muted-foreground">Body preview</span>
				{#if hasText && hasHtml}
					<div class="flex gap-1">
						<Button
							variant={bodyMode === 'text' ? 'default' : 'ghost'}
							size="sm"
							class="h-6 px-2 text-sm"
							onclick={() => (bodyMode = 'text')}
						>Text</Button>
						<Button
							variant={bodyMode === 'html' ? 'default' : 'ghost'}
							size="sm"
							class="h-6 px-2 text-sm"
							onclick={() => (bodyMode = 'html')}
						>HTML</Button>
					</div>
				{/if}
			</div>
			{#if bodyMode === 'text' && hasText}
				<pre class="max-h-64 overflow-auto whitespace-pre-wrap rounded border border-border bg-muted/40 p-2 font-mono text-sm">{outputs.body_text_preview}</pre>
			{:else if bodyMode === 'html' && hasHtml}
				<div class="space-y-1">
					<div class="flex items-center justify-between">
						<button
							type="button"
							class="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
							onclick={() => (showHtmlSource = !showHtmlSource)}
						>
							{#if showHtmlSource}
								<ChevronDown class="size-3" />
								Hide HTML source
							{:else}
								<ChevronRight class="size-3" />
								Show HTML source
							{/if}
						</button>
					</div>
					{#if showHtmlSource}
						<pre class="max-h-64 overflow-auto whitespace-pre-wrap rounded border border-border bg-muted/40 p-2 font-mono text-sm">{outputs.body_html_preview}</pre>
					{:else}
						<!-- Render the HTML inside an isolated iframe so author CSS
						     can't leak into the instance view chrome. -->
						<iframe
							title="Rendered email body"
							sandbox=""
							srcdoc={outputs.body_html_preview}
							class="h-64 w-full rounded border border-border bg-white"
						></iframe>
					{/if}
				</div>
			{/if}
		</div>
	{/if}

	{#if !isSuccess}
		<div class="space-y-1 rounded-md border border-destructive/30 bg-destructive/5 p-2">
			<span class="text-sm font-medium text-destructive">Failure detail</span>
			{#if outcome.type === 'template_render'}
				<p class="text-sm">
					<span class="font-mono text-muted-foreground">{outcome.file}:</span>
					{outcome.error}
				</p>
			{:else if outcome.type === 'invalid_address'}
				<p class="text-sm">
					<span class="font-mono text-muted-foreground">{outcome.field}:</span>
					"{outcome.value}" — {outcome.error}
				</p>
			{:else if outcome.type === 'invalid_config'}
				<p class="text-sm">{outcome.message}</p>
			{:else if outcome.type === 'connect_failed'}
				<p class="text-sm">
					Could not connect to <span class="font-mono">{outcome.host}:{outcome.port}</span> — {outcome.error}
				</p>
			{:else if outcome.type === 'tls_failed'}
				<p class="text-sm">{outcome.error}</p>
			{:else if outcome.type === 'auth_failed'}
				<p class="text-sm">
					SMTP AUTH rejected.
					{#if outcome.server_response}
						<span class="block font-mono text-sm">{outcome.server_response}</span>
					{/if}
				</p>
			{:else if outcome.type === 'recipient_rejected'}
				<p class="text-sm">Server rejected one or more recipients.</p>
				{#if outcome.failed_recipients && outcome.failed_recipients.length > 0}
					<div class="flex flex-wrap gap-1">
						{#each outcome.failed_recipients as r (r)}
							<Badge variant="outline" class="font-mono text-sm">{r}</Badge>
						{/each}
					</div>
				{/if}
				{#if outcome.server_response}
					<span class="block font-mono text-sm">{outcome.server_response}</span>
				{/if}
			{:else if outcome.type === 'server_error'}
				<p class="text-sm">
					{#if outcome.code}Code {outcome.code}.{/if}
					{outcome.server_response ?? ''}
				</p>
			{:else if outcome.type === 'attachment_error'}
				<p class="text-sm">
					<span class="font-mono text-muted-foreground">{outcome.filename}:</span>
					{outcome.error}
				</p>
			{:else if outcome.type === 'timeout'}
				<p class="text-sm">Send exceeded the run timeout.</p>
			{/if}
		</div>
	{/if}

	<div>
		<button
			type="button"
			class="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
			onclick={() => (showRawOutcome = !showRawOutcome)}
		>
			{#if showRawOutcome}
				<ChevronDown class="size-3" />
				Hide raw outcome
			{:else}
				<ChevronRight class="size-3" />
				Show raw outcome
			{/if}
		</button>
		{#if showRawOutcome}
			<pre class="mt-1 max-h-48 overflow-auto whitespace-pre rounded border border-border bg-muted/40 p-2 font-mono text-sm">{JSON.stringify(outcome, null, 2)}</pre>
		{/if}
	</div>
</div>
