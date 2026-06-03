<script lang="ts">
	// ROS automated-step config panel.
	//
	// Authoring surface for the `ros` execution backend (mirrors the executor's
	// `RosConfig` DTO the mekhan compiler validates and the ROS backend runs):
	//  - operation select (publish_topic | call_service | await_topic |
	//    send_action_goal). `publish_topic` is the default — it publishes a
	//    single message onto a topic. `call_service` invokes a service and
	//    awaits the response. `await_topic` blocks until the next message on a
	//    topic arrives. `send_action_goal` sends a goal to an action server and
	//    awaits the result.
	//  - interface_name: the ROS graph name (topic / service / action), e.g.
	//    `/turtle1/cmd_vel`.
	//  - interface_type: the ROS interface type, e.g. `geometry_msgs/Twist`. A
	//    datalist surfaces a few common turtlesim types as suggestions.
	//  - fields: the message / request / goal payload as JSON. May carry
	//    `{{ slug.field }}` refs the backend Tera-renders at run time.
	//  - timeout_ms: optional per-request timeout (default 30000).
	//
	// There is NO ResourcePicker — ROS has no workspace resource. The derived
	// output port is recomputed automatically by AutomatedStepSection on every
	// config change (output_authoring == Derived).
	//
	// Persistence follows the repo's onchange-config idiom (NOT bind:) — the
	// panel emits a fresh config object via `onchange`, identical to the Loki /
	// Postgres panels.

	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import InsertRefButton from '../InsertRefButton.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { appendSnippet } from '$lib/editor/append-snippet';

	type Props = {
		config: Record<string, unknown>;
		readonly?: boolean;
		onchange: (config: Record<string, unknown>) => void;
		scope?: ScopeEntry[];
		binding?: unknown;
		nodeId?: string;
		templateId?: string;
	};

	let { config, readonly = false, onchange, scope = [] }: Props = $props();

	// Typed reads with defaults matching the executor's RosConfig serde defaults
	// so partial drafts deserialize correctly when re-saving.
	const operation = $derived((config.operation as string | undefined) ?? 'publish_topic');
	const interfaceName = $derived((config.interface_name as string | undefined) ?? '');
	const interfaceType = $derived((config.interface_type as string | undefined) ?? '');
	const timeoutMs = $derived(config.timeout_ms as number | undefined);

	// `fields` is a structured JSON payload. We keep a local editable text buffer
	// so an in-progress (invalid) edit doesn't get discarded or crash the panel.
	// On every keystroke we try to parse; valid JSON is committed to the config,
	// invalid JSON keeps the raw text on screen and shows a subtle hint.
	function fieldsToText(v: unknown): string {
		if (v === undefined || v === null) return '';
		try {
			return JSON.stringify(v, null, 2);
		} catch {
			return '';
		}
	}

	// Initial buffer seed. The `$effect` below keeps it in sync with later
	// upstream `config.fields` changes, so only the initial value is read here.
	// svelte-ignore state_referenced_locally
	let fieldsText = $state(fieldsToText(config.fields));
	let fieldsError = $state(false);

	// Re-seed the buffer when the upstream config.fields changes from outside
	// this editor (e.g. switching nodes / collaborative edit) — but only when it
	// differs from what our current text parses to, so we don't clobber an
	// in-progress edit on every keystroke round-trip.
	$effect(() => {
		const incoming = fieldsToText(config.fields);
		let mine: string;
		try {
			mine = fieldsToText(JSON.parse(fieldsText));
		} catch {
			mine = '__invalid__';
		}
		if (incoming !== mine) {
			fieldsText = incoming;
			fieldsError = false;
		}
	});

	const operationLabels: Record<string, string> = {
		publish_topic: 'Publish topic',
		call_service: 'Call service',
		await_topic: 'Await topic',
		send_action_goal: 'Send action goal'
	};

	// Common turtlesim interface types surfaced as datalist suggestions.
	const commonTypes = [
		'geometry_msgs/Twist',
		'turtlesim/Pose',
		'turtlesim/srv/TeleportAbsolute',
		'turtlesim/srv/Spawn',
		'turtlesim/action/RotateAbsolute'
	];

	function patch(updates: Record<string, unknown>) {
		onchange({ ...config, ...updates });
	}

	// Optional string field: set when non-empty, delete the key when cleared so
	// the wire config omits it rather than carrying an empty string.
	function patchOptionalString(key: string, value: string) {
		const next = { ...config };
		if (value.trim() === '') delete next[key];
		else next[key] = value;
		onchange(next);
	}

	// Number field with a serde default (timeout_ms): delete the key when cleared
	// so the default applies, otherwise store the parsed integer.
	function patchNumber(key: string, raw: string) {
		const next = { ...config };
		const v = parseInt(raw, 10);
		if (raw.trim() === '' || Number.isNaN(v)) delete next[key];
		else next[key] = v;
		onchange(next);
	}

	// On every edit of the JSON buffer, try to commit. Empty clears the key;
	// valid JSON commits the parsed value; invalid JSON flags the hint but keeps
	// the raw text so the user can keep typing.
	function onFieldsInput(raw: string) {
		fieldsText = raw;
		if (raw.trim() === '') {
			fieldsError = false;
			const next = { ...config };
			delete next.fields;
			onchange(next);
			return;
		}
		try {
			const parsed = JSON.parse(raw);
			fieldsError = false;
			patch({ fields: parsed });
		} catch {
			fieldsError = true;
		}
	}

	// Insert a ref snippet into the JSON buffer. Refs land adjacent (no
	// separating space) so a placeholder can drop inside a JSON string literal.
	function insertRef(snippet: string) {
		onFieldsInput(appendSnippet(fieldsText, snippet, ''));
	}
