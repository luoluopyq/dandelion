/** View projections of the immutable project-memory ledger. */
export interface ProjectMemoryEvent {
  id: string;
  kind: string;
  authority: string;
  observedAt: string;
  revisionOf?: string;
  sourceRef?: { kind?: string; id?: string };
  validFrom?: string;
  validTo?: string;
  payload: Record<string, unknown>;
}
export interface MemoryPreviewPack {
  items: { sourceEventId: string; content: string; relevance: number; whySelected?: string[] }[];
  usedTokens: number;
  tokenBudget: number;
}
export interface MemoryHistoryRow {
  event: ProjectMemoryEvent;
  content: string;
  action: string;
  kind: string;
  current: boolean;
}
export type MemoryView = 'confirmed' | 'pending' | 'history';
export const MEMORY_CONTENT_BYTES = 4000;
export const memoryBytes = (text: string) => new TextEncoder().encode(text.trim()).length;
export const memoryText = (event: ProjectMemoryEvent) => String(event.payload.content ?? event.payload.text ?? '');
export function memoryKindLabel(kind: string) {
  return ({ fact: '事实', preference: '偏好', decision: '决策', observation: '观察', document: '资料', message: '对话' } as Record<string, string>)[kind] || '其他';
}
const latestFirst = (a: ProjectMemoryEvent, b: ProjectMemoryEvent) =>
  (Date.parse(b.observedAt) || 0) - (Date.parse(a.observedAt) || 0) || a.id.localeCompare(b.id);
export function projectMemoryView(events: ProjectMemoryEvent[]) {
  const byId = new Map(events.map(event => [event.id, event]));
  const superseded = new Set(events.flatMap(event => event.revisionOf ? [event.revisionOf] : []));
  const current = events.filter(event => event.kind !== 'retraction' && !superseded.has(event.id)).sort(latestFirst);
  const confirmed = current.filter(event => event.authority !== 'model_candidate');
  const pending = current.filter(event => event.authority === 'model_candidate');
  const currentIds = new Set(current.map(event => event.id));
  const history: MemoryHistoryRow[] = [...events].sort(latestFirst).map(event => {
    const prior = event.revisionOf ? byId.get(event.revisionOf) : undefined;
    const candidate = prior?.authority === 'model_candidate';
    let action = event.authority === 'model_candidate' ? '提出记忆' : '添加记忆';
    if (event.kind === 'retraction') action = candidate ? '忽略提议' : '忘记记忆';
    else if (event.sourceRef?.kind === 'candidate_confirmation') action = '确认记忆';
    else if (event.revisionOf) action = candidate ? '编辑并确认' : '编辑记忆';
    return { event, action, kind: event.kind === 'retraction' ? prior?.kind || 'other' : event.kind, current: currentIds.has(event.id), content: event.kind === 'retraction' ? (prior ? memoryText(prior) : '原记忆已不可读取') : memoryText(event) };
  });
  return { confirmed, pending, history };
}
export const matchesMemoryKind = (kind: string, filter: string) => filter === 'all' || (filter === 'other' ? !['fact','preference','decision'].includes(kind) : kind === filter);
export function filterMemoryEvents(events: ProjectMemoryEvent[], query: string, kind = 'all') {
  const text = query.trim().toLocaleLowerCase();
  return events.filter(event => matchesMemoryKind(event.kind, kind)
    && (!text || `${memoryText(event)} ${memoryKindLabel(event.kind)}`.toLocaleLowerCase().includes(text)));
}
export function resolveMemorySelection(events: ProjectMemoryEvent[], selectedId: string) {
  return events.some(event => event.id === selectedId) ? selectedId : events[0]?.id || '';
}
export function memoryRootSource(event: ProjectMemoryEvent, events: ProjectMemoryEvent[]) {
  const byId = new Map(events.map(item => [item.id, item]));
  const seen = new Set<string>();
  let root = event;
  while (root.revisionOf && byId.has(root.revisionOf) && !seen.has(root.id)) {
    seen.add(root.id);
    root = byId.get(root.revisionOf)!;
  }
  return root;
}
export function memorySourceLabel(event: ProjectMemoryEvent, events: ProjectMemoryEvent[]) {
  const root = memoryRootSource(event, events);
  const kind = root.sourceRef?.kind;
  if (kind === 'desktop_user') return '手动添加';
  if (kind === 'keyword_candidate') return '对话中提取';
  if (kind === 'agent_proposal') return '助手提议';
  if (root.authority === 'tool_verified') return '工具核对';
  return '已有记忆';
}
export function memorySourceSession(event: ProjectMemoryEvent, events: ProjectMemoryEvent[]) {
  const source = memoryRootSource(event, events).sourceRef;
  if (!['keyword_candidate','agent_proposal'].includes(source?.kind || '')) return null;
  const id = source?.id?.split(':')[0];
  return id && /^[a-zA-Z0-9_-]+$/.test(id) ? id : null;
}
