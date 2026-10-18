<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from './lib/api.ts';
  import type { ConversationSummary, FlowSummary } from './lib/types.ts';
  import FlowEditor from './components/FlowEditor.svelte';
  import ConversationView from './components/ConversationView.svelte';

  type Mode = 'conversations' | 'flows';
  let mode: Mode = 'conversations';
  let flows: FlowSummary[] = [];
  let conversations: ConversationSummary[] = [];
  let selectedFlowId: string | null = null;
  let selectedConversationId: string | null = null;
  let filter = '';
  let connection = 'Connecting';
  let error = '';

  async function refresh() {
    try {
      await api.health();
      const [flowResult, conversationResult] = await Promise.all([api.flows(), api.conversations()]);
      flows = flowResult.flows;
      conversations = conversationResult.conversations;
      connection = 'Connected';
      error = '';
    } catch (failure) {
      connection = 'Unavailable';
      error = failure instanceof Error ? failure.message : 'Could not connect to the API.';
    }
  }

  onMount(() => { void refresh(); });
  $: visibleFlows = flows.filter((flow) => flow.name.toLowerCase().includes(filter.toLowerCase()));
  $: visibleConversations = conversations.filter((conversation) =>
    `${conversation.flow_name} ${conversation.preview ?? ''}`.toLowerCase().includes(filter.toLowerCase())
  );

  function openFlow(id: string | null) {
    mode = 'flows';
    selectedFlowId = id;
  }

  function openConversation(id: string | null) {
    mode = 'conversations';
    selectedConversationId = id;
  }

  function dateLabel(value: string): string {
    return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(value));
  }
</script>

<svelte:head>
  <title>Rustflow Workbench</title>
  <meta name="description" content="Build flows and inspect conversations in the local Rustflow workbench." />
</svelte:head>

<div class="app-shell">
  <header class="topbar">
    <div class="brand" aria-label="Rustflow Workbench">
      <span class="brand-mark">r<span>.</span></span>
      <span class="brand-name">rustflow <span>workbench</span></span>
    </div>
    <div class="topbar-right">
      <span class:offline={connection !== 'Connected'} class="connection"><i></i>{connection}</span>
      <span class="topbar-divider"></span>
      <span class="local-label">LOCAL WORKSPACE</span>
    </div>
  </header>

  <div class="app-main">
    <aside class="sidebar">
      <div class="section-switch" aria-label="Workspace sections">
        <button class:active={mode === 'conversations'} onclick={() => { mode = 'conversations'; filter = ''; }}>
          <span class="switch-glyph">◎</span> Conversations <span class="switch-count">{conversations.length}</span>
        </button>
        <button class:active={mode === 'flows'} onclick={() => { mode = 'flows'; filter = ''; }}>
          <span class="switch-glyph">◇</span> Flows <span class="switch-count">{flows.length}</span>
        </button>
      </div>

      <div class="sidebar-heading">
        <span>{mode === 'conversations' ? 'RECENT CONVERSATIONS' : 'PUBLISHED FLOWS'}</span>
        <button class="subtle-icon" title="Refresh list" aria-label="Refresh list" onclick={() => void refresh()}>↻</button>
      </div>
      <div class="sidebar-search">
        <span>⌕</span><input aria-label="Filter list" placeholder="Filter {mode}…" bind:value={filter} />
      </div>
      <div class="sidebar-list">
        {#if mode === 'conversations'}
          <button class:current={selectedConversationId === null} class="list-item new-item" onclick={() => openConversation(null)}>
            <span class="new-symbol">＋</span><span>New conversation</span>
          </button>
          {#each visibleConversations as conversation (conversation.conversation_id)}
            <button class:current={selectedConversationId === conversation.conversation_id} class="list-item conversation-item" onclick={() => openConversation(conversation.conversation_id)}>
              <span class="item-title">{conversation.flow_name}</span>
              <span class="item-preview">{conversation.preview || 'No messages yet'}</span>
              <span class="item-meta"><span class:status-active={conversation.status === 'running' || conversation.status === 'queued'} class="tiny-dot"></span>{conversation.run_count} {conversation.run_count === 1 ? 'turn' : 'turns'}<span>{dateLabel(conversation.updated_at)}</span></span>
            </button>
          {/each}
        {:else}
          <button class:current={selectedFlowId === null} class="list-item new-item" onclick={() => openFlow(null)}>
            <span class="new-symbol">＋</span><span>New flow draft</span>
          </button>
          {#each visibleFlows as flow (flow.flow_id)}
            <button class:current={selectedFlowId === flow.flow_id} class="list-item" onclick={() => openFlow(flow.flow_id)}>
              <span class="item-title">{flow.name}</span>
              <span class="item-preview">{flow.flow_id.slice(0, 8)} · version {flow.latest_version ?? '—'}</span>
            </button>
          {/each}
        {/if}
      </div>
      <div class="sidebar-footer">API &amp; WORKER REQUIRED <span>v0.1</span></div>
    </aside>

    <main class="workspace">
      {#if error}
        <div class="connection-banner" role="alert">{error}<span>Start the API and worker, then refresh.</span></div>
      {/if}
      {#if mode === 'flows'}
        {#key selectedFlowId}
          <FlowEditor {selectedFlowId} {flows} onPublished={() => void refresh()} />
        {/key}
      {:else}
        {#key selectedConversationId}
          <ConversationView {selectedConversationId} {flows} onConversationCreated={(id) => { selectedConversationId = id; void refresh(); }} />
        {/key}
      {/if}
    </main>
  </div>
</div>
