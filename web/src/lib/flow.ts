import type { Binding, FlowDefinition, FlowNode, NodeKind } from './types.ts';

export const NODE_SPECS: Record<NodeKind, {
  label: string;
  description: string;
  inputs: { name: string; optional?: boolean }[];
  outputs: string[];
}> = {
  input: { label: 'Input', description: 'Read one declared flow input.', inputs: [], outputs: ['value'] },
  'prompt.render': { label: 'Prompt', description: 'Render a prompt from bound values.', inputs: [{ name: 'text' }, { name: 'question', optional: true }], outputs: ['value'] },
  'llm.chat': { label: 'Model', description: 'Call a configured model alias.', inputs: [{ name: 'prompt' }], outputs: ['value'] },
  condition: { label: 'Condition', description: 'Compare text with an expected value.', inputs: [{ name: 'text' }], outputs: ['value'] },
  select: { label: 'Select', description: 'Choose a branch result.', inputs: [{ name: 'condition' }, { name: 'when_true' }, { name: 'when_false' }], outputs: ['value'] },
  'rag.retrieve': { label: 'Retrieve', description: 'Fetch tenant RAGFlow sources.', inputs: [{ name: 'query' }], outputs: ['context', 'value', 'sources'] },
  'variable.get': { label: 'Get variable', description: 'Read a conversation string variable.', inputs: [], outputs: ['value'] },
  'variable.set': { label: 'Set variable', description: 'Write a conversation string variable.', inputs: [{ name: 'value' }], outputs: ['value'] },
  output: { label: 'Output', description: 'Expose the final string value.', inputs: [{ name: 'value' }], outputs: ['value'] }
};

const defaults: Record<NodeKind, Record<string, string | number>> = {
  input: { name: 'message' },
  'prompt.render': { template: 'Answer the user: {{text}}' },
  'llm.chat': { model_alias: 'mock', max_output_tokens: 512 },
  condition: { equals: '' },
  select: {},
  'rag.retrieve': { top_k: 3 },
  'variable.get': { name: 'memory' },
  'variable.set': { name: 'memory' },
  output: {}
};

export function blankNode(type: NodeKind, id: string): FlowNode {
  return {
    id, type, config: { ...defaults[type] }, bindings: {},
    retry: { max_attempts: type === 'llm.chat' ? 1 : 0 },
    timeout_ms: type === 'llm.chat' || type === 'rag.retrieve' ? 30_000 : 1_000
  };
}

export function newFlow(): FlowDefinition {
  const input = blankNode('input', 'message');
  const prompt = blankNode('prompt.render', 'prompt');
  prompt.bindings.text = { node: input.id, output: 'value' };
  const model = blankNode('llm.chat', 'model');
  model.bindings.prompt = { node: prompt.id, output: 'value' };
  const output = blankNode('output', 'reply');
  output.bindings.value = { node: model.id, output: 'value' };
  return withEdges({
    schema_version: 1,
    flow_id: crypto.randomUUID(),
    inputs: { message: 'string' },
    outputs: { reply: { node: output.id, output: 'value' } },
    nodes: [input, prompt, model, output],
    edges: [],
    limits: { max_nodes: 16, max_run_ms: 120_000, max_parallel_nodes: 4 }
  });
}

export function withEdges(flow: FlowDefinition): FlowDefinition {
  return {
    ...flow,
    edges: flow.nodes.flatMap((node) =>
      Object.entries(node.bindings).map(([to_input, from]) => ({ from, to_node: node.id, to_input }))
    )
  };
}

export function renameNode(flow: FlowDefinition, oldId: string, newId: string): FlowDefinition {
  if (!newId || (oldId !== newId && flow.nodes.some((node) => node.id === newId))) {
    throw new Error('Node IDs must be unique and non-empty.');
  }
  const rename = (binding: Binding): Binding => binding.node === oldId ? { ...binding, node: newId } : binding;
  return withEdges({
    ...flow,
    nodes: flow.nodes.map((node) => ({
      ...node,
      id: node.id === oldId ? newId : node.id,
      bindings: Object.fromEntries(Object.entries(node.bindings).map(([key, binding]) => [key, rename(binding)])),
      run_if: node.run_if?.node === oldId ? { ...node.run_if, node: newId } : node.run_if
    })),
    outputs: Object.fromEntries(Object.entries(flow.outputs).map(([key, binding]) => [key, rename(binding)]))
  });
}

export function removeNode(flow: FlowDefinition, id: string): FlowDefinition {
  return withEdges({
    ...flow,
    nodes: flow.nodes.filter((node) => node.id !== id).map((node) => ({
      ...node,
      bindings: Object.fromEntries(Object.entries(node.bindings).filter(([, binding]) => binding.node !== id)),
      run_if: node.run_if?.node === id ? undefined : node.run_if
    })),
    outputs: Object.fromEntries(Object.entries(flow.outputs).filter(([, binding]) => binding.node !== id))
  });
}

export function nextNodeId(flow: FlowDefinition, type: NodeKind): string {
  const base = type.replaceAll('.', '-');
  let index = 1;
  while (flow.nodes.some((node) => node.id === `${base}-${index}`)) index += 1;
  return `${base}-${index}`;
}

export interface GraphNode { id: string; x: number; y: number; column: number }
export function layoutNodes(flow: FlowDefinition): GraphNode[] {
  const byId = new Map(flow.nodes.map((node) => [node.id, node]));
  const columns = new Map<string, number>();
  const visiting = new Set<string>();
  function column(id: string): number {
    if (columns.has(id)) return columns.get(id)!;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const node = byId.get(id);
    const dependencies = node ? [...Object.values(node.bindings).map((binding) => binding.node), ...(node.run_if ? [node.run_if.node] : [])] : [];
    const result = dependencies.length ? Math.max(...dependencies.map((dep) => byId.has(dep) ? column(dep) + 1 : 0)) : 0;
    visiting.delete(id);
    columns.set(id, Math.min(result, flow.nodes.length));
    return columns.get(id)!;
  }
  flow.nodes.forEach((node) => column(node.id));
  const withinColumn = new Map<number, number>();
  return flow.nodes.map((node) => {
    const index = columns.get(node.id) ?? 0;
    const row = withinColumn.get(index) ?? 0;
    withinColumn.set(index, row + 1);
    return { id: node.id, x: 42 + index * 252, y: 54 + row * 150, column: index };
  });
}

export function parseFlowJson(text: string): FlowDefinition {
  const value: unknown = JSON.parse(text);
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error('Flow must be a JSON object.');
  const flow = value as FlowDefinition;
  if (typeof flow.flow_id !== 'string' || !Array.isArray(flow.nodes) || !flow.inputs || !flow.outputs || !flow.limits) {
    throw new Error('Flow needs flow_id, inputs, outputs, nodes, and limits.');
  }
  return withEdges(flow);
}
