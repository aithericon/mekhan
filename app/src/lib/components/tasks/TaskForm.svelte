<script lang="ts">
	import type { TaskStep, TaskField } from '$lib/hpi/types';
	import { BlockRenderer } from '$lib/hpi';
	import RepeaterBlock from './RepeaterBlock.svelte';
	import FieldWidget from '$lib/fields/FieldWidget.svelte';
	import { fromTaskFieldKind } from '$lib/fields/adapters';
	import * as Select from '$lib/components/ui/select';
	import { Input } from '$lib/components/ui/input';
	import { Checkbox } from '$lib/components/ui/checkbox';
	import * as FileDropZone from '$lib/components/ui/file-drop-zone';
	import { Button } from '$lib/components/ui/button';
	import { Label } from '$lib/components/ui/label';
	import * as RadioGroup from '$lib/components/ui/radio-group';
	import * as RatingGroup from '$lib/components/ui/rating-group';
	import { Calendar } from '$lib/components/ui/calendar';
	import * as Popover from '$lib/components/ui/popover';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import { CalendarDate, getLocalTimeZone } from '@internationalized/date';
	import { renderMdsvex } from '$lib/mdsvex';
	import { MDSVEX_CLASS } from '$lib/mdsvex-styles';
	import { toast } from 'svelte-sonner';
	import Check from '@lucide/svelte/icons/check';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import * as Stepper from '$lib/components/ui/stepper';
	import {
		getTextValue as _getTextValue,
		getCheckboxValue as _getCheckboxValue,
		getNumberValue as _getNumberValue,
		parseCalendarDate,
		parseTimePart,
		buildDateString,
		parseFileValue,
		validateFields,
		validateField,
		coerceFormData,
		fieldsForStep,
		parseRepeaterRef,
		getAtPath,
		asItemsArray
	} from './task-form-values.svelte.ts';
	import type { FieldSpec } from '$lib/fields/spec';

	/**
	 * Build a FieldSpec from a TaskField for delegation to FieldWidget.
	 * Only the kinds delegated (text, textarea, signature) need full spec
	 * coverage; the rest are documented for completeness.
	 *
	 * Critically: spec.testid is set to `field-${field.name}` — the EXACT
	 * selector that focusField() and e2e tests depend on.
	 */
	function toFieldSpec(field: TaskField): FieldSpec {
		return {
			kind: fromTaskFieldKind(field.kind),
			name: field.name,
			label: field.label,
			testid: `field-${field.name}`,
			placeholder: field.placeholder,
			required: field.required,
			options: field.options,
			rows: field.kind === 'textarea' ? 4 : undefined,
			penColor: field.pen_color,
			min: field.min,
			max: field.max,
			step: field.step,
			maxRating: field.max_rating,
			includeTime: field.include_time,
			maxFiles: field.max_files,
			maxFileSize: field.max_file_size,
			accept: field.accept
		};
	}

	interface Props {
		steps: TaskStep[];
		onsubmit: (data: Record<string, unknown>) => void;
		oncancel?: (reason?: string) => void;
		submitting?: boolean;
		/** Persist form draft (values + active step) to localStorage under this key. */
		taskId?: string;
		/**
		 * Feature B — upstream data the HumanTask request envelope carries
		 * for Repeater blocks. Looked up via the Repeater's `items_ref`
		 * pre-`[*]` path; the renderer iterates the resolved array. When
		 * omitted (or the path resolves to a non-array), Repeater blocks
		 * render an empty-state notice instead of crashing.
		 */
		taskData?: Record<string, unknown>;
	}

	let { steps, onsubmit, oncancel, submitting = false, taskId, taskData }: Props = $props();

	let formData: Record<string, unknown> = $state({});
	let errors: Record<string, string> = $state({});
	// 1-based to match Stepper.Root contract
	let activeStep = $state(1);
	let datePopoverOpen: Record<string, boolean> = $state({});

	const STORAGE_KEY = $derived(taskId ? `task-draft-${taskId}` : null);
	let draftLoaded = $state(false);

	$effect(() => {
		const key = STORAGE_KEY;
		if (!key || draftLoaded) return;
		draftLoaded = true;
		if (typeof localStorage === 'undefined') return;
		try {
			const saved = localStorage.getItem(key);
			if (!saved) return;
			const draft = JSON.parse(saved) as { formValues?: Record<string, unknown>; step?: number };
			if (draft.formValues && typeof draft.formValues === 'object') {
				formData = { ...draft.formValues };
			}
			if (typeof draft.step === 'number' && draft.step >= 1 && draft.step <= steps.length) {
				activeStep = draft.step;
			}
		} catch {
			localStorage.removeItem(key);
		}
	});

	$effect(() => {
		const key = STORAGE_KEY;
		if (!key || !draftLoaded) return;
		if (typeof localStorage === 'undefined') return;
		const snapshot = $state.snapshot(formData);
		const step = activeStep;
		const hasValues = Object.keys(snapshot).length > 0;
		if (!hasValues && step === 1) {
			localStorage.removeItem(key);
			return;
		}
		try {
			localStorage.setItem(key, JSON.stringify({ formValues: snapshot, step }));
		} catch {
			/* quota or disabled — silently drop */
		}
	});

	function clearDraft() {
		if (!STORAGE_KEY || typeof localStorage === 'undefined') return;
		localStorage.removeItem(STORAGE_KEY);
	}

	function setFieldErrorBound(name: string, message: string) {
		errors = { ...errors, [name]: message };
	}

	function clearFieldErrorBound(name: string) {
		if (!(name in errors)) return;
		const { [name]: _, ...rest } = errors;
		errors = rest;
	}

	function focusField(fieldName: string): void {
		queueMicrotask(() => {
			document
				.querySelector<HTMLElement>(`[data-testid="field-${fieldName}"]`)
				?.focus();
		});
	}

	/** Validate one step's fields. Returns the first invalid field name, or null. */
	function validateStep(stepIdx: number): string | null {
		const s = steps[stepIdx];
		if (!s) return null;
		const fieldFail = validateFields(
			fieldsForStep(s),
			formData,
			setFieldErrorBound,
			clearFieldErrorBound
		);
		if (fieldFail) return fieldFail;
		return validateRepeaters(s);
	}

	/** Walk forward from current step toward `targetStep` (1-based, exclusive).
	 *  Returns 1-based index of first failing step (and focuses its bad field),
	 *  or null if all steps in [activeStep, targetStep) validate. */
	function validateUpTo(targetStep: number): number | null {
		for (let s = activeStep; s < targetStep; s++) {
			const firstInvalid = validateStep(s - 1);
			if (firstInvalid) {
				focusField(firstInvalid);
				return s;
			}
		}
		return null;
	}

	// Value accessors bound to formData
	function getTextValue(name: string): string {
		return _getTextValue(formData, name);
	}
	function getCheckboxValue(name: string): boolean {
		return _getCheckboxValue(formData, name);
	}
	function getNumberValue(name: string): number {
		return _getNumberValue(formData, name);
	}
	function setTextValue(name: string, value: string) {
		formData = { ...formData, [name]: value };
		if (errors[name]) {
			const { [name]: _, ...rest } = errors;
			errors = rest;
		}
	}
	function setCheckboxValue(name: string, value: boolean) {
		formData = { ...formData, [name]: value };
		if (errors[name]) {
			const { [name]: _, ...rest } = errors;
			errors = rest;
		}
	}
	function setNumberValue(name: string, value: number) {
		formData = { ...formData, [name]: value };
		if (errors[name]) {
			const { [name]: _, ...rest } = errors;
			errors = rest;
		}
	}
	const currentStep = $derived(steps[activeStep - 1]);
	const isLastStep = $derived(activeStep === steps.length);
	const allFields = $derived(steps.flatMap((s) => fieldsForStep(s)));

	// ── Repeater (Feature B) helpers ─────────────────────────────
	// Repeater state lives at `formData[output_slug]: Array<Record<string, unknown>>`.
	// Per-row field errors use the key `<output_slug>.<row_index>.<field_name>`
	// so the existing focusField pattern keeps working for them.
	function repeaterRows(outputSlug: string): Record<string, unknown>[] {
		const v = formData[outputSlug];
		return Array.isArray(v) ? (v as Record<string, unknown>[]) : [];
	}

	function setRepeaterField(
		outputSlug: string,
		rowIndex: number,
		fieldName: string,
		value: unknown
	) {
		const rows = repeaterRows(outputSlug).slice();
		const cur = (rows[rowIndex] ?? {}) as Record<string, unknown>;
		rows[rowIndex] = { ...cur, [fieldName]: value };
		formData = { ...formData, [outputSlug]: rows };
		const errKey = `${outputSlug}.${rowIndex}.${fieldName}`;
		if (errors[errKey]) {
			const { [errKey]: _, ...rest } = errors;
			errors = rest;
		}
	}

	function getRepeaterFieldText(outputSlug: string, rowIndex: number, fieldName: string): string {
		const rows = repeaterRows(outputSlug);
		const v = rows[rowIndex]?.[fieldName];
		if (typeof v === 'number') return Number.isFinite(v) ? String(v) : '';
		return typeof v === 'string' ? v : '';
	}

	function getRepeaterFieldBool(outputSlug: string, rowIndex: number, fieldName: string): boolean {
		const rows = repeaterRows(outputSlug);
		return rows[rowIndex]?.[fieldName] === true;
	}

	/**
	 * Walk a step's Repeater blocks; for each block resolve the upstream
	 * items array from `taskData` and emit one synthetic field-error key
	 * per (row, sub-field) on validation failure. Returns the first
	 * invalid `<output_slug>.<row>.<field>` (mirrors validateFields'
	 * single-string return).
	 */
	function validateRepeaters(step: TaskStep): string | null {
		let first: string | null = null;
		for (const block of step.blocks) {
			if (block.type !== 'repeater') continue;
			const parsed = parseRepeaterRef(block.items_ref);
			if (!parsed) continue;
			const items = asItemsArray(getAtPath(taskData ?? {}, [parsed.head, ...parsed.pre]));
			const rows = repeaterRows(block.output_slug);
			// Only Input children contribute to the per-row schema —
			// display blocks (Mdsvex/Callout/Image/...) render but
			// have nothing to validate.
			const inputFields = block.blocks
				.filter((b): b is Extract<typeof b, { type: 'input' }> => b.type === 'input')
				.map((b) => b.field);
			for (let i = 0; i < items.length; i++) {
				const row = (rows[i] ?? {}) as Record<string, unknown>;
				for (const subField of inputFields) {
					const message = validateField(subField, row);
					const key = `${block.output_slug}.${i}.${subField.name}`;
					if (message) {
						setFieldErrorBound(key, message);
						if (!first) first = key;
					} else {
						clearFieldErrorBound(key);
					}
				}
			}
		}
		return first;
	}

	function repeaterBlocks(step: TaskStep) {
		return step.blocks.filter((b): b is Extract<typeof b, { type: 'repeater' }> =>
			b.type === 'repeater'
		);
	}

	function handleSubmit() {
		const firstInvalid = validateFields(
			allFields,
			formData,
			setFieldErrorBound,
			clearFieldErrorBound
		);
		// Repeater rows go through their own per-row validation. A failing
		// repeater behaves like a failing top-level field: jump to its step.
		let firstRepeaterInvalid: string | null = null;
		for (const step of steps) {
			const r = validateRepeaters(step);
			if (r && !firstRepeaterInvalid) firstRepeaterInvalid = r;
		}
		const blocker = firstInvalid ?? firstRepeaterInvalid;
		if (blocker) {
			// Jump to first step containing an error (field-level or
			// repeater-level — both live in `errors`).
			for (let i = 0; i < steps.length; i++) {
				const stepFields = fieldsForStep(steps[i]);
				const repBlocks = repeaterBlocks(steps[i]);
				const hasFieldErr = stepFields.some((f) => errors[f.name]);
				const hasRepErr = repBlocks.some((rb) =>
					Object.keys(errors).some((k) => k.startsWith(`${rb.output_slug}.`))
				);
				if (hasFieldErr || hasRepErr) {
					activeStep = i + 1;
					focusField(blocker);
					break;
				}
			}
			return;
		}
		clearDraft();
		onsubmit(coerceFormData(allFields, formData));
	}

	function goToNextStep() {
		if (activeStep >= steps.length) return;
		const firstInvalid = validateStep(activeStep - 1);
		if (firstInvalid) {
			focusField(firstInvalid);
			return;
		}
		activeStep += 1;
	}

	function goToPreviousStep() {
		activeStep = Math.max(1, activeStep - 1);
	}

	/** Stepper trigger click: backward = free, forward = validate intermediate steps. */
	function handleStepTriggerClick(target: number, event: MouseEvent) {
		if (target <= activeStep) return; // backward / same = free
		event.preventDefault(); // block Stepper's auto-select
		const failedAt = validateUpTo(target);
		activeStep = failedAt ?? target;
	}

	function handleCancel() {
		const reason = prompt('Reason for cancellation (optional):');
		if (reason === null) return;
		clearDraft();
		oncancel?.(reason || undefined);
	}

	// File upload handler.
	//
	// Disabled at runtime: the `/api/v1/files/upload/{template_id}/{node_id}`
	// route is template-scoped (used by the IDE to attach files to a specific
	// workflow node). At task-completion time TaskForm only has a `task_id`,
	// not the originating template/node ids, so it cannot call that route
	// correctly. Until a dedicated task-scoped upload endpoint exists, file
	// fields surface as a no-op with an explicit toast rather than firing a
	// 404 silently.
	async function handleUpload(_field: TaskField, _files: File[]) {
		toast.error('File uploads from task forms are not yet supported.');
	}

	function removeFile(fieldName: string, url: string) {
		const current = parseFileValue(getTextValue(fieldName)).filter((f) => f.url !== url);
		setTextValue(fieldName, current.length > 0 ? JSON.stringify(current) : '');
	}
