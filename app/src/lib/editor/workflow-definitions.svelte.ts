// Editor-scoped copy of the loaded template's workflow `definitions` — the
// reusable JSON-Schema fragments referenced by `{"$ref":"#/definitions/X"}`
// (e.g. an Agent's `response_format.schema`).
//
// WHY a module store: the Yjs collaborative doc models only nodes/edges/
// viewport, so `definitions` are NOT in `binding.graph`. They live only on the
// fetched `template` object. The client-side derived-port twin
// (`derived-ports.ts`) needs them to resolve `$ref` response_formats for
// display (canvas card + panel Outputs) — mirroring the server's compile-entry
// `inline_agent_response_format_refs` pass. The editor is single-template per
// JS context, so a reactive module singleton fed on template load is the
// least-invasive seam: the two `outputPortsFor(data)` call sites stay unchanged
// and re-derive reactively when definitions arrive.
//
// Authoring note: the editor has no UI to create `$ref`s (the JSON-Schema
// builder emits inline schemas), so this only ever surfaces definitions from
// imported / demo templates — the picker + compile already resolve them
// server-side.

let definitions = $state<Record<string, unknown>>({});

/** Set on template load. `null`/`undefined` clears to an empty map. */
export function setWorkflowDefinitions(defs: Record<string, unknown> | null | undefined): void {
	definitions = defs ?? {};
}

/** The current workflow definitions. Reactive — reads inside a `$derived`
 * re-run when a template loads. */
export function getWorkflowDefinitions(): Record<string, unknown> {
	return definitions;
}
