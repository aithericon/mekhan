<script lang="ts">
	// SMTP automated-step config panel.
	//
	// Authoring surface:
	//  - Resource binding dropdown (workspace SMTP resources). The transport
	//    config (host/port/auth) lives on the resource, not on the step.
	//  - Recipients editor (To / Cc / Bcc). Each row is a single text input
	//    that accepts a Tera template (`{{ intake.email }}` or a bare addr).
	//  - Optional From override.
	//  - Subject template — required, single-line Tera source.
	//  - Body text + Body HTML templates — at least one required.
	//  - Attachments — list of `{filename, source_ref}`; source_ref is a
	//    Tera template referencing an upstream-step file artifact.
	//  - dry_run — render templates + assemble MIME but don't send. Useful
	//    for verifying the renderer before pointing at a real SMTP server.
	//
	// Wire shape: produces the `SmtpConfig` the mekhan compiler validates
	// (see `service/src/compiler/backend_configs.rs::Smtp` arm). The body /
	// subject sources are embedded inline so the executor doesn't need to
	// coordinate with node-file storage at run time.

	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { FormField } from '$lib/components/ui/form-field';
	import InsertRefButton from '../InsertRefButton.svelte';
	import ResourcePicker from '../shared/ResourcePicker.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { appendSnippet } from '$lib/editor/append-snippet';

	type TemplateSource = { label: string; source: string };
	type AttachmentSpec = { filename: string; input_name: string; mime?: string };

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		scope?: ScopeEntry[];
	};

	let { config, readonly = false, onchange, scope = [] }: Props = $props();

	// Typed projections. Defaults match the executor's SmtpConfig defaults
	// so partial drafts deserialize correctly when re-saving.
	const resourceAlias = $derived((config.resource_alias as string | undefined) ?? '');
	const to = $derived((config.to as string[] | undefined) ?? []);
	const cc = $derived((config.cc as string[] | undefined) ?? []);
	const bcc = $derived((config.bcc as string[] | undefined) ?? []);
	const fromOverride = $derived((config.from as string | undefined) ?? '');
	const subjectSrc = $derived((config.subject as TemplateSource | undefined)?.source ?? '');
	const bodyText = $derived(config.body_text as TemplateSource | undefined);
	const bodyHtml = $derived(config.body_html as TemplateSource | undefined);
	const attachments = $derived((config.attachments as AttachmentSpec[] | undefined) ?? []);
	const dryRun = $derived((config.dry_run as boolean | undefined) ?? false);

	function patch(updates: Record<string, unknown>) {
		onchange({ ...config, ...updates });
	}

	function setRecipients(field: 'to' | 'cc' | 'bcc', values: string[]) {
		patch({ [field]: values });
	}

	function updateRecipient(field: 'to' | 'cc' | 'bcc', idx: number, value: string) {
		const arr = [...((config[field] as string[]) ?? [])];
		arr[idx] = value;
		setRecipients(field, arr);
	}

	function addRecipient(field: 'to' | 'cc' | 'bcc') {
		const arr = [...((config[field] as string[]) ?? []), ''];
		setRecipients(field, arr);
	}

	function removeRecipient(field: 'to' | 'cc' | 'bcc', idx: number) {
		const arr = ((config[field] as string[]) ?? []).filter((_, i) => i !== idx);
		setRecipients(field, arr);
	}

	function setSubjectSource(source: string) {
		const next: TemplateSource = {
			label: (config.subject as TemplateSource | undefined)?.label ?? 'subject.tera',
			source
		};
		patch({ subject: next });
	}

	function setBodyText(source: string | null) {
		if (source === null) {
			const { body_text: _unused, ...rest } = config as Record<string, unknown> & {
				body_text?: unknown;
			};
			onchange(rest);
			return;
		}
		const next: TemplateSource = {
			label: (config.body_text as TemplateSource | undefined)?.label ?? 'body.txt.tera',
			source
		};
		patch({ body_text: next });
	}

	function setBodyHtml(source: string | null) {
		if (source === null) {
			const { body_html: _unused, ...rest } = config as Record<string, unknown> & {
				body_html?: unknown;
			};
			onchange(rest);
			return;
		}
		const next: TemplateSource = {
			label: (config.body_html as TemplateSource | undefined)?.label ?? 'body.html.tera',
			source
		};
		patch({ body_html: next });
	}

	function setAttachments(next: AttachmentSpec[]) {
		patch({ attachments: next });
	}

	function addAttachment() {
		const idx = attachments.length;
		setAttachments([
			...attachments,
			{ filename: '', input_name: `_att_${idx}` }
		]);
	}

	function updateAttachment(idx: number, updates: Partial<AttachmentSpec>) {
		const next = [...attachments];
		next[idx] = { ...next[idx], ...updates };
		setAttachments(next);
	}

	function removeAttachment(idx: number) {
		setAttachments(attachments.filter((_, i) => i !== idx));
	}

	// SmtpConfigPanel uses sep='' (no space) — snippets are inserted directly
	// adjacent to the cursor position in Tera templates. The shared util
	// defaults to sep=' '; we pass sep='' explicitly here.
	function smtpAppend(target: string, snippet: string): string {
		return appendSnippet(target, snippet, '');
	}