</script>

<form
	class="flex flex-col gap-6"
	onsubmit={(e) => {
		e.preventDefault();
		handleSubmit();
	}}
>
	<!-- Multi-step indicator -->
	{#if steps.length > 1}
		<Stepper.Root bind:step={activeStep}>
			<Stepper.Nav class="mb-2 w-full gap-4 overflow-visible" orientation="horizontal">
				{#each steps as step, i (step.id)}
					{@const isDone = i + 1 < activeStep}
					<Stepper.Item id={step.id} class="min-w-0">
						<Stepper.Trigger
							class="w-full min-w-0 items-center gap-2 rounded-xl px-2 pt-0 pb-1 text-center transition-colors"
							onclick={(e) => handleStepTriggerClick(i + 1, e)}
						>
							<Stepper.Indicator>
								{#if isDone}
									<Check class="size-4" />
								{:else}
									{i + 1}
								{/if}
							</Stepper.Indicator>
							<div class="w-full min-w-0">
								<Stepper.Title
									class="text-sm leading-tight break-words whitespace-normal group-data-[current=false]/stepper-trigger:text-muted-foreground group-data-[current=true]/stepper-trigger:text-foreground"
								>
									{step.title}
								</Stepper.Title>
							</div>
						</Stepper.Trigger>
						<Stepper.Separator />
					</Stepper.Item>
				{/each}
			</Stepper.Nav>
		</Stepper.Root>

		<div class="flex items-center justify-between">
			{#if currentStep}
				<h3 class="text-sm font-semibold text-foreground">{currentStep.title}</h3>
			{:else}
				<span></span>
			{/if}
			<span class="text-sm text-muted-foreground">Step {activeStep} of {steps.length}</span>
		</div>
	{/if}

	<!-- Current step blocks -->
	{#if currentStep}
		{#if currentStep.description_mdsvex}
			<div class={MDSVEX_CLASS}>
				{@html renderMdsvex(currentStep.description_mdsvex)}
			</div>
		{/if}

		{#each currentStep.blocks as block}
			{#if block.type === 'input'}
				{@const field = block.field}
				{@const fieldId = `field-${field.name}`}
				<div class="space-y-2 py-1" data-testid={`step-block-input-${field.name}`}>
					<!-- Label -->
					<Label for={fieldId} class="text-base font-semibold text-foreground">
						{field.label}
						{#if field.required}
							<span class="text-primary">*</span>
						{/if}
					</Label>

					<!-- Field input (ported from HPI FieldRenderer) -->
					{#if field.kind === 'textarea'}
						<!-- Delegated to FieldWidget: spec.testid=`field-${name}` preserves focusField + e2e;
						     rows=4 + class preserve visual chrome. FieldWidget threads spec.testid onto
						     the Textarea element and the class prop onto its class attribute. -->
						<FieldWidget
							spec={toFieldSpec(field)}
							value={getTextValue(field.name)}
							class="min-h-[120px] rounded-xl bg-white/80"
							onchange={(v) => setTextValue(field.name, typeof v === 'string' ? v : String(v ?? ''))}
						/>
					{:else if field.kind === 'select'}
						<Select.Root
							type="single"
							value={getTextValue(field.name)}
							onValueChange={(value) => setTextValue(field.name, value)}
						>
							<Select.Trigger
								id={fieldId}
								data-testid={`field-${field.name}`}
								class="w-full rounded-xl bg-white/80"
							>
								{#if getTextValue(field.name)}
									{getTextValue(field.name)}
								{:else}
									<span class="text-muted-foreground">Select an option</span>
								{/if}
							</Select.Trigger>
							<Select.Content>
								{#each field.options ?? [] as option (option.value)}
									<Select.Item value={option.value} label={option.label} />
								{/each}
							</Select.Content>
						</Select.Root>
					{:else if field.kind === 'checkbox'}
						<div class="flex items-center gap-3 py-2">
							<Checkbox
								id={fieldId}
								data-testid={`field-${field.name}`}
								checked={getCheckboxValue(field.name)}
								onCheckedChange={(value) => setCheckboxValue(field.name, value === true)}
							/>
							<Label for={fieldId} class="cursor-pointer text-base text-foreground">
								Yes
							</Label>
						</div>
					{:else if field.kind === 'file'}
						<FileDropZone.Root
							id={fieldId}
							data-testid={`field-${field.name}-input`}
							accept={field.accept}
							maxFiles={field.max_files ?? 1}
							maxFileSize={field.max_file_size}
							fileCount={parseFileValue(getTextValue(field.name)).length}
							onUpload={(files) => handleUpload(field, files)}
							onFileRejected={({ reason }) => setFieldErrorBound(field.name, reason)}
						>
							<FileDropZone.Trigger data-testid={`field-${field.name}`} />
						</FileDropZone.Root>
						{@const uploadedFiles = parseFileValue(getTextValue(field.name))}
						{#if uploadedFiles.length > 0}
							<ul class="mt-2 space-y-1">
								{#each uploadedFiles as uploaded (uploaded.url)}
									<li class="flex items-center gap-2 text-sm text-muted-foreground">
										<span>{uploaded.name}</span>
										<Button
											variant="ghost"
											size="sm"
											type="button"
											class="h-auto px-1 py-0 text-sm text-destructive hover:text-destructive hover:underline"
											onclick={() => removeFile(field.name, uploaded.url)}
										>
											remove
										</Button>
									</li>
								{/each}
							</ul>
						{/if}
					{:else if field.kind === 'signature'}
						<!-- Delegated to FieldWidget: same SignaturePad, same JSON-string value,
						     same penColor. spec.testid=`field-${name}` threaded onto SignaturePad
						     (FieldWidget signature branch now renders data-testid={spec.testid}). -->
						<FieldWidget
							spec={toFieldSpec(field)}
							value={getTextValue(field.name)}
							onchange={(v) => setTextValue(field.name, typeof v === 'string' ? v : String(v ?? ''))}
						/>
					{:else if field.kind === 'radio'}
						<RadioGroup.Root
							value={getTextValue(field.name)}
							onValueChange={(value) => setTextValue(field.name, value)}
							class="flex flex-col gap-2 py-1"
							data-testid={`field-${field.name}`}
						>
							{#each field.options ?? [] as option, i (option.value)}
								{@const optionId = `${field.name}-${i}`}
								<div class="flex items-center space-x-2 rounded-lg px-2 py-1.5 transition-colors hover:bg-muted/50">
									<RadioGroup.Item value={option.value} id={optionId} />
									<Label for={optionId} class="cursor-pointer font-normal">{option.label}</Label>
								</div>
							{/each}
						</RadioGroup.Root>
					{:else if field.kind === 'date'}
						{@const dateStr = getTextValue(field.name)}
						{@const calDate = dateStr ? parseCalendarDate(dateStr) : undefined}
						{@const timePart = field.include_time ? parseTimePart(dateStr) : ''}
						<div class="flex gap-3" data-testid={`field-${field.name}`}>
							<Popover.Root
								open={datePopoverOpen[field.name] ?? false}
								onOpenChange={(v) => (datePopoverOpen = { ...datePopoverOpen, [field.name]: v })}
							>
								<Popover.Trigger>
									{#snippet child({ props: triggerProps })}
										<Button
											{...triggerProps}
											variant="outline"
											class="w-48 justify-between font-normal {!calDate ? 'text-muted-foreground' : ''}"
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
											setTextValue(
												field.name,
												buildDateString(cd, field.include_time ? timePart || '00:00' : '')
											);
											datePopoverOpen = { ...datePopoverOpen, [field.name]: false };
										}}
									/>
								</Popover.Content>
							</Popover.Root>
							{#if field.include_time}
								<Input
									type="time"
									step="60"
									value={timePart || ''}
									class="w-28 appearance-none bg-background [&::-webkit-calendar-picker-indicator]:hidden [&::-webkit-calendar-picker-indicator]:appearance-none"
									oninput={(event) => {
										const t = (event.currentTarget as HTMLInputElement).value;
										setTextValue(field.name, buildDateString(calDate, t));
									}}
								/>
							{/if}
						</div>
					{:else if field.kind === 'range'}
						{@const rangeMin = field.min ?? 0}
						{@const rangeMax = field.max ?? 100}
						{@const rangeStep = field.step ?? 1}
						<div class="flex max-w-sm items-center gap-3">
							<span class="text-sm text-muted-foreground">{rangeMin}</span>
							<input
								id={fieldId}
								data-testid={`field-${field.name}`}
								type="range"
								min={rangeMin}
								max={rangeMax}
								step={rangeStep}
								class="flex-1 accent-primary"
								value={getTextValue(field.name) || String(rangeMin)}
								oninput={(event) =>
									setTextValue(field.name, (event.currentTarget as HTMLInputElement).value)}
							/>
							<span class="text-sm text-muted-foreground">{rangeMax}</span>
							<span class="min-w-[2.5rem] rounded-md bg-muted/50 px-2 py-1 text-center text-sm font-medium">
								{getTextValue(field.name) || rangeMin}
							</span>
						</div>
					{:else if field.kind === 'rating'}
						{@const maxRating = field.max_rating ?? 5}
						{@const currentRating = getNumberValue(field.name)}
						<div class="flex items-center gap-1 py-1" data-testid={`field-${field.name}`}>
							<RatingGroup.Root
								value={currentRating}
								max={maxRating}
								onValueChange={(v) => setNumberValue(field.name, v)}
								aria-label={field.label}
							>
								{#each Array(maxRating) as _, i (i)}
									<RatingGroup.Item index={i} />
								{/each}
							</RatingGroup.Root>
							{#if currentRating > 0}
								<span class="ml-2 text-sm text-muted-foreground">{currentRating}/{maxRating}</span>
							{/if}
						</div>
					{:else if field.kind === 'number'}
						<!-- number: kept bespoke — FieldWidget.number uses secretPlaceholder (not
						     field.placeholder), so delegating would silently drop the placeholder.
						     Value stays raw string (setTextValue) for coerceFormData at submit. -->
						<Input
							id={fieldId}
							data-testid={`field-${field.name}`}
							type="number"
							placeholder={field.placeholder}
							class="rounded-xl bg-white/80"
							value={getTextValue(field.name)}
							oninput={(event) =>
								setTextValue(field.name, (event.currentTarget as HTMLInputElement).value)}
						/>
					{:else}
						<!-- text: delegated to FieldWidget. spec.testid=`field-${name}` preserves
						     focusField + e2e. class prop threads rounded-xl bg-white/80 chrome.
						     Value stays raw string via setTextValue (coerceFormData unchanged). -->
						<FieldWidget
							spec={toFieldSpec(field)}
							value={getTextValue(field.name)}
							class="rounded-xl bg-white/80"
							onchange={(v) => setTextValue(field.name, typeof v === 'string' ? v : String(v ?? ''))}
						/>
					{/if}

					<!-- Field description -->
					{#if field.description_mdsvex}
						<div class={MDSVEX_CLASS}>
							{@html renderMdsvex(field.description_mdsvex)}
						</div>
					{/if}

					<!-- Validation error -->
					{#if errors[field.name]}
						<p class="text-sm text-destructive" data-testid={`field-error-${field.name}`}>
							{errors[field.name]}
						</p>
					{/if}
				</div>
			{:else if block.type === 'repeater'}
				<RepeaterBlock
					items_ref={block.items_ref}
					item_label_ref={block.item_label_ref}
					blocks={block.blocks}
					output_slug={block.output_slug}
					{taskData}
					getText={getRepeaterFieldText}
					getBool={getRepeaterFieldBool}
					setValue={setRepeaterField}
					{errors}
				/>
			{:else}
				<BlockRenderer {block} />
			{/if}
		{/each}
	{/if}

	<!-- Navigation / action bar -->
	<div class="flex items-center gap-2 border-t border-border pt-4">
		{#if oncancel}
			<Button
				type="button"
				variant="ghost"
				class="text-muted-foreground hover:text-red-700"
				onclick={handleCancel}
				disabled={submitting}
			>
				Reject task
			</Button>
		{/if}

		<div class="ml-auto flex items-center gap-2">
			{#if steps.length > 1 && activeStep > 1}
				<Button
					type="button"
					variant="outline"
					onclick={goToPreviousStep}
					disabled={submitting}
				>
					Previous
				</Button>
			{/if}

			{#if steps.length > 1 && !isLastStep}
				<Button type="button" onclick={goToNextStep} disabled={submitting}>
					Next
				</Button>
			{:else}
				<Button
					type="submit"
					disabled={submitting}
					class="group gap-2 shadow-md shadow-primary/20 transition hover:-translate-y-0.5 hover:shadow-lg hover:shadow-primary/30"
				>
					{submitting ? 'Submitting...' : 'Complete task'}
					{#if !submitting}
						<ArrowRight class="size-4 opacity-70 transition-opacity group-hover:opacity-100" />
					{/if}
				</Button>
			{/if}
		</div>
	</div>
</form>
