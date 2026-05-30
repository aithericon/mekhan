import { getTemplateIoContract } from '$lib/api/client';
import { portsEqual } from '$lib/editor/port-utils';
import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
import type { SubWorkflowNodeData } from '$lib/types/editor';

/**
 * Refresh every SubWorkflow node's derived I/O contract from the compiler's
 * single resolver (`GET /api/v1/templates/{id}/io-contract` = `derive_child_io`,
 * the same derivation publish freezes) and write the result into the Yjs graph.
 *
 * This is the EXACT source the property panel already uses — we only lift it to
 * template-load time so a sub-workflow advertises what it consumes (`input
 * contract`) and returns (`output`) on the canvas without the author opening
 * each node's panel first. No contract logic is reimplemented here: we call the
 * resolver and persist its answer through the normal `updateNodeData` sink.
 *
 * Idempotent: a node is patched only when the freshly-derived contract differs
 * from what's stored (`portsEqual`), so a second run is a no-op and the write
 * can't feed back into a reactive loop.
 */
export async function refreshSubworkflowContracts(binding: YjsGraphBinding): Promise<void> {
	for (const node of binding.graph.nodes) {
		if (node.data.type !== 'sub_workflow') continue;
		const data = node.data as SubWorkflowNodeData;
		if (!data.templateId) continue;
		const version = data.versionPin?.mode === 'pinned' ? data.versionPin.version : undefined;
		try {
			const c = await getTemplateIoContract(data.templateId, version);
			if (portsEqual(data.output, c.output) && portsEqual(data.inputContract, c.input)) {
				continue;
			}
			binding.updateNodeData(node.id, {
				...data,
				output: c.output,
				inputContract: c.input
			});
		} catch (e) {
			// A child that's unpublished / mid-edit just stays unannotated here;
			// the publish path will surface a precise compile error if it matters.
			console.warn(`io-contract refresh failed for sub_workflow node ${node.id}`, e);
		}
	}
}
