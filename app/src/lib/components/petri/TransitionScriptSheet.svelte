<script lang="ts">
	import { X, Pencil, Save, XCircle, Zap } from '@lucide/svelte';
	import MonacoEditor from './MonacoEditor.svelte';
	import type { Transition, Port, TransitionStatus } from '$lib/types/petri';

	interface Props {
		transition: Transition | null;
		inputPorts: Port[];
		outputPorts: Port[];
		guard: string | null;
		script: string;
		status?: TransitionStatus;
		effectHandlerId?: string | null;
		open: boolean;
		onClose: () => void;
		onSaveScript?: (transitionId: string, script: string, guard: string | null) => Promise<void>;
	}

	let {
		transition,
		inputPorts,
		outputPorts,
		guard,
		script,
		status,
		effectHandlerId = null,
		open,
		onClose,
		onSaveScript
	}: Props = $props();

	const isEffect = $derived(!!effectHandlerId);

	// Edit mode state
	let isEditing = $state(false);
	let editedScript = $state('');
	let editedGuard = $state<string | null>(null);
	let isSaving = $state(false);
	let saveError = $state<string | null>(null);

	function startEditing() {
		editedScript = script;
		editedGuard = guard;
		isEditing = true;
		saveError = null;
	}

	function cancelEditing() {
		isEditing = false;
		editedScript = '';
		editedGuard = null;
		saveError = null;
	}

	async function saveChanges() {
		if (!transition) return;

		isSaving = true;
		saveError = null;

		try {
			await onSaveScript?.(transition.id, editedScript, editedGuard);
			isEditing = false;
		} catch (error) {
			saveError = error instanceof Error ? error.message : 'Failed to save changes';
		} finally {
			isSaving = false;
		}
	}

	// Format status for display
	const statusText = $derived.by(() => {
		if (!status) return null;
		switch (status) {
			case 'enabled':
				return { text: 'Enabled', color: 'text-green-600 bg-green-50' };
			case 'disabled_no_tokens':
				return { text: 'Disabled: Missing tokens', color: 'text-amber-600 bg-amber-50' };
			case 'disabled_guard_failed':
				return { text: `Disabled: Guard failed`, color: 'text-red-600 bg-red-50' };
			case 'disabled_guard_error':
				return { text: `Disabled: Guard error`, color: 'text-red-600 bg-red-50' };
			default:
				return null;
		}
	});

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			onClose();
		}
	}
</script>

<svelte:window onkeydown={handleKeydown} />

