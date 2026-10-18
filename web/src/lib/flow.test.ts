import assert from 'node:assert/strict';
import { test } from 'node:test';
import { layoutNodes, newFlow, parseFlowJson, removeNode, renameNode, withEdges } from './flow.ts';
import { SseParser } from './api.ts';

test('default draft preserves bindings and derives the exact edge list', () => {
  const flow = newFlow();
  assert.equal(flow.nodes.length, 4);
  assert.equal(flow.edges.length, 3);
  assert.deepEqual(withEdges(flow).edges, flow.edges);
  assert.equal(layoutNodes(flow).find((node) => node.id === 'reply')?.column, 3);
});

test('renaming and deleting a node updates every dependent reference', () => {
  const flow = newFlow();
  const renamed = renameNode(flow, 'model', 'assistant');
  assert.equal(renamed.nodes.find((node) => node.id === 'reply')?.bindings.value.node, 'assistant');
  assert.equal(renamed.edges[2].from.node, 'assistant');
  assert.throws(() => renameNode(flow, 'model', 'reply'));
  const removed = removeNode(renamed, 'assistant');
  assert.equal(removed.nodes.find((node) => node.id === 'reply')?.bindings.value, undefined);
  assert.equal(removed.edges.length, 1);
});

test('JSON import checks the top-level contract', () => {
  assert.equal(parseFlowJson(JSON.stringify(newFlow())).nodes.length, 4);
  assert.throws(() => parseFlowJson('[]'));
  assert.throws(() => parseFlowJson('{}'));
});

test('SSE parser handles chunks, multiple events and multiline data', () => {
  const parser = new SseParser();
  assert.deepEqual(parser.feed('id: 1\nevent: token\ndata: {"meta'), []);
  const events = parser.feed('data":1}\n\nid: 2\r\nevent: run.completed\r\ndata: one\r\ndata: two\r\n\r\n');
  assert.deepEqual(events, [
    { id: '1', type: 'token', data: '{"metadata":1}' },
    { id: '2', type: 'run.completed', data: 'one\ntwo' }
  ]);
  assert.deepEqual(parser.feed('event: token\r'), []);
  assert.deepEqual(parser.feed('\ndata: {"metadata":{"token":"hi"}}\r\n\r\n'), [
    { id: '', type: 'token', data: '{"metadata":{"token":"hi"}}' }
  ]);
});
