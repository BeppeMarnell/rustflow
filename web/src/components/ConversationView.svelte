<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { api, streamRun } from '../lib/api.ts';
  import type { FlowDefinition, FlowSummary, Message, Note, Run, Timeline, ValueType } from '../lib/types.ts';

  export let selectedConversationId: string | null;
  export let flows: FlowSummary[];
  export let onConversationCreated: (id: string) => void;

  let flowChoice = '';
  let versionId = '';
  let versionNumber = 0;
  let definition: FlowDefinition | null = null;
  let inputDraft: Record<string, string | boolean> = {};
  let messages: Message[] = [];
  let runs: Run[] = [];
  let notes: Note[] = [];
  let variables: Record<string, unknown> = {};
  let selectedRunId: string | null = null;
  let timeline: Timeline | null = null;
  let inspectorTab: 'trace' | 'notes' | 'variables' = 'trace';
  let streamText = '';
  let running = false;
  let error = '';
  let noteDraft = '';
  let editingNote = '';
  let editingBody = '';
  let variableName = '';
  let variableJson = '""';
  let transcript: HTMLDivElement;

  onMount(() => {
    const current = selectedConversationId ? null : flows[0];
    flowChoice = current?.flow_id ?? '';
    if (selectedConversationId) {
      void loadConversation();
    } else if (flowChoice) {
      void loadFlow(flowChoice);
    }
  });
  $: if (!selectedConversationId && !flowChoice && flows.length) { void loadFlow(flows[0].flow_id); }

  async function loadConversation() {
    if (!selectedConversationId) return;
    try {
      const [messageResult, runResult, noteResult, variableResult] = await Promise.all([
        api.messages(selectedConversationId), api.runs(selectedConversationId),
        api.notes(selectedConversationId), api.variables(selectedConversationId)
      ]);
      messages = messageResult.messages;
      runs = runResult.runs;
      notes = noteResult.notes;
      variables = variableResult.variables;
      flowChoice = runs[0]?.flow_id || flows[0]?.flow_id || '';
      if (flowChoice) await loadFlow(flowChoice);
      if (runs.length && !selectedRunId) await selectRun(runs[0].run_id);
      await scrollDown();
    } catch (failure) { error = messageOf(failure); }
  }

  async function loadFlow(id: string) {
    flowChoice = id;
    versionId = '';
    definition = null;
    try {
      const versions = await api.versions(id);
      if (!versions.versions.length) return;
      const selected = versions.versions[0];
      const result = await api.version(id, selected.version);
      versionId = selected.version_id;
      versionNumber = selected.version;
      definition = result.definition;
      inputDraft = Object.fromEntries(Object.entries(definition.inputs).map(([name, type]) => [name, type === 'boolean' ? false : type === 'object' ? '{}' : '']));
      error = '';
    } catch (failure) { error = messageOf(failure); }
  }

  function messageOf(value: unknown): string { return value instanceof Error ? value.message : 'Request failed.'; }
  function parseInput(): Record<string, unknown> {
    const input: Record<string, unknown> = {};
    for (const [name, type] of Object.entries(definition?.inputs ?? {})) {
      const value = inputDraft[name];
      input[name] = type === 'object' ? JSON.parse(String(value || '{}')) : type === 'boolean' ? Boolean(value) : String(value ?? '');
    }
    return input;
  }

  async function scrollDown() {
    await tick();
    if (transcript) transcript.scrollTop = transcript.scrollHeight;
  }

  async function send() {
    if (!versionId || running) return;
    error = '';
    let input: Record<string, unknown>;
    try { input = parseInput(); } catch { error = 'Object inputs must contain valid JSON.'; return; }
    running = true;
    streamText = '';
    try {
      const run = await api.createRun(versionId, input, selectedConversationId ?? undefined);
      if (!selectedConversationId) {
        messages = [...messages, { id: run.run_id, role: 'user', content: String(input.message ?? JSON.stringify(input)), created_at: new Date().toISOString() }];
      } else {
        const updated = await api.messages(selectedConversationId);
        messages = updated.messages;
      }
      await scrollDown();
      await streamRun(run.run_id, (event) => {
        if (event.event_type === 'token') {
          streamText += String(event.metadata.token ?? '');
          void scrollDown();
        }
        if (event.event_type === 'run.failed' || event.event_type === 'run.cancelled') {
          error = `Run ${event.status || 'failed'}. Open its trace for details.`;
        }
      });
      if (selectedConversationId) {
        await loadConversation();
      } else {
        onConversationCreated(run.conversation_id);
      }
      for (const name of Object.keys(inputDraft)) if (definition?.inputs[name] === 'string') inputDraft[name] = '';
      inputDraft = { ...inputDraft };
      streamText = '';
    } catch (failure) { error = messageOf(failure); }
    finally { running = false; }
  }

  async function selectRun(id: string) {
    selectedRunId = id;
    try { timeline = await api.timeline(id); } catch (failure) { error = messageOf(failure); }
  }

  async function addNote() {
    if (!selectedConversationId || !noteDraft.trim()) return;
    try {
      const note = await api.addNote(selectedConversationId, noteDraft);
      notes = [...notes, note];
      noteDraft = '';
    } catch (failure) { error = messageOf(failure); }
  }

  async function saveNote(noteId: string) {
    if (!selectedConversationId) return;
    try {
      const updated = await api.updateNote(selectedConversationId, noteId, editingBody);
      notes = notes.map((note) => note.note_id === noteId ? updated : note);
      editingNote = '';
    } catch (failure) { error = messageOf(failure); }
  }

  async function deleteNote(noteId: string) {
    if (!selectedConversationId) return;
    try {
      await api.deleteNote(selectedConversationId, noteId);
      notes = notes.filter((note) => note.note_id !== noteId);
    } catch (failure) { error = messageOf(failure); }
  }

  async function setVariable() {
    if (!selectedConversationId || !variableName.trim()) return;
    try {
      await api.setVariable(selectedConversationId, variableName, JSON.parse(variableJson));
      variables = (await api.variables(selectedConversationId)).variables;
      variableName = '';
      variableJson = '""';
    } catch (failure) { error = messageOf(failure); }
  }

  async function deleteVariable(name: string) {
    if (!selectedConversationId) return;
    try {
      await api.deleteVariable(selectedConversationId, name);
      variables = (await api.variables(selectedConversationId)).variables;
    } catch (failure) { error = messageOf(failure); }
  }

  function inputType(type: ValueType) { return type === 'boolean' ? 'checkbox' : 'text'; }
  function time(value: string) { return new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' }).format(new Date(value)); }
  function dateLabel(value: string) { return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(value)); }
