// Stand-ins for `$lib/api/client` + `$lib/types/editor` so the dumper can
// `jiti.import` showcase.ts without resolving the real runtime modules.
// Only the literal data exports of showcase.ts are read; these no-op
// callables exist solely to satisfy `import { ... }` lookups.
export const listTemplates = () => { throw new Error('shim: not callable'); };
export const getTemplate = () => { throw new Error('shim: not callable'); };
export const createTemplate = () => { throw new Error('shim: not callable'); };