{#if open && transition}
	<!-- Backdrop - covers the canvas area only (right sidebars: 320px inspector + 288px event log) -->
	<div
		class="fixed inset-0 bg-black/20 z-40"
		style="right: 608px;"
		onclick={onClose}
		onkeydown={(e) => e.key === 'Escape' && onClose()}
		role="button"
		tabindex="-1"
		aria-label="Close sheet"
	></div>

	<!-- Sheet panel - slides from bottom over the canvas -->
	<div
		class="fixed bottom-0 left-0 z-50 bg-card border-t border-r border-border shadow-2xl transition-transform duration-300"
		style="right: 608px; max-height: 70vh;"
	>
		<!-- Header -->
		<div class="flex items-center justify-between px-4 py-3 border-b border-border bg-muted">
			<div class="flex items-center gap-3">
				<h2 class="text-lg font-semibold text-foreground">{transition.name}</h2>
				{#if isEffect}
					<span class="flex items-center gap-1 px-2 py-0.5 text-sm font-medium rounded bg-purple-500/15 text-purple-700 dark:text-purple-400">
						<Zap class="w-3 h-3" />
						Effect
					</span>
				{/if}
				{#if statusText && !isEditing}
					<span class="px-2 py-0.5 text-sm font-medium rounded {statusText.color}">
						{statusText.text}
					</span>
				{/if}
				{#if isEditing}
					<span class="px-2 py-0.5 text-sm font-medium rounded bg-blue-500/15 text-blue-400">
						Editing
					</span>
				{/if}
			</div>
			<div class="flex items-center gap-2">
				{#if isEditing}
					<button
						onclick={saveChanges}
						disabled={isSaving}
						class="flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded bg-green-600 text-white hover:bg-green-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
					>
						<Save class="w-4 h-4" />
						{isSaving ? 'Saving...' : 'Save'}
					</button>
					<button
						onclick={cancelEditing}
						disabled={isSaving}
						class="flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded bg-secondary text-secondary-foreground hover:bg-accent disabled:opacity-50 transition-colors"
					>
						<XCircle class="w-4 h-4" />
						Cancel
					</button>
				{:else}
					{#if !isEffect}
						<button
							onclick={startEditing}
							class="flex items-center gap-1.5 px-3 py-1.5 text-sm font-medium rounded bg-blue-600 text-white hover:bg-blue-700 transition-colors"
						>
							<Pencil class="w-4 h-4" />
							Edit
						</button>
					{/if}
				{/if}
				<button
					onclick={onClose}
					class="p-1 rounded hover:bg-muted transition-colors"
					aria-label="Close"
				>
					<X class="w-5 h-5 text-muted-foreground" />
				</button>
			</div>
		</div>

		<!-- Content -->
		<div class="overflow-y-auto p-4 space-y-6" style="max-height: calc(70vh - 56px);">
			<!-- Ports section -->
			<div class="grid grid-cols-2 gap-4">
				<!-- Input Ports -->
				<div>
					<h3 class="text-sm font-medium text-foreground/80 mb-2">Input Ports</h3>
					{#if inputPorts.length > 0}
						<div class="space-y-1">
							{#each inputPorts as port (port.name)}
								<div class="flex items-center gap-2 px-2 py-1.5 bg-blue-500/10 rounded text-sm">
									<span class="w-2 h-2 rounded-full bg-blue-400"></span>
									<span class="font-mono">{port.name}</span>
									<span class="text-muted-foreground text-sm">({port.cardinality})</span>
								</div>
							{/each}
						</div>
					{:else}
						<p class="text-sm text-muted-foreground italic">No input ports</p>
					{/if}
				</div>

				<!-- Output Ports -->
				<div>
					<h3 class="text-sm font-medium text-foreground/80 mb-2">Output Ports</h3>
					{#if outputPorts.length > 0}
						<div class="space-y-1">
							{#each outputPorts as port (port.name)}
								<div class="flex items-center gap-2 px-2 py-1.5 bg-green-500/10 rounded text-sm">
									<span class="w-2 h-2 rounded-full bg-green-400"></span>
									<span class="font-mono">{port.name}</span>
									<span class="text-muted-foreground text-sm">({port.cardinality})</span>
								</div>
							{/each}
						</div>
					{:else}
						<p class="text-sm text-muted-foreground italic">No output ports</p>
					{/if}
				</div>
			</div>

			<!-- Error message -->
			{#if saveError}
				<div class="p-3 bg-red-500/10 border border-red-500/30 rounded-lg text-sm text-red-400">
					<strong>Error:</strong> {saveError}
				</div>
			{/if}

			{#if isEffect}
				<!-- Effect Handler section -->
				<div class="p-4 bg-purple-500/10 border border-purple-500/30 rounded-lg space-y-3">
					<div class="flex items-center gap-2">
						<Zap class="w-4 h-4 text-purple-600 dark:text-purple-400" />
						<h3 class="text-sm font-medium text-foreground">Effect Handler</h3>
					</div>
					<div class="px-3 py-2 bg-card rounded border border-border font-mono text-sm text-foreground">
						{effectHandlerId}
					</div>
					<p class="text-sm text-muted-foreground">
						This transition executes a registered effect handler instead of a Rhai script.
						Effects run side-effects in live mode and replay from stored results during replay.
					</p>
				</div>

				<!-- Guard section (effects can still have guards) -->
				{#if guard}
					<div>
						<h3 class="text-sm font-medium text-foreground/80 mb-2 flex items-center gap-2">
							Guard Script
							{#if status === 'disabled_guard_failed'}
								<span class="text-sm text-red-500">(currently false)</span>
							{/if}
						</h3>
						<MonacoEditor value={guard} language="rhai" height="80px" readOnly />
					</div>
				{/if}
			{:else}
				<!-- Guard section -->
				{#if guard || isEditing}
					<div>
						<h3 class="text-sm font-medium text-foreground/80 mb-2 flex items-center gap-2">
							Guard Script
							{#if !isEditing && status === 'disabled_guard_failed'}
								<span class="text-sm text-red-500">(currently false)</span>
							{/if}
						</h3>
						{#if isEditing}
							<MonacoEditor
								value={editedGuard || ''}
								language="rhai"
								height="80px"
								readOnly={false}
								onChange={(v) => editedGuard = v || null}
							/>
						{:else}
							<MonacoEditor value={guard || '// No guard'} language="rhai" height="80px" readOnly />
						{/if}
					</div>
				{/if}

				<!-- Main Script section -->
				<div>
					<h3 class="text-sm font-medium text-foreground/80 mb-2">Main Script (Rhai)</h3>
					{#if isEditing}
						<MonacoEditor
							value={editedScript}
							language="rhai"
							height="200px"
							readOnly={false}
							onChange={(v) => editedScript = v}
						/>
					{:else}
						<MonacoEditor value={script || '// No script defined'} language="rhai" height="200px" readOnly />
					{/if}
				</div>

				<!-- Script explanation -->
				<div class="p-3 bg-muted rounded-lg text-sm text-muted-foreground">
					<p class="font-medium mb-1">How scripts work:</p>
					<ul class="list-disc list-inside space-y-1 text-sm">
						<li>Input port data is available as variables (e.g., <code class="bg-secondary px-1 rounded">order</code>, <code class="bg-secondary px-1 rounded">ctx</code>)</li>
						<li>Guard scripts return <code class="bg-secondary px-1 rounded">true</code>/<code class="bg-secondary px-1 rounded">false</code> to enable/disable the transition</li>
						<li>Main scripts return a map of output port data: <code class="bg-secondary px-1 rounded">#&#123; out: data &#125;</code></li>
					</ul>
				</div>
			{/if}
		</div>
	</div>
{/if}
