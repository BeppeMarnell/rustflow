<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '../lib/api.ts';
  import { blankNode, layoutNodes, newFlow, nextNodeId, NODE_SPECS, parseFlowJson, removeNode, renameNode, withEdges } from '../lib/flow.ts';
  import type { Binding, FlowDefinition, FlowNode, FlowSummary, NodeKind, ValueType } from '../lib/types.ts';

  export let selectedFlowId: string | null;
  export let flows: FlowSummary[];
  export let onPublished: () => void;

  let draft: FlowDefinition = newFlow();
  let name = 'Untitled flow';
  let originVersion: number | null = null;
  let selectedNodeId = '';
  let nodeIdDraft = '';
  let mode: 'graph' | 'json' = 'graph';
  let rawJson = '';
  let addKind: NodeKind = 'prompt.render';
  let busy = false;
  let notice = '';
  let error = '';
  let testOpen = false;
  let testInput: Record<string, string | boolean> = {};
  let testResult: { outputs: Record<string, unknown>; nodes: unknown[] } | null = null;

  onMount(() => {
    if (selectedFlowId) void loadPublished(selectedFlowId);
    else resetTestInput();
  });

  $: selectedNode = draft.nodes.find((node) => node.id === selectedNodeId) ?? null;
  $: positions = layoutNodes(draft);
  $: graphWidth = Math.max(680, ...positions.map((position) => position.x + 242));
  $: graphHeight = Math.max(430, ...positions.map((position) => position.y + 135));

  function messageOf(value: unknown) { return value instanceof Error ? value.message : 'Request failed.'; }
  function resetTestInput() {
    testInput = Object.fromEntries(Object.entries(draft.inputs).map(([key, type]) => [key, type === 'boolean' ? false : type === 'object' ? '{}' : '']));
  }
  function changed(next: FlowDefinition) { draft = withEdges(next); notice = ''; testResult = null; }
  function updateNode(id: string, update: (node: FlowNode) => FlowNode) {
    changed({ ...draft, nodes: draft.nodes.map((node) => node.id === id ? update(node) : node) });
  }
  function setConfig(key: string, value: string | number | undefined) {
    if (!selectedNode) return;
    updateNode(selectedNode.id, (node) => {
      const config = { ...node.config };
      if (value === undefined || value === '') delete config[key];
      else config[key] = value;
      return { ...node, config };
    });
  }
  function setBinding(input: string, encoded: string) {
    if (!selectedNode) return;
    updateNode(selectedNode.id, (node) => {
      const bindings = { ...node.bindings };
      if (encoded) bindings[input] = JSON.parse(encoded) as Binding;
      else delete bindings[input];
      return { ...node, bindings };
    });
  }
  function setGuard(id: string) {
    if (!selectedNode) return;
    updateNode(selectedNode.id, (node) => ({ ...node, run_if: id ? { node: id, equals: node.run_if?.equals ?? true } : undefined }));
  }
  function addNode() {
    const node = blankNode(addKind, nextNodeId(draft, addKind));
    changed({ ...draft, nodes: [...draft.nodes, node] });
    selectedNodeId = node.id;
    nodeIdDraft = node.id;
  }
  function selectNode(id: string) { selectedNodeId = id; nodeIdDraft = id; }
  function commitNodeId() {
    if (!selectedNode) return;
    try {
      const id = nodeIdDraft.trim();
      changed(renameNode(draft, selectedNode.id, id));
      selectedNodeId = id;
      error = '';
    } catch (failure) { error = messageOf(failure); nodeIdDraft = selectedNode.id; }
  }
  function deleteNode() {
    if (!selectedNode) return;
    changed(removeNode(draft, selectedNode.id));
    selectedNodeId = '';
  }
  function position(id: string) { return positions.find((item) => item.id === id); }
  function edgePath(from: string, to: string): string {
    const a = position(from), b = position(to);
    if (!a || !b) return '';
    const x1 = a.x + 194, y1 = a.y + 49, x2 = b.x, y2 = b.y + 49;
    const bend = Math.max(40, (x2 - x1) / 2);
    return `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`;
  }

  async function loadPublished(id: string) {
    try {
      const versions = await api.versions(id);
      if (!versions.versions.length) return;
      const latest = versions.versions[0];
      const result = await api.version(id, latest.version);
      draft = structuredClone(result.definition);
      name = flows.find((flow) => flow.flow_id === id)?.name ?? 'Untitled flow';
      originVersion = latest.version;
      resetTestInput();
    } catch (failure) { error = messageOf(failure); }
  }
  function editJson() { rawJson = JSON.stringify(withEdges(draft), null, 2); mode = 'json'; }
  function applyJson() {
    try { draft = parseFlowJson(rawJson); mode = 'graph'; resetTestInput(); error = ''; notice = 'JSON applied to draft.'; }
    catch (failure) { error = messageOf(failure); }
  }
  function resolvedDraft(): FlowDefinition {
    if (mode === 'json') { draft = parseFlowJson(rawJson); rawJson = JSON.stringify(draft, null, 2); }
    return withEdges(draft);
  }
  async function validate() {
    busy = true; error = ''; notice = '';
    try { await api.validate(resolvedDraft()); notice = 'Draft is valid and ready to publish.'; }
    catch (failure) { error = messageOf(failure); }
    finally { busy = false; }
  }
  async function publish() {
    if (!name.trim()) { error = 'Give the flow a name before publishing.'; return; }
    busy = true; error = ''; notice = '';
    try {
      const flow = resolvedDraft();
      await api.validate(flow);
      const published = await api.publish(name.trim(), flow);
      originVersion = published.version;
      notice = `Published immutable version ${published.version}. Further edits remain a draft.`;
      onPublished();
    } catch (failure) { error = messageOf(failure); }
    finally { busy = false; }
  }
  async function dryRun() {
    busy = true; error = ''; testResult = null;
    try {
      const input: Record<string, unknown> = {};
      for (const [key, type] of Object.entries(draft.inputs)) {
        input[key] = type === 'object' ? JSON.parse(String(testInput[key] || '{}')) : type === 'boolean' ? Boolean(testInput[key]) : String(testInput[key] ?? '');
      }
      testResult = await api.dryRun(resolvedDraft(), input);
      testOpen = true;
    } catch (failure) { error = messageOf(failure); testOpen = true; }
    finally { busy = false; }
  }

  function renameInput(oldName: string, newName: string) {
    const key = newName.trim();
    if (!key || (key !== oldName && key in draft.inputs)) return;
    const inputs = Object.fromEntries(Object.entries(draft.inputs).map(([name, type]) => [name === oldName ? key : name, type]));
    changed({ ...draft, inputs, nodes: draft.nodes.map((node) => node.type === 'input' && node.config.name === oldName ? { ...node, config: { ...node.config, name: key } } : node) });
    resetTestInput();
  }
  function addInput() {
    let index = 1; while (`input${index}` in draft.inputs) index++;
    changed({ ...draft, inputs: { ...draft.inputs, [`input${index}`]: 'string' } }); resetTestInput();
  }
  function removeInput(name: string) {
    const inputs = { ...draft.inputs }; delete inputs[name]; changed({ ...draft, inputs }); resetTestInput();
  }
  function renameOutput(oldName: string, newName: string) {
    const key = newName.trim();
    if (!key || (key !== oldName && key in draft.outputs)) return;
    changed({ ...draft, outputs: Object.fromEntries(Object.entries(draft.outputs).map(([name, binding]) => [name === oldName ? key : name, binding])) });
  }
  function addOutput() {
    let index = 1; while (`result${index}` in draft.outputs) index++;
    const node = draft.nodes.find((item) => item.type === 'output');
    changed({ ...draft, outputs: { ...draft.outputs, [`result${index}`]: { node: node?.id || '', output: 'value' } } });
  }
  function removeOutput(name: string) { const outputs = { ...draft.outputs }; delete outputs[name]; changed({ ...draft, outputs }); }
  function setOutput(name: string, encoded: string) {
    changed({ ...draft, outputs: { ...draft.outputs, [name]: encoded ? JSON.parse(encoded) as Binding : { node: '', output: 'value' } } });
  }
  function bindingOptions(exclude?: string) {
    return draft.nodes.filter((node) => node.id !== exclude).flatMap((node) => NODE_SPECS[node.type].outputs.map((output) => ({
      label: `${node.id} · ${output}`, value: JSON.stringify({ node: node.id, output })
    })));
  }
