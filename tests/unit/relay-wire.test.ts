// Desktop chat state → relay messages (src/renderer/lib/relay/wire.ts): every message must fit
// one sealed frame, however long the chat or the project list.
import { describe, expect, it } from 'vitest'
import { PLAINTEXT_MAX } from '../../src/shared/relay/crypto'
import { ToWeb } from '../../src/shared/relay/messages'
import { EMPTY_THREAD, type ChatItem, type ChatThread } from '../../src/renderer/lib/chat-thread'
import {
  ITEM_TEXT_CAP,
  markSent,
  snapshot,
  updates,
  wireItem
} from '../../src/renderer/lib/relay/wire'

const size = (m: unknown): number => new TextEncoder().encode(JSON.stringify(m)).length
const thread = (items: ChatItem[], busy = false): ChatThread => ({ ...EMPTY_THREAD, items, busy })
const tool = (over: Partial<Extract<ChatItem, { kind: 'tool' }>>): ChatItem =>
  ({
    kind: 'tool',
    id: 't1',
    name: 'Bash',
    toolKind: 'shell',
    subtitle: 'ls',
    preview: null,
    output: null,
    isError: false,
    approval: null,
    ...over
  }) as ChatItem

describe('wireItem', () => {
  it('names a tool call’s state and keeps the end of long output', () => {
    expect(wireItem(tool({})).tool?.state).toBe('running')
    expect(wireItem(tool({ output: 'ok' })).tool?.state).toBe('done')
    expect(wireItem(tool({ output: 'no', isError: true })).tool?.state).toBe('error')
    expect(
      wireItem(tool({ approval: { reqId: 'r', canAlways: false, decided: null } })).tool?.state
    ).toBe('approval')
    const long = wireItem(tool({ output: 'a'.repeat(ITEM_TEXT_CAP) + 'END' }))
    expect(long.cut).toBe(true)
    expect(long.text.endsWith('END')).toBe(true)
    const msg = wireItem({ kind: 'assistant', id: 'a', text: 'START' + 'b'.repeat(ITEM_TEXT_CAP) })
    expect(msg.text.startsWith('START')).toBe(true)
    expect(
      wireItem({ kind: 'turn', id: 'x', costUsd: null, durationMs: 4200, error: null }).text
    ).toBe('Done in 4s')
  })
})

describe('snapshot and updates', () => {
  const big = (n: number): ChatItem[] =>
    Array.from({ length: n }, (_, i) => ({
      kind: 'assistant',
      id: `a${i}`,
      text: '😀'.repeat(6000)
    }))

  it('fits a snapshot of a huge chat into one frame, newest items first, marked more', () => {
    const s = snapshot('p', thread(big(50)))
    expect(size({ v: 1, n: Date.now() * 1000, m: s })).toBeLessThanOrEqual(PLAINTEXT_MAX)
    expect(s.more).toBe(true)
    expect(s.items.at(-1)?.id).toBe('a49')
    expect(ToWeb.safeParse(s).success).toBe(true)
  })

  it('sends only what changed, split into frames that fit; nothing when nothing changed', () => {
    const t = thread(big(3))
    const sent = new Map<string, string>()
    markSent(sent, snapshot('p', thread([])).items)
    const frames = updates('p', t, sent, false)
    expect(frames.length).toBeGreaterThan(1)
    for (const f of frames) expect(size({ v: 1, n: 1, m: f })).toBeLessThanOrEqual(PLAINTEXT_MAX)
    expect(frames.flatMap((f) => f.items.map((i) => i.id))).toEqual(['a0', 'a1', 'a2'])
    expect(updates('p', t, sent, false)).toEqual([])
    // Only busy changed: one frame with no items.
    expect(updates('p', { ...t, busy: true }, sent, false)).toEqual([
      { kind: 'chat.update', paneId: 'p', items: [], busy: true }
    ])
  })
})
