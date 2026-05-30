<script lang="ts">
	/**
	 * FieldWidget — the single canonical kind→widget renderer.
	 *
	 * Exhaustive over all 12 canonical FieldKind values; an unmapped kind is
	 * caught at the {:else} branch which calls assertNever() (dev throws, prod
	 * logs + falls through to a plain text input).
	 *
	 * BOUNDARY: FieldWidget is presentation-only. It does NOT perform submit-time
	 * coercion (range/rating stay strings internally; number stays raw until the
	 * host's coerceFormData runs). Seeding is handled by defaultValueForKind in
	 * fields/spec.ts. Validation stays in task-form-values.svelte.ts.
	 *
	 * HOST-SPECIFIC QUIRKS are honored by hosts NOT delegating here for kinds
	 * they intentionally degrade (e.g. CreateInstanceDialog renders signature /
	 * timestamp as plain text and never calls FieldWidget for those kinds).
	 */

	import type { FieldSpec, SelectOption } from './spec';
	import type { FIELD_KINDS } from './kind';

	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import { Label } from '$lib/components/ui/label';
	import { Button } from '$lib/components/ui/button';
	import * as Select from '$lib/components/ui/select';
	import * as RadioGroup from '$lib/components/ui/radio-group';
	import * as RatingGroup from '$lib/components/ui/rating-group';
	import * as FileDropZone from '$lib/components/ui/file-drop-zone';
	import * as Popover from '$lib/components/ui/popover';
	import { Calendar } from '$lib/components/ui/calendar';
	import { SignaturePad } from '$lib/components/ui/signature-pad';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import { CalendarDate, getLocalTimeZone } from '@internationalized/date';

	// ── helpers shared with TaskForm (date parse/build) ───────────────────────
	// Inline lightweight versions so FieldWidget has no dependency on
	// task-form-values.svelte.ts (which is a host module, not a shared lib).

	function parseCalendarDate(str: string): CalendarDate | undefined {
		const datePart = str.split('T')[0];
		const m = datePart?.match(/^(\d{4})-(\d{2})-(\d{2})$/);
		if (!m) return undefined;
		return new CalendarDate(Number(m[1]), Number(m[2]), Number(m[3]));
	}

	function parseTimePart(str: string): string {
		const idx = str.indexOf('T');
		return idx >= 0 ? str.slice(idx + 1) : '';
	}

	function buildDateString(date: CalendarDate | undefined, time: string): string {
		if (!date) return '';
		const d = `${String(date.year).padStart(4, '0')}-${String(date.month).padStart(2, '0')}-${String(date.day).padStart(2, '0')}`;
		return time ? `${d}T${time}` : d;
	}

	// Normalize a mixed options array (bare strings or {value,label} objects)
	// into the canonical SelectOption shape.
	function normalizeOptions(raw: SelectOption[] | undefined): SelectOption[] {
		if (!raw) return [];
		return raw.map((o) => (typeof o === 'string' ? { value: o, label: o } : o));
	}

	// Dev-time exhaustiveness guard. Called in the {:else} fallback branch.
	// In dev it throws; in prod it logs and the caller degrades to a text input.
	function assertNever(kind: never): void {
		if (import.meta.env.DEV) {
			throw new Error(`FieldWidget: unhandled kind "${kind as string}"`);
		} else {
			console.error(`FieldWidget: unhandled kind "${kind as string}"`);
		}
	}

	// ── props ─────────────────────────────────────────────────────────────────

	type Props = {
		spec: FieldSpec;
		value: unknown;
		readonly?: boolean;
		onchange: (next: unknown) => void;
		/**
		 * 'checkbox' (default): render a native Checkbox.
		 * 'select': render a true/false string-model Select (ResourceEditModal
		 * parity — carries string values 'true'/'false', caller coerces at submit).
		 */
		booleanWidget?: 'checkbox' | 'select';
		/** When true render the field as a password input (json/text/textarea). */
		secret?: boolean;
		/** Placeholder shown in secret inputs (e.g. "(leave blank to keep current)"). */
		secretPlaceholder?: string;
		/**
		 * When true, number inputs emit parsed numbers (parseFloat / parseInt).
		 * When false (default), they emit raw strings — the host coerces at submit.
		 * Matches SchemaForm's `coerceNumbers` prop.
		 */
		coerceNumbers?: boolean;
	};

	let {
		spec,
		value,
		readonly = false,
		onchange,
		booleanWidget = 'checkbox',
		secret = false,
		secretPlaceholder,
		coerceNumbers = false
	}: Props = $props();

	// ── derived helpers ───────────────────────────────────────────────────────

	const strVal = $derived(
		typeof value === 'number' && Number.isFinite(value)
			? String(value)
			: typeof value === 'string'
				? value
				: ''
	);

	const opts = $derived(normalizeOptions(spec.options));

	// Date kind internal state
	let datePopoverOpen = $state(false);
	const calDate = $derived(strVal ? parseCalendarDate(strVal) : undefined);
	const timePart = $derived(spec.includeTime ? parseTimePart(strVal) : '');
