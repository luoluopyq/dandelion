import type { KnowledgeItem } from './types';
export type KnowledgeFilter = 'all' | 'recent' | 'pending' | 'favorites' | 'topic';
export const KNOWLEDGE_TYPES: Record<string,string> = { article:'文章', note:'笔记', code:'代码', link:'链接', file:'文件', video:'视频', image:'图片' };
export function knowledgePending(item: KnowledgeItem): boolean {
  return ['pending','failed'].includes(item.parseStatus) || ['pending','failed'].includes(item.classificationStatus) || item.suggestedTopics.length > 0;
}
export function knowledgeStatus(item: KnowledgeItem): string {
  if (!item.enabled) return '已保存，尚未用于对话';
  if (item.parseStatus === 'empty') return '原件无正文，标题和笔记可检索';
  if (item.parseStatus === 'unsupported') return '仅标题和笔记可检索';
  if (item.parseStatus === 'failed') return '原件已保存，正文待处理';
  if (item.parseStatus === 'pending') return '原件已保存，等待解析';
  return item.modelAllowed ? '可检索，可供模型引用' : '本地可检索';
}
export function filterKnowledgeItems(items: KnowledgeItem[], filter: KnowledgeFilter, topic: string, kind: string, now = Date.now()): KnowledgeItem[] {
  return items.filter(item => (!kind || item.kind === kind)
    && (filter !== 'favorites' || item.favorite)
    && (filter !== 'pending' || knowledgePending(item))
    && (filter !== 'topic' || item.topics.includes(topic))
    && (filter !== 'recent' || now - new Date(item.updatedAt).getTime() <= 7 * 86400000));
}
export function parseTopics(value: string): string[] {
  const values = value.split(/[,，、\n]/).map(t=>t.trim()).filter(Boolean);
  return [...new Map(values.map(t=>[t.toLocaleLowerCase(),t])).values()].slice(0,8);
}