</script>

<div class="workbench flow-workbench">
  <section class="primary-pane">
    <div class="pane-header">
      <div class="pane-title"><div class="eyebrow">{originVersion ? `PUBLISHED v${originVersion} · DRAFT COPY` : 'UNPUBLISHED DRAFT'}</div><h1>{name}</h1></div>
      <div class="header-actions">
        <button class="button ghost" disabled={busy} onclick={() => void validate()}>Validate</button>
        <button class="button ghost" disabled={busy} onclick={() => { testOpen = !testOpen; }}>Test run</button>
        <button class="button primary" disabled={busy} onclick={() => void publish()}>Publish version ↗</button>
      </div>
    </div>
    {#if error || notice}<div class:error={!!error} class="flow-notice" role="status">{error || notice}</div>{/if}
    <div class="flow-toolbar">
      <div class="view-toggle"><button class:active={mode === 'graph'} onclick={() => { if (mode === 'json') applyJson(); else mode = 'graph'; }}>Graph</button><button class:active={mode === 'json'} onclick={editJson}>JSON</button></div>
      <div class="toolbar-right">
        <button class="button ghost" onclick={() => selectedNodeId = ''}>Flow settings</button>
        <select aria-label="Node type to add" bind:value={addKind}>
          {#each Object.entries(NODE_SPECS) as [kind, spec]}<option value={kind}>{spec.label}</option>{/each}
        </select>
        <button class="button" onclick={addNode}>＋ Add node</button>
      </div>
    </div>

    {#if mode === 'graph'}
      <div class="graph-scroll">
        <div class="graph-surface" style:width={graphWidth + 'px'} style:height={graphHeight + 'px'}>
          <svg class="graph-lines" width={graphWidth} height={graphHeight} aria-hidden="true">
            {#each draft.edges as edge}
              <path d={edgePath(edge.from.node, edge.to_node)} fill="none" stroke="#c7c9bd" stroke-width="1.5" marker-end="url(#arrow)" />
            {/each}
            {#each draft.nodes.filter((node) => node.run_if) as node}
              <path d={edgePath(node.run_if!.node, node.id)} fill="none" stroke="#bc916f" stroke-width="1.2" stroke-dasharray="4 5" />
            {/each}
            <defs><marker id="arrow" markerWidth="8" markerHeight="8" refX="6" refY="4" orient="auto"><path d="M 0 0 L 7 4 L 0 8" fill="none" stroke="#c7c9bd" stroke-width="1" /></marker></defs>
          </svg>
          {#each draft.nodes as node (node.id)}
            {@const at = position(node.id)}
            <button class:selected={selectedNodeId === node.id} class="graph-node" style:left={(at?.x ?? 0) + 'px'} style:top={(at?.y ?? 0) + 'px'} onclick={() => selectNode(node.id)}>
              <span class="node-kind">{NODE_SPECS[node.type].label}<span class="node-index">{String((at?.column ?? 0) + 1).padStart(2, '0')}</span></span>
              <strong>{node.id}</strong>
              <small>{node.type === 'llm.chat' ? String(node.config.model_alias || 'model') : node.type === 'prompt.render' ? String(node.config.template || 'template').slice(0, 26) : NODE_SPECS[node.type].description}</small>
              <i class="port port-in"></i><i class="port port-out"></i>
            </button>
          {/each}
        </div>
      </div>
    {:else}
      <div class="json-editor"><div class="json-caption">EDIT THE FULL VERSIONED DEFINITION · EDGES ARE DERIVED FROM BINDINGS</div><textarea aria-label="Flow JSON" bind:value={rawJson} spellcheck="false"></textarea><div><button class="button" onclick={applyJson}>Apply JSON to graph</button></div></div>
    {/if}

    {#if testOpen}
      <div class="test-panel">
        <div class="test-heading"><strong>Dry run</strong><span>Models are simulated. Nothing is published or stored.</span><button class="icon-button" aria-label="Close test panel" onclick={() => testOpen = false}>×</button></div>
        <div class="test-fields">
          {#each Object.entries(draft.inputs) as [key, type]}
            <label><span>{key} <em>{type}</em></span>{#if type === 'boolean'}<input type="checkbox" checked={Boolean(testInput[key])} onchange={(event) => { testInput[key] = event.currentTarget.checked; }} />{:else}<input type="text" value={String(testInput[key] ?? '')} oninput={(event) => { testInput[key] = event.currentTarget.value; }} placeholder={type === 'object' ? '{}' : 'Input value'} />{/if}</label>
          {/each}
        </div>
        <button class="button" disabled={busy} onclick={() => void dryRun()}>Run draft ↗</button>
        {#if testResult}<div class="test-result"><span>OUTPUT</span><pre>{JSON.stringify(testResult.outputs, null, 2)}</pre><details><summary>{testResult.nodes.length} node results</summary><pre>{JSON.stringify(testResult.nodes, null, 2)}</pre></details></div>{/if}
      </div>
    {/if}
  </section>

  <aside class="inspector">
    <div class="inspector-header"><div class="eyebrow">INSPECTOR</div><h2>{selectedNode ? NODE_SPECS[selectedNode.type].label : 'Flow settings'}</h2></div>
    <div class="inspector-scroll">
      {#if selectedNode}
        <p class="inspector-description">{NODE_SPECS[selectedNode.type].description}</p>
        <div class="field"><label for="node-id">NODE ID</label><input id="node-id" bind:value={nodeIdDraft} onblur={commitNodeId} onkeydown={(event) => { if (event.key === 'Enter') commitNodeId(); }} /></div>
        <div class="section-label">CONFIGURATION</div>
        {#if selectedNode.type === 'input'}
          <div class="field"><label for="input-name">FLOW INPUT</label><select id="input-name" value={String(selectedNode.config.name ?? '')} onchange={(event) => setConfig('name', event.currentTarget.value)}>{#each Object.keys(draft.inputs) as key}<option value={key}>{key}</option>{/each}</select></div>
        {:else if selectedNode.type === 'prompt.render'}
          <div class="field"><label for="prompt-template">TEMPLATE</label><textarea id="prompt-template" value={String(selectedNode.config.template ?? '')} oninput={(event) => setConfig('template', event.currentTarget.value)} rows="5"></textarea><p class="field-hint">Use {'{{text}}'} and optional {'{{question}}'} bindings.</p></div>
        {:else if selectedNode.type === 'llm.chat'}
          <div class="field"><label for="model-alias">MODEL ALIAS</label><select id="model-alias" value={String(selectedNode.config.model_alias ?? 'mock')} onchange={(event) => setConfig('model_alias', event.currentTarget.value)}><option value="mock">mock</option><option value="openai">openai</option></select></div>
          <div class="field-row"><div class="field"><label for="max-tokens">MAX OUTPUT TOKENS</label><input id="max-tokens" type="number" min="1" max="4096" value={Number(selectedNode.config.max_output_tokens ?? 512)} oninput={(event) => setConfig('max_output_tokens', Number(event.currentTarget.value))} /></div><div class="field"><label for="max-prompt">PROMPT BYTES</label><input id="max-prompt" type="number" min="1" max="65536" value={Number(selectedNode.config.max_prompt_bytes ?? 16384)} oninput={(event) => setConfig('max_prompt_bytes', Number(event.currentTarget.value))} /></div></div>
          {#if selectedNode.config.model_alias === 'openai'}<div class="field"><label for="credential-id">CREDENTIAL ID</label><input id="credential-id" value={String(selectedNode.config.credential_id ?? '')} oninput={(event) => setConfig('credential_id', event.currentTarget.value)} placeholder="Tenant credential UUID" /><p class="field-hint">Register the Vault reference through the API. Never enter a provider key here.</p></div>{/if}
        {:else if selectedNode.type === 'condition'}
          <div class="field"><label for="equals">EQUALS</label><input id="equals" value={String(selectedNode.config.equals ?? '')} oninput={(event) => setConfig('equals', event.currentTarget.value)} /></div>
        {:else if selectedNode.type === 'rag.retrieve'}
          <div class="field"><label for="top-k">TOP K</label><input id="top-k" type="number" min="1" max="10" value={Number(selectedNode.config.top_k ?? 3)} oninput={(event) => setConfig('top_k', Number(event.currentTarget.value))} /></div>
        {:else if selectedNode.type === 'variable.get' || selectedNode.type === 'variable.set'}
          <div class="field"><label for="variable-node-name">VARIABLE NAME</label><input id="variable-node-name" value={String(selectedNode.config.name ?? '')} oninput={(event) => setConfig('name', event.currentTarget.value)} /></div>
        {:else}<p class="empty-note">This node has no configuration fields.</p>{/if}
        {#if NODE_SPECS[selectedNode.type].inputs.length}
          <hr class="rule" /><div class="section-label">INPUT BINDINGS</div>
          {#each NODE_SPECS[selectedNode.type].inputs as input}
            <div class="field"><label for={'binding-' + input.name}>{input.name.toUpperCase()}{input.optional ? ' · OPTIONAL' : ''}</label><select id={'binding-' + input.name} value={selectedNode.bindings[input.name] ? JSON.stringify(selectedNode.bindings[input.name]) : ''} onchange={(event) => setBinding(input.name, event.currentTarget.value)}><option value="">Unbound</option>{#each bindingOptions(selectedNode.id) as option}<option value={option.value}>{option.label}</option>{/each}</select></div>
          {/each}
        {/if}
        <hr class="rule" /><div class="section-label">EXECUTION</div>
        <div class="field"><label for="node-guard">RUN IF CONDITION</label><select id="node-guard" value={selectedNode.run_if?.node ?? ''} onchange={(event) => setGuard(event.currentTarget.value)}><option value="">Always run</option>{#each draft.nodes.filter((node) => node.type === 'condition' && node.id !== selectedNode!.id) as node}<option value={node.id}>{node.id}</option>{/each}</select></div>
        {#if selectedNode.run_if}<div class="field"><label for="guard-equals">WHEN VALUE IS</label><select id="guard-equals" value={String(selectedNode.run_if.equals)} onchange={(event) => updateNode(selectedNode!.id, (node) => ({ ...node, run_if: { node: node.run_if!.node, equals: event.currentTarget.value === 'true' } }))}><option value="true">true</option><option value="false">false</option></select></div>{/if}
        <div class="field-row"><div class="field"><label for="retry">RETRIES</label><input id="retry" type="number" min="0" max="10" value={selectedNode.retry.max_attempts} oninput={(event) => updateNode(selectedNode!.id, (node) => ({ ...node, retry: { max_attempts: Number(event.currentTarget.value) } }))} /></div><div class="field"><label for="timeout">TIMEOUT MS</label><input id="timeout" type="number" min="1" value={selectedNode.timeout_ms} oninput={(event) => updateNode(selectedNode!.id, (node) => ({ ...node, timeout_ms: Number(event.currentTarget.value) }))} /></div></div>
        <hr class="rule" /><button class="button danger" onclick={deleteNode}>Delete node</button>
      {:else}
        <div class="field"><label for="flow-name">FLOW NAME</label><input id="flow-name" bind:value={name} /></div>
        <div class="field"><label for="flow-id">FLOW ID</label><input id="flow-id" class="mono" value={draft.flow_id} oninput={(event) => changed({ ...draft, flow_id: event.currentTarget.value })} /></div>
        <div class="field"><label for="schema-version">SCHEMA VERSION</label><input id="schema-version" type="number" min="1" value={draft.schema_version} oninput={(event) => changed({ ...draft, schema_version: Number(event.currentTarget.value) })} /></div>
        <hr class="rule" /><div class="section-label">LIMITS</div>
        <div class="field-row"><div class="field"><label for="max-nodes">MAX NODES</label><input id="max-nodes" type="number" min="1" value={draft.limits.max_nodes} oninput={(event) => changed({ ...draft, limits: { ...draft.limits, max_nodes: Number(event.currentTarget.value) } })} /></div><div class="field"><label for="max-parallel">PARALLEL NODES</label><input id="max-parallel" type="number" min="1" max="32" value={draft.limits.max_parallel_nodes} oninput={(event) => changed({ ...draft, limits: { ...draft.limits, max_parallel_nodes: Number(event.currentTarget.value) } })} /></div></div>
        <div class="field"><label for="max-run-ms">RUN TIMEOUT MS</label><input id="max-run-ms" type="number" min="1" value={draft.limits.max_run_ms} oninput={(event) => changed({ ...draft, limits: { ...draft.limits, max_run_ms: Number(event.currentTarget.value) } })} /></div>
        <hr class="rule" /><div class="section-label">FLOW INPUTS <button class="inline-add" onclick={addInput}>＋ Add</button></div>
        {#each Object.entries(draft.inputs) as [key, type]}
          <div class="mapping-row"><input aria-label={'Input name ' + key} value={key} onchange={(event) => renameInput(key, event.currentTarget.value)} /><select aria-label={'Input type ' + key} value={type} onchange={(event) => changed({ ...draft, inputs: { ...draft.inputs, [key]: event.currentTarget.value as ValueType } })}><option value="string">string</option><option value="boolean">boolean</option><option value="object">object</option></select><button class="icon-button" aria-label={'Remove input ' + key} onclick={() => removeInput(key)}>×</button></div>
        {/each}
        <hr class="rule" /><div class="section-label">FLOW OUTPUTS <button class="inline-add" onclick={addOutput}>＋ Add</button></div>
        {#each Object.entries(draft.outputs) as [key, binding]}
          <div class="output-mapping"><div class="mapping-row"><input aria-label={'Output name ' + key} value={key} onchange={(event) => renameOutput(key, event.currentTarget.value)} /><button class="icon-button" aria-label={'Remove output ' + key} onclick={() => removeOutput(key)}>×</button></div><select aria-label={'Output source ' + key} value={JSON.stringify(binding)} onchange={(event) => setOutput(key, event.currentTarget.value)}><option value="">Choose output node</option>{#each draft.nodes.filter((node) => node.type === 'output') as node}<option value={JSON.stringify({ node: node.id, output: 'value' })}>{node.id} · value</option>{/each}</select></div>
        {/each}
        <p class="field-hint">Flow outputs must point to output nodes. Edges follow node bindings automatically.</p>
      {/if}
    </div>
  </aside>
</div>

<style>
  .flow-notice { background: #e8f0e7; color: #4f7056; border-bottom: 1px solid #d8e2d7; padding: 9px 23px; font-size: 11px; }
  .flow-notice.error { background: #fae9df; color: #a24d31; border-color: #f0d4c2; }
  .flow-toolbar { display: flex; align-items: center; justify-content: space-between; gap: 12px; min-height: 48px; padding: 6px 18px 6px 23px; border-bottom: 1px solid #e1e1d9; background: #f8f7f3; }
  .view-toggle { display: flex; align-items: center; padding: 2px; border: 1px solid #e0e0d8; border-radius: 5px; background: #f0efea; }
  .view-toggle button { border: 0; background: transparent; color: #8c9389; padding: 6px 11px; border-radius: 3px; font-size: 11px; font-weight: 640; }
  .view-toggle button.active { color: #3a4237; background: #fff; box-shadow: 0 1px 3px #25292316; }
  .toolbar-right { display: flex; align-items: center; gap: 6px; }
  .toolbar-right select { border: 0; background: transparent; color: #626a5e; outline: 0; font-size: 11px; }
  .graph-scroll { flex: 1; min-height: 0; overflow: auto; background-color: #f4f3ef; background-image: radial-gradient(#d9dad2 0.7px, transparent 0.7px); background-size: 19px 19px; }
  .graph-surface { position: relative; min-width: 100%; min-height: 100%; }
  .graph-lines { position: absolute; inset: 0; pointer-events: none; }
  .graph-node { position: absolute; width: 194px; height: 99px; display: flex; flex-direction: column; align-items: flex-start; text-align: left; border: 1px solid #d4d7cb; border-radius: 7px; background: #fffefa; box-shadow: 0 3px 9px #343a3010; padding: 11px 13px; color: #343c32; transition: border-color .15s, box-shadow .15s, transform .15s; }
  .graph-node:hover { transform: translateY(-2px); box-shadow: 0 7px 15px #343a301c; }
  .graph-node.selected { border-color: #b76842; box-shadow: 0 0 0 2px #ead5c5, 0 7px 15px #343a3014; }
  .node-kind { width: 100%; display: flex; justify-content: space-between; font: 660 9px ui-monospace, SFMono-Regular, Menlo, monospace; color: #b36d48; letter-spacing: .08em; text-transform: uppercase; }
  .node-index { color: #b7bcb2; }
  .graph-node strong { font-size: 13px; margin-top: 11px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; max-width: 100%; }
  .graph-node small { color: #a3a99f; font-size: 10px; margin-top: 4px; text-overflow: ellipsis; overflow: hidden; white-space: nowrap; width: 100%; }
  .port { position: absolute; top: 45px; width: 8px; height: 8px; border: 2px solid #bcc3b8; border-radius: 50%; background: #f4f3ef; }.port-in { left: -5px; }.port-out { right: -5px; }
  .json-editor { flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 10px; padding: 17px 22px; background: #f4f3ef; }
  .json-caption { color: #9fa69c; font: 640 10px ui-monospace, SFMono-Regular, Menlo, monospace; letter-spacing: .07em; }
  .json-editor textarea { flex: 1; min-height: 300px; width: 100%; resize: none; border: 1px solid #d9dcd2; border-radius: 5px; background: #fffefa; color: #40483d; padding: 16px; font: 11px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace; outline: 0; }
  .test-panel { border-top: 1px solid #deded5; padding: 13px 21px 17px; background: #f9f8f5; max-height: 300px; overflow: auto; }
  .test-heading { display: flex; align-items: center; gap: 11px; margin-bottom: 11px; }.test-heading strong { font-size: 12px; }.test-heading span { color: #9da49a; font-size: 11px; }.test-heading button { margin-left: auto; }
  .test-fields { display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 10px; }.test-fields label { display: flex; flex-direction: column; gap: 5px; min-width: 140px; font-size: 10px; color: #727b70; }.test-fields label em { font-style: normal; color: #abb1a6; }.test-fields input[type=text] { border: 1px solid #daddd3; background: #fff; border-radius: 4px; padding: 8px; font-size: 11px; }
  .test-result { border-top: 1px solid #e4e4dd; margin-top: 12px; padding-top: 10px; }.test-result span { font: 650 10px ui-monospace, SFMono-Regular, Menlo, monospace; color: #7f897d; }.test-result pre { white-space: pre-wrap; overflow-wrap: anywhere; font: 11px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; }.test-result summary { color: #8d948a; font-size: 11px; cursor: pointer; }
  .inspector-description { margin: 0 0 19px; color: #969e93; font-size: 11px; line-height: 1.5; }
  .inline-add { border: 0; background: transparent; color: #a7613e; padding: 0; font: inherit; letter-spacing: 0; text-transform: none; }
  .mapping-row { display: flex; gap: 5px; align-items: center; margin: 0 0 7px; }.mapping-row input { width: 50%; min-width: 0; border: 1px solid #dcded5; border-radius: 4px; padding: 7px; font-size: 11px; }.mapping-row select { flex: 1; min-width: 0; border: 1px solid #dcded5; border-radius: 4px; padding: 7px 2px; font-size: 11px; }.mapping-row .icon-button { flex: 0 0 25px; }
  .output-mapping { padding-bottom: 12px; margin-bottom: 12px; border-bottom: 1px solid #e6e5de; }.output-mapping .mapping-row input { flex: 1; }.output-mapping > select { width: 100%; border: 1px solid #dcded5; border-radius: 4px; padding: 7px; font-size: 11px; background: #fff; }
  @media (max-width: 760px) { .flow-toolbar { padding: 8px 11px; } .toolbar-right { flex-wrap: wrap; justify-content: flex-end; } .graph-scroll { min-height: 410px; } }
</style>