</script>

<div class="space-y-1.5">
	<FormField label="Operation" for="ros-operation">
		<Select.Root
			type="single"
			value={operation}
			onValueChange={(v) => {
				if (v) patch({ operation: v });
			}}
			disabled={readonly}
		>
			<Select.Trigger disabled={readonly} id="ros-operation" data-testid="ros-operation">
				{operationLabels[operation] ?? operation}
			</Select.Trigger>
			<Select.Content>
				<Select.Item value="publish_topic" label={operationLabels.publish_topic} />
				<Select.Item value="call_service" label={operationLabels.call_service} />
				<Select.Item value="await_topic" label={operationLabels.await_topic} />
				<Select.Item value="send_action_goal" label={operationLabels.send_action_goal} />
			</Select.Content>
		</Select.Root>
	</FormField>
</div>

<div class="space-y-1.5">
	<FormField label="Interface name" for="ros-interface-name">
		<Input
			id="ros-interface-name"
			type="text"
			value={interfaceName}
			placeholder="/turtle1/cmd_vel"
			disabled={readonly}
			oninput={(e) => patch({ interface_name: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
			data-testid="ros-interface-name"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		The ROS graph name of the topic, service, or action.
	</p>
</div>

<div class="space-y-1.5">
	<FormField label="Interface type" for="ros-interface-type">
		<Input
			id="ros-interface-type"
			type="text"
			value={interfaceType}
			placeholder="geometry_msgs/Twist"
			list="ros-interface-types"
			disabled={readonly}
			oninput={(e) => patch({ interface_type: (e.currentTarget as HTMLInputElement).value })}
			class="font-mono"
			data-testid="ros-interface-type"
		/>
		<datalist id="ros-interface-types">
			{#each commonTypes as t (t)}
				<option value={t}></option>
			{/each}
		</datalist>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		The ROS interface type, e.g. <code class="font-mono">geometry_msgs/Twist</code>.
	</p>
</div>

<div class="space-y-1.5">
	<FormField label="Fields (JSON)" for="ros-fields">
		<Textarea
			id="ros-fields"
			value={fieldsText}
			placeholder={'{\n  "linear": { "x": 2.0 },\n  "angular": { "z": 1.0 }\n}'}
			disabled={readonly}
			oninput={(e) => onFieldsInput((e.currentTarget as HTMLTextAreaElement).value)}
			class="min-h-[6rem] font-mono text-sm"
			rows={6}
			data-testid="ros-fields"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		The message / request / goal payload as JSON. Use
		<code class="font-mono">{'{{ slug.field }}'}</code> to splice an upstream value.
	</p>
	{#if fieldsError}
		<p class="text-sm italic text-destructive" data-testid="ros-fields-error">
			Invalid JSON — fix the syntax to save these fields.
		</p>
	{/if}
	{#if scope.length > 0 && !readonly}
		<InsertRefButton
			{scope}
			disabled={readonly}
			placeholder="Insert ref into fields…"
			oninsert={(s) => insertRef(s)}
		/>
	{/if}
</div>

<div class="space-y-1.5">
	<FormField label="Timeout (ms)" for="ros-timeout">
		<Input
			id="ros-timeout"
			type="number"
			min={1}
			value={timeoutMs ?? ''}
			placeholder="30000"
			disabled={readonly}
			oninput={(e) => patchNumber('timeout_ms', (e.currentTarget as HTMLInputElement).value)}
			data-testid="ros-timeout"
		/>
	</FormField>
	<p class="text-sm italic text-muted-foreground">
		Per-request timeout. Capped at the step's overall job timeout.
	</p>
</div>