</script>

{#if spec.kind === 'text'}
	<Input
		id={`field-${spec.name}`}
		type={secret ? 'password' : 'text'}
		class={spec.mono ? 'font-mono text-sm' : 'text-sm'}
		data-testid={spec.testid}
		value={strVal}
		placeholder={secret ? (secretPlaceholder ?? spec.placeholder) : spec.placeholder}
		disabled={readonly}
		oninput={(e) => onchange((e.currentTarget as HTMLInputElement).value)}
	/>

{:else if spec.kind === 'textarea'}
	<Textarea
		id={`field-${spec.name}`}
		data-testid={spec.testid}
		value={strVal}
		rows={spec.rows ?? 2}
		placeholder={spec.placeholder}
		disabled={readonly}
		oninput={(e) => {
			const v = (e.currentTarget as HTMLTextAreaElement).value;
			onchange(v);
		}}
	/>

{:else if spec.kind === 'number'}
	<Input
		id={`field-${spec.name}`}
		type={secret ? 'password' : 'number'}
		class="text-sm"
		min={spec.min}
		max={spec.max}
		step={spec.step}
		value={strVal !== '' ? strVal : ''}
		placeholder={secretPlaceholder}
		disabled={readonly}
		oninput={(e) => {
			const raw = (e.currentTarget as HTMLInputElement).value;
			if (!coerceNumbers) {
				onchange(raw);
				return;
			}
			if (raw === '') {
				onchange(undefined);
				return;
			}
			const n = spec.integer ? parseInt(raw, 10) : parseFloat(raw);
			onchange(Number.isFinite(n) ? n : raw);
		}}
	/>

{:else if spec.kind === 'bool'}
	{#if booleanWidget === 'select'}
		<!-- String-model select: emits 'true'/'false'; host coerces at submit. -->
		<Select.Root
			type="single"
			value={value === true || value === 'true' ? 'true' : value === false || value === 'false' ? 'false' : ''}
			onValueChange={(v) => onchange(v)}
			disabled={readonly}
		>
			<Select.Trigger class="w-full text-sm">
				{value === true || value === 'true'
					? 'True'
					: value === false || value === 'false'
						? 'False'
						: '— select —'}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="true" label="True" />
				<Select.Item value="false" label="False" />
			</Select.Content>
		</Select.Root>
	{:else}
		<Checkbox
			id={`field-${spec.name}`}
			checked={value === true}
			disabled={readonly}
			onCheckedChange={(v) => onchange(v === true)}
		/>
	{/if}

{:else if spec.kind === 'select'}
	{@const selected = typeof value === 'string' ? value : ''}
	<Select.Root
		type="single"
		value={selected}
		onValueChange={(v) => onchange(v ?? '')}
		disabled={readonly}
	>
		<Select.Trigger class="w-full text-sm">
			{opts.find((o) => o.value === selected)?.label ?? (selected || '— select —')}
		</Select.Trigger>
		<Select.Content>
			{#each opts as opt (opt.value)}
				<Select.Item value={opt.value} label={opt.label}>
					{opt.label}
				</Select.Item>
			{/each}
		</Select.Content>
	</Select.Root>

{:else if spec.kind === 'radio'}
	<RadioGroup.Root
		value={typeof value === 'string' ? value : ''}
		onValueChange={(v) => onchange(v)}
		class="flex flex-col gap-2 py-1"
		disabled={readonly}
	>
		{#each opts as opt, i (opt.value)}
			{@const optId = `field-${spec.name}-${i}`}
			<div
				class="flex items-center space-x-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-muted/50"
			>
				<RadioGroup.Item value={opt.value} id={optId} />
				<Label for={optId} class="cursor-pointer font-normal text-sm">{opt.label}</Label>
			</div>
		{/each}
	</RadioGroup.Root>

{:else if spec.kind === 'range'}
	{@const rangeMin = spec.min ?? 0}
	{@const rangeMax = spec.max ?? 100}
	{@const rangeStep = spec.step ?? 1}
	{@const displayVal = strVal || String(rangeMin)}
	<div class="flex max-w-sm items-center gap-3">
		<span class="text-sm text-muted-foreground">{rangeMin}</span>
		<input
			id={`field-${spec.name}`}
			type="range"
			min={rangeMin}
			max={rangeMax}
			step={rangeStep}
			class="flex-1 accent-primary"
			value={displayVal}
			disabled={readonly}
			oninput={(e) => onchange((e.currentTarget as HTMLInputElement).value)}
		/>
		<span class="text-sm text-muted-foreground">{rangeMax}</span>
		<span
			class="min-w-[2.5rem] rounded-md bg-muted/50 px-2 py-1 text-center text-sm font-medium"
		>
			{displayVal}
		</span>
	</div>

{:else if spec.kind === 'rating'}
	{@const maxRating = spec.maxRating ?? 5}
	{@const currentRating = typeof value === 'number' ? value : Number(value) || 0}
	<div class="flex items-center gap-1 py-1">
		<RatingGroup.Root
			value={currentRating}
			max={maxRating}
			onValueChange={(v) => onchange(v)}
			aria-label={spec.label ?? spec.name}
			disabled={readonly}
		>
			{#each Array(maxRating) as _, i (i)}
				<RatingGroup.Item index={i} />
			{/each}
		</RatingGroup.Root>
		{#if currentRating > 0}
			<span class="ml-2 text-sm text-muted-foreground">{currentRating}/{maxRating}</span>
		{/if}
	</div>

{:else if spec.kind === 'date'}
	<div class="flex gap-3">
		<Popover.Root bind:open={datePopoverOpen}>
			<Popover.Trigger>
				{#snippet child({ props: triggerProps })}
					<Button
						{...triggerProps}
						variant="outline"
						disabled={readonly}
						class="w-48 justify-between font-normal text-sm {!calDate
							? 'text-muted-foreground'
							: ''}"
					>
						{calDate
							? calDate.toDate(getLocalTimeZone()).toLocaleDateString()
							: 'Select date'}
						<ChevronDown class="size-4 opacity-50" />
					</Button>
				{/snippet}
			</Popover.Trigger>
			<Popover.Content class="w-auto overflow-hidden p-0" align="start">
				<Calendar
					type="single"
					value={calDate}
					captionLayout="dropdown"
					onValueChange={(v) => {
						const cd = v as CalendarDate | undefined;
						onchange(buildDateString(cd, spec.includeTime ? timePart || '00:00' : ''));
						datePopoverOpen = false;
					}}
				/>
			</Popover.Content>
		</Popover.Root>
		{#if spec.includeTime}
			<Input
				type="time"
				step="60"
				value={timePart || ''}
				disabled={readonly}
				class="w-28 appearance-none bg-background text-sm [&::-webkit-calendar-picker-indicator]:hidden [&::-webkit-calendar-picker-indicator]:appearance-none"
				oninput={(e) => {
					const t = (e.currentTarget as HTMLInputElement).value;
					onchange(buildDateString(calDate, t));
				}}
			/>
		{/if}
	</div>

{:else if spec.kind === 'file'}
	<FileDropZone.Root
		id={`field-${spec.name}`}
		accept={spec.accept}
		maxFiles={spec.maxFiles ?? 1}
		maxFileSize={spec.maxFileSize}
		disabled={readonly}
		onUpload={async (files) => { onchange(files); }}
		onFileRejected={({ reason }) => {
			// Host handles rejection display; widget just emits null.
			console.warn(`FieldWidget[file]: rejected — ${reason}`);
			onchange(null);
		}}
	>
		<FileDropZone.Trigger />
	</FileDropZone.Root>

{:else if spec.kind === 'signature'}
	<SignaturePad
		id={`field-${spec.name}`}
		value={strVal}
		penColor={spec.penColor}
		disabled={readonly}
		onchange={(val) => onchange(val)}
	/>

{:else if spec.kind === 'json'}
	<Textarea
		id={`field-${spec.name}`}
		value={strVal}
		rows={spec.rows ?? 4}
		placeholder={spec.placeholder ?? '{}'}
		disabled={readonly}
		class="font-mono text-sm"
		oninput={(e) => onchange((e.currentTarget as HTMLTextAreaElement).value)}
	/>

{:else}
	<!-- Dev-time guard: assertNever fires if a new FieldKind bypasses all branches. -->
	<!-- In prod this falls through to a plain text input so the UI doesn't break. -->
	{@const _exhaustive = (assertNever(spec.kind as never), '')}
	<Input
		type="text"
		class="text-sm"
		value={strVal}
		disabled={readonly}
		oninput={(e) => onchange((e.currentTarget as HTMLInputElement).value)}
	/>
{/if}
