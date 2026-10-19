import test from 'node:test';
import assert from 'node:assert/strict';
import { parseKnowledgeCitation } from '../node_modules/.wiki-view-test/knowledge-citations.js';
test('知识引用只解析稳定空间和片段标识，支持导入与迁移的片段', () => {
  assert.deepEqual(parseKnowledgeCitation('#knowledge/space_ab/chunk_123'), { spaceId:'space_ab',chunkId:'chunk_123' });
  assert.deepEqual(parseKnowledgeCitation('#knowledge/space-abc/chunk_m_doc_0'),{spaceId:'space-abc',chunkId:'chunk_m_doc_0'});
});
test('外部地址、路径穿越、脚本和附加参数不能被解释为内部引用', () => {
  for (const href of ['https://example.com/#knowledge/s/c','#knowledge/s/../c','#knowledge/s/c?path=/tmp/a','#knowledge/s/c/more','javascript:alert(1)','#knowledge/%2e%2e/c','#knowledge/s/<script>']) {
    assert.equal(parseKnowledgeCitation(href),null,href);
  }
});
test('知识文章允许编码后的单个 Markdown 文件名，拒绝目录跳转', () => {
  assert.deepEqual(parseKnowledgeCitation('#knowledge-page/space_ab/%E7%AC%94%E8%AE%B0.md'), {spaceId:'space_ab',page:'笔记.md'});
  for (const href of ['#knowledge-page/s/%2E%2E%2Fx.md','#knowledge-page/s/C%3A%5Cprivate.md','#knowledge-page/s/%00x.md','#knowledge-page/s/%GG.md']) {
    assert.equal(parseKnowledgeCitation(href), null, href);
  }
});
