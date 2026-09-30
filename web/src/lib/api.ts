import type {
  ConversationSummary, FlowDefinition, FlowSummary, FlowVersion, Message,
  Note, Run, RunEvent, Timeline
} from './types.ts';

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) { super(message); this.status = status; }
}

export async function request<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? undefined : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body)
  });
  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try { message = (await response.json()).error || message; } catch { /* keep HTTP status */ }
    throw new ApiError(response.status, message);
  }
  return response.status === 204 ? undefined as T : response.json() as Promise<T>;
}

export const api = {
  health: () => request<{ status: string }>('/health/ready'),
  flows: () => request<{ flows: FlowSummary[] }>('/v1/flows'),
  versions: (id: string) => request<{ versions: FlowVersion[] }>(`/v1/flows/${id}/versions`),
  version: (id: string, number: number) => request<{ definition: FlowDefinition; version_id: string }>(`/v1/flows/${id}/versions/${number}`),
  validate: (definition: FlowDefinition) => request<{ valid: boolean }>('/v1/flows/validate', 'POST', definition),
  dryRun: (definition: FlowDefinition, input: Record<string, unknown>) => request<{ outputs: Record<string, unknown>; nodes: unknown[] }>('/v1/flows/dry-run', 'POST', { definition, input }),
  publish: (name: string, definition: FlowDefinition) => request<{ version_id: string; version: number }>(`/v1/flows/${definition.flow_id}/versions`, 'POST', { name, definition }),
  conversations: () => request<{ conversations: ConversationSummary[] }>('/v1/conversations'),
  messages: (id: string) => request<{ messages: Message[] }>(`/v1/conversations/${id}/messages`),
  runs: (id: string) => request<{ runs: Run[] }>(`/v1/conversations/${id}/runs`),
  timeline: (id: string) => request<Timeline>(`/v1/runs/${id}/timeline`),
  createRun: (flow_version_id: string, input: Record<string, unknown>, conversation_id?: string) =>
    request<{ run_id: string; conversation_id: string }>('/v1/runs', 'POST', { flow_version_id, input, ...(conversation_id ? { conversation_id } : {}) }),
  notes: (id: string) => request<{ notes: Note[] }>(`/v1/conversations/${id}/notes`),
  addNote: (id: string, body: string) => request<Note>(`/v1/conversations/${id}/notes`, 'POST', { body }),
  updateNote: (id: string, noteId: string, body: string) => request<Note>(`/v1/conversations/${id}/notes/${noteId}`, 'PATCH', { body }),
  deleteNote: (id: string, noteId: string) => request<void>(`/v1/conversations/${id}/notes/${noteId}`, 'DELETE'),
  variables: (id: string) => request<{ variables: Record<string, unknown> }>(`/v1/conversations/${id}/variables`),
  setVariable: (id: string, name: string, value: unknown) => request<unknown>(`/v1/conversations/${id}/variables/${name}`, 'PUT', { value }),
  deleteVariable: (id: string, name: string) => request<void>(`/v1/conversations/${id}/variables/${name}`, 'DELETE')
};

export interface SseEvent { id: string; type: string; data: string }

export class SseParser {
  private buffer = '';
  feed(chunk: string): SseEvent[] {
    this.buffer = (this.buffer + chunk).replaceAll('\r\n', '\n');
    const events: SseEvent[] = [];
    let end: number;
    while ((end = this.buffer.indexOf('\n\n')) !== -1) {
      const block = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(end + 2);
      let id = '';
      let type = 'message';
      const data: string[] = [];
      for (const line of block.split('\n')) {
        if (line.startsWith('id:')) id = line.slice(3).trimStart();
        else if (line.startsWith('event:')) type = line.slice(6).trimStart();
        else if (line.startsWith('data:')) data.push(line.slice(5).trimStart());
      }
      if (data.length) events.push({ id, type, data: data.join('\n') });
    }
    return events;
  }
}

export async function streamRun(runId: string, onEvent: (event: RunEvent) => void): Promise<void> {
  const response = await fetch(`/v1/runs/${runId}/events`, { headers: { Accept: 'text/event-stream' } });
  if (!response.ok || !response.body) throw new ApiError(response.status, 'Could not open run stream.');
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const parser = new SseParser();
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      for (const item of parser.feed(decoder.decode(value, { stream: true }))) {
        if (item.type === 'error') throw new Error(item.data);
        onEvent(JSON.parse(item.data) as RunEvent);
      }
    }
  } finally {
    reader.releaseLock();
  }
}