</script>

<ResourcePicker
	resourceType="smtp"
	selected={resourceAlias}
	onChange={(v) => patch({ resource_alias: v })}
	label="SMTP resource"
	{readonly}
	testId="smtp-resource-select"
	typeLabel="SMTP"
/>

{#snippet recipientRow(field: 'to' | 'cc' | 'bcc', addr: string, idx: number)}
	<div class="flex items-center gap-1.5">
		<Input
			type="text"
			value={addr}
			placeholder={field === 'to' ? '{{ intake.email }}' : 'addr@example.com'}
			disabled={readonly}
			oninput={(e) =>
				updateRecipient(field, idx, (e.currentTarget as HTMLInputElement).value)}
			class="min-w-0 flex-1 font-mono"
			data-testid={`smtp-${field}-${idx}`}
		/>
		{#if scope.length > 0 && !readonly}
			<div class="w-32 shrink-0">
				<InsertRefButton
					{scope}
					disabled={readonly}
					placeholder="Insert ref…"
					oninsert={(s) => updateRecipient(field, idx, smtpAppend(addr, s))}
				/>
			</div>
		{/if}
		{#if !readonly}
			<button
				type="button"
				class="rounded p-1 text-muted-foreground hover:text-destructive"
				onclick={() => removeRecipient(field, idx)}
				title="Remove recipient"
			>
				<Trash2 class="size-3.5" />
			</button>
		{/if}
	</div>
{/snippet}

{#snippet recipientGroup(field: 'to' | 'cc' | 'bcc', label: string, list: string[])}
	<div class="space-y-1">
		<div class="flex items-center justify-between">
			<span class="text-sm font-medium text-muted-foreground">{label}</span>
			{#if !readonly}
				<button
					type="button"
					class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground"
					onclick={() => addRecipient(field)}
				>
					<Plus class="size-3" />
					Add
				</button>
			{/if}
		</div>
		{#if list.length === 0}
			<p class="text-sm italic text-muted-foreground">
				{field === 'to' ? 'At least one To: recipient required.' : 'None.'}
			</p>
		{:else}
			<div class="space-y-1">
				{#each list as addr, idx (idx)}
					{@render recipientRow(field, addr, idx)}
				{/each}
			</div>
		{/if}
	</div>
{/snippet}

{@render recipientGroup('to', 'To', to)}
{@render recipientGroup('cc', 'Cc', cc)}
{@render recipientGroup('bcc', 'Bcc', bcc)}

<div class="space-y-1.5">
	<FormField label="From (optional override)" for="smtp-from">
		<Input
			id="smtp-from"
			type="text"
			value={fromOverride}
			placeholder="Falls back to resource.from_address"
			disabled={readonly}
			oninput={(e) => patch({ from: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
		/>
	</FormField>
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert ref into From…"
			oninsert={(s) => patch({ from: smtpAppend(fromOverride, s) })}
		/>
	{/if}
</div>

<div class="space-y-1.5">
	<FormField label="Subject template (Tera)" for="smtp-subject">
		<Textarea
			id="smtp-subject"
			value={subjectSrc}
			placeholder={'Welcome, {{ intake.name }}!'}
			disabled={readonly}
			oninput={(e) => setSubjectSource((e.currentTarget as HTMLTextAreaElement).value)}
			class="min-h-[2.25rem] font-mono text-sm"
			rows={2}
			data-testid="smtp-subject"
		/>
	</FormField>
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert ref into Subject…"
			oninsert={(s) => setSubjectSource(smtpAppend(subjectSrc, s))}
		/>
	{/if}
</div>

<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Plain-text body (Tera)</span>
		<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
			<Checkbox
				checked={bodyText !== undefined}
				disabled={readonly}
				onCheckedChange={(v) => setBodyText(v ? '' : null)}
			/>
			Include
		</label>
	</div>
	{#if bodyText !== undefined}
		<Textarea
			value={bodyText.source}
			placeholder={'Hi {{ intake.name }},\nThanks for signing up.'}
			disabled={readonly}
			oninput={(e) => setBodyText((e.currentTarget as HTMLTextAreaElement).value)}
			class="min-h-[5rem] font-mono text-sm"
			rows={5}
			data-testid="smtp-body-text"
		/>
		{#if scope.length > 0 && !readonly}
			<InsertRefButton
				{scope}
				disabled={readonly}
				placeholder="Insert ref into text body…"
				oninsert={(s) => setBodyText(smtpAppend(bodyText.source, s))}
			/>
		{/if}
	{/if}
</div>

<div class="space-y-1.5 rounded-lg border border-border bg-muted/30 p-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">HTML body (Tera)</span>
		<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
			<Checkbox
				checked={bodyHtml !== undefined}
				disabled={readonly}
				onCheckedChange={(v) => setBodyHtml(v ? '' : null)}
			/>
			Include
		</label>
	</div>
	{#if bodyHtml !== undefined}
		<Textarea
			value={bodyHtml.source}
			placeholder={'<p>Hi {{ intake.name }},</p>\n<p>Thanks!</p>'}
			disabled={readonly}
			oninput={(e) => setBodyHtml((e.currentTarget as HTMLTextAreaElement).value)}
			class="min-h-[5rem] font-mono text-sm"
			rows={6}
			data-testid="smtp-body-html"
		/>
		{#if scope.length > 0 && !readonly}
			<InsertRefButton
				{scope}
				disabled={readonly}
				placeholder="Insert ref into HTML body…"
				oninsert={(s) => setBodyHtml(smtpAppend(bodyHtml.source, s))}
			/>
		{/if}
	{/if}
</div>

<div class="space-y-1.5">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Attachments</span>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-accent hover:text-foreground"
				onclick={addAttachment}
			>
				<Plus class="size-3" />
				Add
			</button>
		{/if}
	</div>
	{#if attachments.length === 0}
		<p class="text-sm italic text-muted-foreground">
			None. Each attachment references an upstream-step file artifact by name.
		</p>
	{:else}
		<div class="space-y-1.5">
			{#each attachments as att, idx (att.input_name)}
				<div class="space-y-1 rounded border border-border p-2">
					<FormField label="Filename (as recipient sees it)" for={`att-fn-${idx}`}>
						<Input
							id={`att-fn-${idx}`}
							type="text"
							value={att.filename}
							placeholder="report.pdf"
							disabled={readonly}
							oninput={(e) =>
								updateAttachment(idx, {
									filename: (e.currentTarget as HTMLInputElement).value
								})}
							class="font-mono"
						/>
					</FormField>
					<FormField label="Input name (staged ref)" for={`att-in-${idx}`}>
						<Input
							id={`att-in-${idx}`}
							type="text"
							value={att.input_name}
							placeholder="_att_0"
							disabled={readonly}
							oninput={(e) =>
								updateAttachment(idx, {
									input_name: (e.currentTarget as HTMLInputElement).value
								})}
							class="font-mono"
						/>
					</FormField>
					<FormField label="MIME type (optional)" for={`att-mime-${idx}`}>
						<Input
							id={`att-mime-${idx}`}
							type="text"
							value={att.mime ?? ''}
							placeholder="auto-detected from filename"
							disabled={readonly}
							oninput={(e) =>
								updateAttachment(idx, {
									mime: (e.currentTarget as HTMLInputElement).value || undefined
								})}
							class="font-mono"
						/>
					</FormField>
					{#if !readonly}
						<button
							type="button"
							class="flex items-center gap-1 rounded-md px-1.5 py-0.5 text-sm text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
							onclick={() => removeAttachment(idx)}
						>
							<Trash2 class="size-3" />
							Remove
						</button>
					{/if}
				</div>
			{/each}
		</div>
	{/if}
</div>

<label class="flex items-center gap-1.5 text-sm text-muted-foreground">
	<Checkbox
		checked={dryRun}
		disabled={readonly}
		onCheckedChange={(v) => patch({ dry_run: v === true })}
	/>
	Dry run (render templates + assemble MIME, do not send)
</label>
