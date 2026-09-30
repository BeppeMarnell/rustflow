export type ValueType = 'string' | 'boolean' | 'object';
export type NodeKind =
  | 'input'
  | 'prompt.render'
  | 'llm.chat'
  | 'condition'
  | 'select'
  | 'rag.retrieve'
  | 'variable.get'
  | 'variable.set'
  | 'output';

export interface Binding { node: string; output: string }
export interface FlowNode {
  id: string;
  type: NodeKind;
  config: Record<string, string | number>;
  bindings: Record<string, Binding>;
  run_if?: { node: string; equals: boolean };
  retry: { max_attempts: number };
  timeout_ms: number;
}
export interface FlowDefinition {
  schema_version: number;
  flow_id: string;
  inputs: Record<string, ValueType>;
  outputs: Record<string, Binding>;
  nodes: FlowNode[];
  edges: { from: Binding; to_node: string; to_input: string }[];
  limits: { max_nodes: number; max_run_ms: number; max_parallel_nodes: number };
}
export interface FlowSummary { flow_id: string; name: string; latest_version: number | null }
export interface FlowVersion { version_id: string; version: number; published_at: string }
export interface ConversationSummary {
  conversation_id: string;
  flow_id: string;
  flow_name: string;
  status: string;
  preview: string | null;
  run_count: number;
  created_at: string;
  updated_at: string;
}
export interface Message { id: string; role: string; content: string; created_at: string }
export interface Run {
  run_id: string;
  flow_version_id: string;
  flow_id: string;
  version: number;
  status: string;
  input: Record<string, unknown>;
  output: Record<string, unknown> | null;
  error: unknown;
  created_at: string;
  started_at: string | null;
  finished_at: string | null;
}
export interface RunEvent {
  run_id: string;
  sequence: number;
  node_id: string | null;
  event_type: string;
  status: string | null;
  metadata: Record<string, unknown>;
  occurred_at: string;
}
export interface NodeAttempt {
  node_id: string;
  status: string;
  attempt: number;
  output: unknown;
  error: unknown;
  started_at: string;
  finished_at: string | null;
}
export interface Timeline { run_id: string; events: RunEvent[]; nodes: NodeAttempt[] }
export interface Note { note_id: string; body: string; created_at: string; updated_at: string }
