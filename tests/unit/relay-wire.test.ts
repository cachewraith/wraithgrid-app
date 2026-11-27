// Desktop chat state → relay messages (src/renderer/lib/relay/wire.ts): every message must fit
// one sealed frame, however long the chat or the project list.
import { describe, expect, it } from 'vitest'
import { PLAINTEXT_MAX } from '../../src/shared/relay/crypto'
import { ToWeb } from '../../src/shared/relay/messages'
import { EMPTY_THREAD, type ChatItem, type ChatThread } from '../../src/renderer/lib/chat-thread'
import { defaultConfig } from '../../src/shared/schema'
import type { ChatCommand } from '../../src/shared/chat'
import type { Config, Workspace } from '../../src/shared/types'
import { ToDesktop } from '../../src/shared/relay/messages'
import {
  COMMANDS_BUDGET,
  ITEM_TEXT_CAP,
  markSent,
  snapshot,
  state,
  updates,
  wireCommands,
  wireItem,
  wireSettings
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

describe('project icons in state', () => {
  const config = (icons: string[]): Config => {
    const base = defaultConfig()
    const ws = base.workspaces[0]!
    return {
      ...base,
      workspaces: icons.map((icon, i): Workspace => ({ ...ws, id: `ws${i}`, name: `p${i}`, icon }))
    }
  }
  const input = (c: Config) => ({
    config: c,
    isChat: () => true,
    thread: () => EMPTY_THREAD,
    activeAt: () => null
  })

  it('sends a known icon id, and leaves out the folder, emoji and unknown sets', () => {
    const s = state('pc', input(config(['mit:react', '', 'ms:folder', '🚀', 'web:evil'])))
    expect(s.projects.map((p) => p.icon)).toEqual([
      'mit:react',
      undefined,
      undefined,
      undefined,
      undefined
    ])
    expect(ToWeb.safeParse(s).success).toBe(true)
  })
})

describe('the CLI’s commands', () => {
  const cmd = (name: string, over: Partial<ChatCommand> = {}): ChatCommand => ({
    name,
    description: `Does ${name}`,
    argumentHint: '',
    aliases: [],
    builtin: false,
    ...over
  })

  it('lists what the desktop’s menu lists from the CLI: no terminal-only ones, none of ours', () => {
    const t = {
      ...EMPTY_THREAD,
      commands: [
        cmd('review', { argumentHint: '<pr>' }),
        cmd('compact', { builtin: true }),
        cmd('config', { builtin: true }),
        cmd('model', { builtin: true }),
        cmd('plan-ideas', { mention: true })
      ],
      terminalOnly: ['config']
    }
    expect(wireCommands(t)).toEqual([
      { name: 'review', description: 'Does review', hint: '<pr>', sigil: '/', builtin: false },
      { name: 'plan-ideas', description: 'Does plan-ideas', hint: '', sigil: '$', builtin: false },
      { name: 'compact', description: 'Does compact', hint: '', sigil: '/', builtin: true }
    ])
  })

  it('keeps the list within its budget, and a snapshot with it within one frame', () => {
    const many = Array.from({ length: 400 }, (_, i) =>
      cmd(`skill-${i}`, { description: '😀'.repeat(400) })
    )
    const t = {
      ...EMPTY_THREAD,
      commands: many,
      items: Array.from({ length: 50 }, (_, i): ChatItem => ({
        kind: 'assistant',
        id: `a${i}`,
        text: '😀'.repeat(6000)
      }))
    }
    const list = wireCommands(t)
    expect(list.length).toBeGreaterThan(10)
    expect(list.length).toBeLessThanOrEqual(200)
    expect(size(list)).toBeLessThanOrEqual(COMMANDS_BUDGET)
    const snap = snapshot('p', t)
    expect(snap.commands).toEqual(list)
    expect(size({ v: 1, n: Date.now() * 1000, m: snap })).toBeLessThanOrEqual(PLAINTEXT_MAX)
    expect(ToWeb.safeParse(snap).success).toBe(true)
  })

  it('leaves commands out of a snapshot until the CLI has said them', () => {
    expect(snapshot('p', thread([])).commands).toBeUndefined()
  })
})

describe('tool details, questions and settings (2.5.3)', () => {
  it('sends a tool’s details, cut, and drops them when the item would not fit a frame', () => {
    const w = wireItem(tool({ preview: { type: 'code', text: 'x'.repeat(10_000) } }))
    expect(w.preview).toEqual({ type: 'code', text: 'x'.repeat(6000) })
    const huge = wireItem(
      tool({
        output: '€'.repeat(ITEM_TEXT_CAP),
        preview: { type: 'diff', path: 'a', before: '€'.repeat(6000), after: '€'.repeat(6000) }
      })
    )
    expect(huge.preview).toBeUndefined()
    expect(size({ v: 1, n: Date.now() * 1000, m: huge })).toBeLessThanOrEqual(PLAINTEXT_MAX)
  })

  it('counts a user message’s images', () => {
    expect(wireItem({ kind: 'user', id: 'u', text: 'hi', images: 2 }).images).toBe(2)
    expect(wireItem({ kind: 'user', id: 'u', text: 'hi', images: 0 }).images).toBeUndefined()
  })

  it('offers only modes that still ask, of those the CLI has', () => {
    const s = wireSettings(
      { model: 'haiku', effort: 'high', chatMode: 'bypassPermissions' },
      { models: [], controls: null }
    )
    expect(s).toMatchObject({ model: 'haiku', effort: 'high', mode: 'bypassPermissions' })
    expect(s.modes).toEqual(['default', 'acceptEdits', 'plan', 'dontAsk'])
    const agy = wireSettings(
      { model: null, effort: null, chatMode: null },
      {
        models: [],
        controls: {
          live: { interrupt: false, model: false, mode: false },
          modes: ['default', 'plan'],
          approvals: false
        }
      }
    )
    expect(agy.modes).toEqual(['default', 'plan'])
    expect(agy.mode).toBe('default')
  })

  it('checks the new web messages', () => {
    const ok = (m: unknown) => ToDesktop.safeParse(m).success
    expect(ok({ kind: 'chat.send', req: 'r', paneId: 'p', text: '' })).toBe(false)
    expect(ok({ kind: 'chat.send', req: 'r', paneId: 'p', text: '', images: ['u'] })).toBe(true)
    expect(ok({ kind: 'chat.send', req: 'r', paneId: 'p', text: 'hi' })).toBe(true)
    expect(ok({ kind: 'chat.set', req: 'r', paneId: 'p', model: 'a', mode: 'plan' })).toBe(false)
    expect(ok({ kind: 'chat.set', req: 'r', paneId: 'p', effort: null })).toBe(true)
    expect(ok({ kind: 'chat.answer', req: 'r', paneId: 'p', ask: 'q', skip: true })).toBe(true)
    expect(
      ok({ kind: 'chat.answer', req: 'r', paneId: 'p', ask: 'q', skip: true, answers: { a: 'b' } })
    ).toBe(false)
    expect(
      ok({
        kind: 'chat.image',
        upload: 'u',
        part: 0,
        parts: 1,
        mediaType: 'image/svg+xml',
        data: 'AAAA'
      })
    ).toBe(false)
    expect(
      ok({ kind: 'chat.image', upload: 'u', part: 0, parts: 1, mediaType: 'image/png', data: '<>' })
    ).toBe(false)
  })
})
