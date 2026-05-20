// Backend-defaulted output port shape for AutomatedStep, mirroring
// `service/src/models/template.rs::default_output_port`. Keeping a TS twin so
// the editor can offer a "Reset to backend default" affordance without an API
// round-trip.

import type { components } from '$lib/api/schema';

type Port = components['schemas']['Port'];
type PortField = components['schemas']['PortField'];
type FieldKind = components['schemas']['FieldKind'];
type ExecutionBackendType = components['schemas']['ExecutionBackendType'];

function f(name: string, label: string, kind: FieldKind): PortField {
	return { name, label, kind, required: false };
}

export function defaultOutputPort(backend: ExecutionBackendType): Port {
	let fields: PortField[];
	switch (backend) {
		case 'python':
			fields = [f('result', 'Result', 'json')];
			break;
		case 'process':
			fields = [
				f('stdout', 'Stdout', 'textarea'),
				f('stderr', 'Stderr', 'textarea'),
				f('exit_code', 'Exit Code', 'number')
			];
			break;
		case 'docker':
			fields = [
				f('stdout', 'Stdout', 'textarea'),
				f('stderr', 'Stderr', 'textarea'),
				f('exit_code', 'Exit Code', 'number'),
				f('image', 'Image', 'text')
			];
			break;
		case 'http':
			fields = [
				f('status_code', 'Status Code', 'number'),
				f('body', 'Body', 'json'),
				f('headers', 'Headers', 'json')
			];
			break;
		case 'llm':
			fields = [f('text', 'Text', 'textarea'), f('usage', 'Usage', 'json')];
			break;
		case 'file_ops':
			fields = [f('files', 'Files', 'json')];
			break;
		case 'kreuzberg':
			fields = [f('text', 'Text', 'textarea'), f('metadata', 'Metadata', 'json')];
			break;
		case 'catalogue_query':
			fields = [
				f('artifacts', 'Artifacts', 'json'),
				f('total_count', 'Total', 'number'),
				f('source_process_ids', 'Source Process IDs', 'json')
			];
			break;
		default:
			fields = [];
	}
	return { id: 'out', label: 'Output', fields };
}

export function emptyOutputPort(): Port {
	return { id: 'out', label: 'Output', fields: [] };
}