</script>

<div class="workbench conversation-workbench">
  <section class="primary-pane">
    <div class="pane-header">
      <div class="pane-title"><div class="eyebrow">{selectedConversationId ? 'CONVERSATION' : 'NEW CONVERSATION'}</div><h1>{selectedConversationId ? (flows.find((flow) => flow.flow_id === flowChoice)?.name || 'Conversation') : 'Start a conversation'}</h1></div>
      <div class="header-actions"><span class="version-caption">{versionId ? `Flow v${versionNumber}` : 'Choose a flow'}</span></div>
    </div>

    <div class="transcript" bind:this={transcript}>
      {#if !messages.length && !running}
        <div class="conversation-empty">
          <div class="empty-orbit">◎</div>
          <h2>No messages yet</h2>
          <p>Choose a published flow and send its first input. The run trace and notes will appear alongside the conversation.</p>
        </div>
      {/if}
      {#each messages as message (message.id)}
        <article class="message" class:user-message={message.role === 'user'}>
          <div class="message-rail"><span class="message-mark">{message.role === 'assistant' ? 'r.' : 'u'}</span></div>
          <div class="message-body"><div class="message-heading"><strong>{message.role === 'assistant' ? 'Assistant' : message.role === 'user' ? 'You' : message.role}</strong><span>{time(message.created_at)}</span></div><div class="message-content">{message.content}</div></div>
        </article>
      {/each}
      {#if running}
        <article class="message streaming-message"><div class="message-rail"><span class="message-mark">r.</span></div><div class="message-body"><div class="message-heading"><strong>Assistant</strong><span class="streaming-label">● STREAMING</span></div><div class="message-content">{streamText}<span class="cursor"></span></div></div></article>
      {/if}
    </div>

    <div class="composer-wrap">
      {#if error}<div class="composer-error" role="alert">{error}</div>{/if}
      <div class="composer-meta">
        <label for="flow-choice">FLOW</label>
        <select id="flow-choice" value={flowChoice} onchange={(event) => void loadFlow(event.currentTarget.value)} disabled={running}>
          {#if !flows.length}<option value="">No published flows</option>{/if}
          {#each flows as flow}<option value={flow.flow_id}>{flow.name} · v{flow.latest_version}</option>{/each}
        </select>
        {#if selectedConversationId}<span class="conversation-id mono">{selectedConversationId.slice(0, 8)}…</span>{/if}
      </div>
      {#if definition}
        <div class="composer-fields">
          {#each Object.entries(definition.inputs) as [name, type]}
            <label class="composer-field" for={'input-' + name}>
              <span>{name.toUpperCase()} <em>{type}</em></span>
              {#if type === 'object'}
                <textarea id={'input-' + name} value={String(inputDraft[name] ?? '{}')} oninput={(event) => { inputDraft[name] = event.currentTarget.value; }} rows="3"></textarea>
              {:else if type === 'boolean'}
                <input id={'input-' + name} type={inputType(type)} checked={Boolean(inputDraft[name])} onchange={(event) => { inputDraft[name] = event.currentTarget.checked; }} />
              {:else}
                <input id={'input-' + name} type="text" placeholder="Write a message…" value={String(inputDraft[name] ?? '')} oninput={(event) => { inputDraft[name] = event.currentTarget.value; }} onkeydown={(event) => { if (event.key === 'Enter') void send(); }} />
              {/if}
            </label>
          {/each}
        </div>
      {/if}
      <div class="composer-bottom"><span>Runs use the selected published version.</span><button class="button primary" disabled={!versionId || running} onclick={() => void send()}>{running ? 'Running…' : 'Send input ↗'}</button></div>
    </div>
  </section>

  <aside class="inspector">
    <div class="inspector-header"><div class="eyebrow">CONTEXT</div><h2>Conversation details</h2></div>
    <div class="inspector-tabs">
      <button class:active={inspectorTab === 'trace'} onclick={() => inspectorTab = 'trace'}>Trace</button>
      <button class:active={inspectorTab === 'notes'} onclick={() => inspectorTab = 'notes'}>Notes {notes.length || ''}</button>
      <button class:active={inspectorTab === 'variables'} onclick={() => inspectorTab = 'variables'}>Variables</button>
    </div>
    <div class="inspector-scroll">
      {#if inspectorTab === 'trace'}
        <div class="section-label">RUNS <span>{runs.length}</span></div>
        {#if !runs.length}<p class="empty-note">Run a flow to see events and node attempts here.</p>{/if}
        <div class="run-list">
          {#each runs as run}
            <button class:selected={selectedRunId === run.run_id} onclick={() => void selectRun(run.run_id)}><span class="run-status" class:success={run.status === 'succeeded'} class:failure={run.status === 'failed'}></span><span>Run {run.run_id.slice(0, 8)}<small>v{run.version} · {run.status}</small></span><span class="run-time">{time(run.created_at)}</span></button>
          {/each}
        </div>
        {#if timeline}
          <hr class="rule" /><div class="section-label">NODE ATTEMPTS <span>{timeline.nodes.length}</span></div>
          {#each timeline.nodes as node}
            <div class="trace-row"><span class="trace-line"></span><div><strong>{node.node_id}</strong><small>{node.status} · attempt {node.attempt}</small>{#if node.error}<pre>{JSON.stringify(node.error, null, 2)}</pre>{/if}</div></div>
          {/each}
          <hr class="rule" /><div class="section-label">EVENTS <span>{timeline.events.length}</span></div>
          {#each timeline.events.filter((event) => event.event_type !== 'token') as event}
            <div class="event-row"><span class="mono">{String(event.sequence).padStart(2, '0')}</span><div>{event.event_type}<small>{event.node_id || 'run'}</small></div></div>
          {/each}
        {/if}
      {:else if inspectorTab === 'notes'}
        {#if !selectedConversationId}<p class="empty-note">Notes can be added after the first turn.</p>{/if}
        {#each notes as note (note.note_id)}
          <div class="note-entry">
            {#if editingNote === note.note_id}<textarea bind:value={editingBody} rows="4"></textarea><div class="note-actions"><button onclick={() => editingNote = ''}>Cancel</button><button onclick={() => void saveNote(note.note_id)}>Save</button></div>
            {:else}<p>{note.body}</p><div class="note-actions"><span>{dateLabel(note.updated_at)}</span><button onclick={() => { editingNote = note.note_id; editingBody = note.body; }}>Edit</button><button onclick={() => void deleteNote(note.note_id)}>Delete</button></div>{/if}
          </div>
        {/each}
        {#if selectedConversationId}<div class="field"><label for="note-draft">ADD NOTE</label><textarea id="note-draft" bind:value={noteDraft} placeholder="A finding, reminder, or decision…" maxlength="10000"></textarea></div><button class="button" disabled={!noteDraft.trim()} onclick={() => void addNote()}>Add note</button>{/if}
      {:else}
        {#if !selectedConversationId}<p class="empty-note">Conversation variables are available after the first turn.</p>{/if}
        {#each Object.entries(variables) as [name, value]}
          <div class="variable-entry"><div><strong>{name}</strong><pre>{JSON.stringify(value, null, 2)}</pre></div><button class="icon-button" aria-label={'Delete ' + name} onclick={() => void deleteVariable(name)}>×</button></div>
        {/each}
        {#if selectedConversationId}<hr class="rule" /><div class="field"><label for="variable-name">NAME</label><input id="variable-name" bind:value={variableName} placeholder="memory" /></div><div class="field"><label for="variable-value">JSON VALUE</label><textarea id="variable-value" bind:value={variableJson} rows="3"></textarea></div><button class="button" disabled={!variableName.trim()} onclick={() => void setVariable()}>Set variable</button>{/if}
      {/if}
    </div>
  </aside>
</div>

<style>
  .version-caption { color: #92998e; font: 11px ui-monospace, SFMono-Regular, Menlo, monospace; }
  .transcript { flex: 1; overflow: auto; padding: 26px clamp(24px, 7vw, 110px) 40px; background: #f4f3ef; }
  .conversation-empty { max-width: 350px; margin: min(17vh, 140px) auto 0; text-align: center; }
  .empty-orbit { font-size: 54px; font-weight: 200; line-height: 1; color: #bb947a; margin-bottom: 16px; }
  .conversation-empty h2 { font-size: 21px; letter-spacing: -.04em; margin: 0 0 7px; }
  .conversation-empty p { color: #8d9589; font-size: 12px; line-height: 1.65; margin: 0; }
  .message { max-width: 760px; display: flex; gap: 16px; position: relative; padding: 0 0 31px; }
  .message:not(:last-child)::after { content: ''; position: absolute; left: 13px; top: 31px; bottom: 0; width: 1px; background: #deded5; }
  .message-rail { flex: 0 0 27px; position: relative; z-index: 1; }
  .message-mark { width: 27px; height: 27px; border-radius: 6px; display: grid; place-items: center; background: #353b32; color: #fff; font: 15px Georgia, serif; }
  .user-message .message-mark { color: #858d82; border: 1px solid #d9dcd2; background: #e9ebe4; font: 600 12px ui-sans-serif, sans-serif; }
  .message-body { flex: 1; min-width: 0; padding-top: 1px; }
  .message-heading { display: flex; align-items: baseline; gap: 10px; margin-bottom: 10px; }
  .message-heading strong { font-size: 12px; }
  .message-heading span { color: #a0a69d; font: 10px ui-monospace, SFMono-Regular, Menlo, monospace; }
  .message-content { white-space: pre-wrap; overflow-wrap: anywhere; font-size: 13px; line-height: 1.7; color: #3c4239; }
  .user-message .message-content { color: #626a5f; }
  .streaming-label { color: #b9633c !important; letter-spacing: .04em; }
  .cursor { display: inline-block; width: 5px; height: 13px; background: #b9633c; vertical-align: -2px; animation: blink 1s steps(1) infinite; }
  @keyframes blink { 50% { opacity: 0; } }
  .composer-wrap { border-top: 1px solid #deded5; background: #f9f8f5; padding: 13px clamp(24px, 7vw, 110px) 17px; }
  .composer-error { color: #a34c30; font-size: 12px; padding: 0 0 9px; }
  .composer-meta { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }
  .composer-meta label, .composer-field span { color: #90978d; font: 640 10px ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing: .06em; }
  .composer-meta select { width: auto; max-width: 270px; border: 0; background: transparent; color: #4e564b; font-size: 11px; font-weight: 650; outline: 0; }
  .conversation-id { color: #a6aba2; margin-left: auto; font-size: 10px; }
  .composer-fields { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 10px; }
  .composer-field { display: flex; flex-direction: column; gap: 6px; }
  .composer-field em { color: #acb2a8; font-style: normal; font-weight: 400; }
  .composer-field input:not([type=checkbox]), .composer-field textarea { width: 100%; min-height: 40px; padding: 10px 12px; border: 1px solid #dedfd7; border-radius: 5px; outline: 0; background: white; font-size: 12px; color: #353d34; }
  .composer-field input:focus, .composer-field textarea:focus { border-color: #bb805d; }
  .composer-field input[type=checkbox] { width: 18px; height: 18px; accent-color: #ae613c; }
  .composer-bottom { display: flex; justify-content: space-between; align-items: center; margin-top: 10px; gap: 12px; }
  .composer-bottom span { color: #a0a69c; font-size: 10px; }
  .inspector-tabs { display: flex; border-bottom: 1px solid #e4e3dc; padding: 0 19px; gap: 21px; }
  .inspector-tabs button { border: 0; border-bottom: 2px solid transparent; background: transparent; padding: 11px 0; color: #939a90; font-size: 11px; font-weight: 640; }
  .inspector-tabs button.active { color: #a65634; border-bottom-color: #b76c46; }
  .run-list { margin-bottom: 10px; }
  .run-list button { display: flex; align-items: flex-start; gap: 8px; width: 100%; border: 0; background: transparent; text-align: left; padding: 9px 5px; border-radius: 4px; color: #4c5349; font: 11px ui-monospace, SFMono-Regular, Menlo, monospace; }
  .run-list button:hover, .run-list button.selected { background: #eeeae2; }
  .run-list small { display: block; color: #a0a69b; font-size: 10px; margin-top: 3px; }
  .run-status { width: 7px; height: 7px; border-radius: 50%; margin-top: 3px; background: #c69a68; flex: 0 0 7px; }
  .run-status.success { background: #6c9e75; }.run-status.failure { background: #c5755a; }
  .run-time { margin-left: auto; color: #a5aa9f; font-size: 10px; }
  .trace-row { display: flex; gap: 10px; min-height: 40px; position: relative; }
  .trace-line { width: 7px; height: 7px; border: 1.5px solid #b76c46; border-radius: 50%; flex: 0 0 7px; margin-top: 3px; }
  .trace-row:not(:last-child)::before { content: ''; position: absolute; width: 1px; top: 12px; bottom: 0; left: 3px; background: #deded5; }
  .trace-row strong { display: block; font: 600 11px ui-monospace, SFMono-Regular, Menlo, monospace; }.trace-row small { display: block; color: #a0a69b; font-size: 10px; margin: 3px 0 12px; }
  .trace-row pre, .variable-entry pre { white-space: pre-wrap; overflow-wrap: anywhere; color: #9c4a33; font-size: 10px; }
  .event-row { display: flex; gap: 11px; padding: 5px 0; color: #667065; font-size: 11px; }.event-row > span { color: #a9aea4; }.event-row small { display: block; color: #a8aea2; font-size: 10px; margin-top: 2px; }
  .note-entry { border-bottom: 1px solid #e5e3dc; padding: 2px 0 13px; margin-bottom: 15px; }.note-entry p { white-space: pre-wrap; font-size: 12px; line-height: 1.6; margin: 0 0 8px; }.note-entry textarea { width: 100%; border: 1px solid #deded5; padding: 7px; resize: vertical; }
  .note-actions { display: flex; justify-content: flex-end; gap: 10px; align-items: center; }.note-actions span { margin-right: auto; color: #a8ada4; font-size: 10px; }.note-actions button { padding: 0; border: 0; background: transparent; color: #a25e3f; font-size: 10px; }
  .variable-entry { display: flex; justify-content: space-between; border-bottom: 1px solid #e5e3dc; padding: 8px 0; }.variable-entry strong { font: 600 11px ui-monospace, SFMono-Regular, Menlo, monospace; }.variable-entry pre { margin: 5px 0 0; color: #6e766b; }
  @media (max-width: 760px) { .transcript { min-height: 300px; max-height: 55vh; } .composer-wrap { padding: 15px 20px; } }
</style>
