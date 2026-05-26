<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { Button } from '$lib/components/ui/button';
	import type { SignaturePadProps } from './types';
	import type { SignatureValue } from '$lib/hpi/types';

	let {
		value = '',
		onchange,
		penColor = '#1e293b',
		disabled = false,
		id,
		'data-testid': dataTestId,
		'aria-invalid': ariaInvalid,
		'aria-describedby': ariaDescribedBy
	}: SignaturePadProps = $props();

	let canvasEl = $state<HTMLCanvasElement | null>(null);
	let isDrawing = $state(false);
	let hasStrokes = $state(false);

	const parsedValue = $derived.by((): SignatureValue | null => {
		if (!value) return null;
		try {
			const parsed = JSON.parse(value) as SignatureValue;
			if (typeof parsed.mode === 'string' && typeof parsed.data === 'string') return parsed;
			return null;
		} catch {
			return null;
		}
	});

	const isEmpty = $derived(!hasStrokes && !parsedValue);

	$effect(() => {
		const canvas = canvasEl;
		if (!canvas) return;

		const ctx = canvas.getContext('2d');
		if (!ctx) return;

		const observer = new ResizeObserver((entries) => {
			for (const entry of entries) {
				const { width, height } = entry.contentRect;
				if (width === 0 || height === 0) continue;
				const dpr = window.devicePixelRatio || 1;
				canvas.width = width * dpr;
				canvas.height = height * dpr;
				ctx.scale(dpr, dpr);

				if (parsedValue?.data.startsWith('data:')) {
					const img = new Image();
					img.onload = () => {
						ctx.clearRect(0, 0, width, height);
						ctx.drawImage(img, 0, 0, width, height);
					};
					img.src = parsedValue.data;
				}
			}
		});
		observer.observe(canvas);

		return () => observer.disconnect();
	});

	function getPoint(e: PointerEvent): { x: number; y: number } {
		const rect = canvasEl!.getBoundingClientRect();
		return { x: e.clientX - rect.left, y: e.clientY - rect.top };
	}

	function handlePointerDown(e: PointerEvent) {
		if (disabled) return;
		const canvas = canvasEl;
		if (!canvas) return;

		canvas.setPointerCapture(e.pointerId);
		isDrawing = true;

		const ctx = canvas.getContext('2d')!;
		const { x, y } = getPoint(e);
		ctx.beginPath();
		ctx.moveTo(x, y);
		ctx.strokeStyle = penColor;
		ctx.lineWidth = 2;
		ctx.lineCap = 'round';
		ctx.lineJoin = 'round';
	}

	function handlePointerMove(e: PointerEvent) {
		if (!isDrawing || disabled) return;
		const ctx = canvasEl?.getContext('2d');
		if (!ctx) return;

		const { x, y } = getPoint(e);
		ctx.lineTo(x, y);
		ctx.stroke();
		hasStrokes = true;
	}

	function handlePointerUp() {
		if (!isDrawing) return;
		isDrawing = false;
		emitValue();
	}

	function emitValue() {
		if (!canvasEl) return;
		const dataUrl = canvasEl.toDataURL('image/png');
		const sig: SignatureValue = {
			mode: 'draw',
			data: dataUrl,
			timestamp: new Date().toISOString()
		};
		onchange?.(JSON.stringify(sig));
	}

	function handleClear() {
		const canvas = canvasEl;
		if (!canvas) return;
		const ctx = canvas.getContext('2d');
		if (!ctx) return;

		const dpr = window.devicePixelRatio || 1;
		ctx.clearRect(0, 0, canvas.width / dpr, canvas.height / dpr);
		hasStrokes = false;
		onchange?.('');
	}
</script>

<div class="space-y-2" data-testid={dataTestId}>
	{#if disabled && parsedValue}
		<div class="rounded-xl border border-border bg-white/80 p-2">
			<img src={parsedValue.data} alt="Signature" class="h-[120px] w-full object-contain" />
		</div>
	{:else}
		<div
			class="relative rounded-xl border {isEmpty
				? 'border-dashed border-muted-foreground/40'
				: 'border-border'} bg-white/80"
		>
			<canvas
				bind:this={canvasEl}
				{id}
				aria-describedby={ariaDescribedBy}
				aria-label="Signature pad"
				class="h-[120px] w-full cursor-crosshair touch-none"
				onpointerdown={handlePointerDown}
				onpointermove={handlePointerMove}
				onpointerup={handlePointerUp}
				onpointerleave={handlePointerUp}
			></canvas>
			{#if isEmpty}
				<div
					class="pointer-events-none absolute inset-0 flex items-center justify-center text-sm text-muted-foreground/50"
				>
					Sign here
				</div>
			{/if}
		</div>
		{#if !disabled && !isEmpty}
			<Button
				variant="ghost"
				size="sm"
				type="button"
				class="h-auto px-0 py-0 text-sm text-muted-foreground hover:text-destructive"
				onclick={handleClear}
				data-testid={dataTestId ? `${dataTestId}-clear` : undefined}
			>
				Clear signature
			</Button>
		{/if}
	{/if}
</div>
