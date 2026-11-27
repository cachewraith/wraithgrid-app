// The desktop end of the relay (src/renderer/lib/relay/host.ts) against a fake server socket,
// a fake store and chat bus, with the real crypto: a browser pairs, proves the pairing link's code,
// then reads and drives chats; anything unproven or replayed is ignored.
import { describe, expect, it, vi } from 'vitest'
import { defaultConfig } from '../../src/shared/schema'
import type { Config, Pane, Workspace } from '../../src/shared/types'
import { channel, newKeyPair, pairingCode } from '../../src/shared/relay/crypto'
import { Counter, decode, encode, parsePairingLink, ToWeb } from '../../src/shared/relay/messages'
import { RelayHost, type RelayApi } from '../../src/renderer/lib/relay/host'
import { EMPTY_THREAD, type ChatThread } from '../../src/renderer/lib/chat-thread'

const PAIR = 'pair_test1'

function setup() {
  const pane = {
    id: 'p1',
    accountId: 'a1',
    cwd: '~/code/api',
    title: 'Fix login',
    view: 'chat',
    shell: false
  } as unknown as Pane
  const ws = {
    id: 'ws1',
    name: 'api',
    folder: '~/code/api',
    accountId: 'a1',
    machineId: null,
    panes: [pane],
    history: []
  } as unknown as Workspace
  const config: Config = {
    ...defaultConfig(),
    accounts: [
      { id: 'a1', name: 'work', cli: 'claude', signedIn: true } as Config['accounts'][number]
    ],
    workspaces: [ws],
    activeWorkspace: 'ws1'
  }
  const runtime: Record<string, { started: boolean; exit: null; activeAt: number | null }> = {}
  const actions = {
    requestStart: vi.fn((id: string) => {
      runtime[id] = { started: true, exit: null, activeAt: null }
    }),
    chatSend: vi.fn(),
    chatInterrupt: vi.fn(),
    chatApprove: vi.fn(),
    chatSetMode: vi.fn(),
    chatSetModel: vi.fn(),
    chatSetEffort: vi.fn(),
    switchWorkspace: vi.fn(),
    createPane: vi.fn(() => {
      ws.panes.push({ ...pane, id: 'p2', title: 'New chat' } as Pane)
      return 'p2'
    })
  }
  const store = {
    getState: () => ({ config, runtime, ...actions }),
    setState: () => undefined,
    subscribe: () => () => undefined,
    getInitialState: () => undefined
  }
  let thread: ChatThread = {
    ...EMPTY_THREAD,
    items: [{ kind: 'user', id: 'u1', text: 'hello', images: 0 }]
  }
  const subs = new Set<() => void>()
  const chat = {
    thread: () => thread,
    load: vi.fn(async () => undefined),
    subscribe: (_id: string, cb: () => void) => {
      subs.add(cb)
      return () => subs.delete(cb)
    }
  }
  const setThread = (t: ChatThread): void => {
    thread = t
    for (const cb of subs) cb()
  }

  const sent: Record<string, unknown>[] = []
  const sock = {
    readyState: 1,
    send: (t: string) => sent.push(JSON.parse(t)),
    close: vi.fn(),
    onopen: null as ((e: Event) => void) | null,
    onmessage: null as ((e: MessageEvent) => void) | null,
    onclose: null as ((e: CloseEvent) => void) | null,
    onerror: null as ((e: Event) => void) | null
  }
  const frame = (f: unknown): void => sock.onmessage!({ data: JSON.stringify(f) } as MessageEvent)

  let saved: unknown = null
  const requestKey = vi.fn(async (_id: string, _key: string) => ({ ok: true as const }))
  const requestAnswer = vi.fn(async (_id: string, _allow: boolean) => ({ ok: true as const }))
  const listed: {
    id: string
    status: string
    phoneName: string | null
    phoneKey: string | null
  }[] = []
  const api: RelayApi = {
    // Like the server: a new pairing is listed as pending until claimed.
    relayPair: async () => {
      listed.splice(0, listed.length, {
        id: PAIR,
        status: 'pending',
        phoneName: null,
        phoneKey: null
      })
      return {
        ok: true,
        pairing: {
          id: PAIR,
          deviceName: 'Work laptop',
          expiresAt: new Date(Date.now() + 6e5).toISOString()
        },
        code: 'C'.repeat(43),
        server: 'https://api.example.com',
        appUrl: 'https://app.example.com'
      }
    },
    relayPairings: async () => ({ ok: true, pairings: listed }),
    relayUnpair: async () => ({ ok: true }),
    relayConnect: async () => ({ ok: true }),
    relaySend: async () => ({ ok: true }),
    relayClose: async () => ({ ok: true }),
    onRelay: () => () => undefined,
    relayLoad: async () => saved,
    relaySave: async (d) => {
      saved = JSON.parse(JSON.stringify(d))
      return { ok: true }
    },
    relayRequestKey: requestKey,
    relayRequestAnswer: requestAnswer,
    status: async () => ({
      server: '',
      signedIn: true,
      user: null,
      deviceId: 'dev_1',
      pending: false
    }),
    onChanged: () => () => undefined
  }
  const host = new RelayHost({
    api,
    store: store as never,
    chat: chat as never,
    openSocket: async () => sock as never,
    timers: { state: 10_000, flush: 1, startWait: 1000 }
  })
  return {
    host,
    sent,
    sock,
    frame,
    actions,
    setThread,
    saved: () => saved,
    config,
    listed,
    requestKey,
    requestAnswer
  }
}

const tick = (ms = 5): Promise<void> => new Promise((r) => setTimeout(r, ms))

/** Pairs like the web app: claim (server says so), then `pair.confirm` sealed to the desktop. */
async function paired(t: ReturnType<typeof setup>, code = 'C'.repeat(43)) {
  await t.host.start()
  const r = await t.host.pair()
  if (!r.ok) throw new Error(r.error)
  await tick()
  t.sock.onopen?.({} as Event)
  t.frame({ t: 'welcome', v: 1, connectionId: 'c', role: 'desktop' })
  const link = parsePairingLink(r.link)!
  const phone = newKeyPair()
  t.listed.splice(0, 1, {
    id: PAIR,
    status: 'active',
    phoneName: 'Phone',
    phoneKey: phone.publicKey
  })
  t.frame({ t: 'pairing.claimed', pairing: PAIR, phoneName: 'Phone', phoneKey: phone.publicKey })
  const ch = channel('web', phone.secretKey, link.desktopKey, PAIR)
  const out = new Counter()
  const inc = new Counter()
  const raw = (m: unknown): string => encode(ch, out, m as never)
  const say = (m: unknown): void => t.frame({ t: 'data', pairing: PAIR, seq: 1, payload: raw(m) })
  say({ kind: 'pair.confirm', code, phoneKey: phone.publicKey, phoneName: 'My phone' })
  await tick()
  const heard = (): ToWeb[] =>
    t.sent
      .filter((f) => f.t === 'data')
      .map((f) => decode(ch, inc, String(f.payload), ToWeb))
      .filter((m): m is ToWeb => m !== null)
  return { say, heard, link, raw }
}

describe('RelayHost', () => {
  it('sends hello, shows a pairing link, and trusts the browser only after the right code', async () => {
    const t = setup()
    const { say, heard, link } = await paired(t)
    expect(t.sent[0]).toEqual({ t: 'hello', v: [1] })
    expect(link).toMatchObject({
      pairingId: PAIR,
      server: 'https://api.example.com',
      deviceName: 'Work laptop'
    })
    expect(t.host.getView().pairings).toEqual([
      { id: PAIR, name: 'My phone', confirmed: true, online: true }
    ])
    // Saved owner-only by main: keys and the confirmed state, not the code any more.
    const saved = t.saved() as {
      pairings: Record<string, { code: string | null; confirmed: boolean }>
    }
    expect(saved.pairings[PAIR]).toMatchObject({ code: null, confirmed: true })

    say({ kind: 'sync' })
    await tick()
    const [st] = heard()
    expect(st).toMatchObject({
      kind: 'state',
      device: 'Work laptop',
      projects: [
        {
          id: 'ws1',
          name: 'api',
          folder: '~/code/api',
          canChat: true,
          chats: [{ paneId: 'p1', title: 'Fix login', cli: 'claude', account: 'work', busy: false }]
        }
      ]
    })
  })

  it('ignores everything from a browser that sent the wrong code', async () => {
    const t = setup()
    const { say, heard } = await paired(t, 'D'.repeat(43))
    expect(t.host.getView().pairings[0]).toMatchObject({ confirmed: false })
    say({ kind: 'sync' })
    say({ kind: 'chat.send', req: 'r1', paneId: 'p1', text: 'rm -rf' })
    await tick(30)
    expect(heard()).toEqual([])
    expect(t.actions.chatSend).not.toHaveBeenCalled()
  })

  it('opens a chat, streams what changes, sends a message, stops a turn', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    say({ kind: 'chat.open', paneId: 'p1' })
    await tick()
    expect(heard().at(-1)).toMatchObject({
      kind: 'chat.snapshot',
      paneId: 'p1',
      items: [{ id: 'u1', kind: 'user', text: 'hello' }],
      busy: false,
      more: false
    })

    t.setThread({
      ...EMPTY_THREAD,
      busy: true,
      items: [
        { kind: 'user', id: 'u1', text: 'hello', images: 0 },
        { kind: 'assistant', id: 'a1', text: 'Hi there' }
      ]
    })
    await tick(20)
    const all = heard()
    expect(all.at(-1)).toEqual({
      kind: 'chat.update',
      paneId: 'p1',
      busy: true,
      items: [{ id: 'a1', kind: 'assistant', text: 'Hi there' }]
    })

    say({ kind: 'chat.send', req: 'r1', paneId: 'p1', text: 'next step' })
    await tick(20)
    expect(t.actions.requestStart).toHaveBeenCalledWith('p1')
    expect(t.actions.chatSend).toHaveBeenCalledWith('p1', 'next step', [])
    expect(heard().at(-1)).toEqual({ kind: 'ack', req: 'r1', ok: true })

    say({ kind: 'chat.stop', req: 'r2', paneId: 'p1' })
    await tick()
    expect(t.actions.chatInterrupt).toHaveBeenCalledWith('p1')

    say({ kind: 'chat.send', req: 'r3', paneId: 'nope', text: 'x' })
    await tick()
    expect(heard().at(-1)).toEqual({
      kind: 'ack',
      req: 'r3',
      ok: false,
      error: 'That chat is gone.'
    })
  })

  it('sends the CLI’s commands once it has said them, and only when they change', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    say({ kind: 'chat.open', paneId: 'p1' })
    await tick()
    expect(heard().at(-1)).not.toHaveProperty('commands')

    const items = [{ kind: 'user' as const, id: 'u1', text: 'hello', images: 0 }]
    const commands = [
      { name: 'review', description: 'Review a PR', argumentHint: '', aliases: [], builtin: false }
    ]
    t.setThread({ ...EMPTY_THREAD, items, commands })
    await tick(20)
    expect(heard().at(-1)).toEqual({
      kind: 'chat.update',
      paneId: 'p1',
      items: [],
      busy: false,
      commands: [
        { name: 'review', description: 'Review a PR', hint: '', sigil: '/', builtin: false }
      ]
    })

    const before = heard().length
    t.setThread({ ...EMPTY_THREAD, items, commands, busy: true })
    await tick(20)
    expect(
      heard()
        .slice(before)
        .some((m) => 'commands' in m)
    ).toBe(false)
  })

  it('answers claude’s question from the web, but never another approval', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    const ask = (reqId: string, questions?: boolean) => ({
      kind: 'tool' as const,
      id: `t-${reqId}`,
      name: questions ? 'AskUserQuestion' : 'Bash',
      toolKind: 'other' as const,
      subtitle: '',
      preview: null,
      output: null,
      isError: false,
      approval: {
        reqId,
        canAlways: false,
        decided: null,
        ...(questions
          ? {
              questions: [
                {
                  question: 'Which database?',
                  header: 'DB',
                  multiSelect: false,
                  options: [
                    { label: 'Postgres', description: '' },
                    { label: 'SQLite', description: '' }
                  ]
                }
              ]
            }
          : {})
      }
    })
    t.setThread({ ...EMPTY_THREAD, items: [ask('q1', true), ask('b1')] })
    say({ kind: 'chat.open', paneId: 'p1' })
    await tick()
    const snap = heard().at(-1) as Extract<ToWeb, { kind: 'chat.snapshot' }>
    expect(snap.items[0]?.ask).toEqual({
      id: 'q1',
      questions: [
        {
          question: 'Which database?',
          header: 'DB',
          multiSelect: false,
          options: [
            { label: 'Postgres', description: '' },
            { label: 'SQLite', description: '' }
          ]
        }
      ]
    })
    expect(snap.items[1]?.ask).toBeUndefined()

    say({ kind: 'chat.answer', req: 'a1', paneId: 'p1', ask: 'b1', answers: { x: 'yes' } })
    await tick()
    expect(heard().at(-1)).toMatchObject({ kind: 'ack', req: 'a1', ok: false })
    say({ kind: 'chat.answer', req: 'a2', paneId: 'p1', ask: 'q1', answers: { Other: 'x' } })
    await tick()
    expect(heard().at(-1)).toMatchObject({ kind: 'ack', req: 'a2', ok: false })
    expect(t.actions.chatApprove).not.toHaveBeenCalled()

    say({
      kind: 'chat.answer',
      req: 'a3',
      paneId: 'p1',
      ask: 'q1',
      answers: { 'Which database?': 'SQLite' }
    })
    await tick()
    expect(heard().at(-1)).toEqual({ kind: 'ack', req: 'a3', ok: true })
    expect(t.actions.chatApprove).toHaveBeenCalledWith('p1', 'q1', 'allow', {
      'Which database?': 'SQLite'
    })
    say({ kind: 'chat.answer', req: 'a4', paneId: 'p1', ask: 'q1', skip: true })
    await tick()
    expect(t.actions.chatApprove).toHaveBeenLastCalledWith('p1', 'q1', 'deny')
  })

  it('switches model, effort and a mode that still asks; never Full access', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    t.setThread({
      ...EMPTY_THREAD,
      models: [
        { value: 'default', label: 'Default', description: '', efforts: ['low', 'high'] },
        { value: 'haiku', label: 'Haiku', description: '', efforts: [] }
      ]
    })
    say({ kind: 'chat.open', paneId: 'p1' })
    await tick()
    expect((heard().at(-1) as Extract<ToWeb, { kind: 'chat.snapshot' }>).settings).toMatchObject({
      model: null,
      mode: 'default',
      modes: ['default', 'acceptEdits', 'plan', 'dontAsk']
    })
    const ack = async (m: Record<string, unknown>) => {
      say({ kind: 'chat.set', req: 'r', paneId: 'p1', ...m })
      await tick()
      return heard()
        .filter((x) => x.kind === 'ack')
        .at(-1)
    }
    expect(await ack({ mode: 'bypassPermissions' })).toMatchObject({ ok: false })
    expect(await ack({ mode: 'auto' })).toMatchObject({ ok: false })
    expect(t.actions.chatSetMode).not.toHaveBeenCalled()
    expect(await ack({ mode: 'plan' })).toMatchObject({ ok: true })
    expect(t.actions.chatSetMode).toHaveBeenCalledWith('p1', 'plan')
    expect(await ack({ model: 'gpt-9' })).toMatchObject({ ok: false })
    expect(await ack({ model: 'haiku' })).toMatchObject({ ok: true })
    expect(t.actions.chatSetModel).toHaveBeenCalledWith('p1', 'haiku')
    expect(await ack({ effort: 'max' })).toMatchObject({ ok: false })
    expect(await ack({ effort: 'high' })).toMatchObject({ ok: true })
    expect(t.actions.chatSetEffort).toHaveBeenCalledWith('p1', 'high')
  })

  it('puts an image together from its frames and sends it with the message', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    const img = (part: number, data: string) => ({
      kind: 'chat.image',
      upload: 'u1',
      part,
      parts: 2,
      mediaType: 'image/png',
      data
    })
    say(img(0, 'aGVs'))
    say({ kind: 'chat.send', req: 's1', paneId: 'p1', text: '', images: ['u1'] })
    await tick(20)
    expect(heard().at(-1)).toMatchObject({ kind: 'ack', req: 's1', ok: false })
    expect(t.actions.chatSend).not.toHaveBeenCalled()
    say(img(1, 'bG8='))
    say({ kind: 'chat.send', req: 's2', paneId: 'p1', text: 'look', images: ['u1'] })
    await tick(20)
    expect(heard().at(-1)).toEqual({ kind: 'ack', req: 's2', ok: true })
    expect(t.actions.chatSend).toHaveBeenCalledWith('p1', 'look', [
      { mediaType: 'image/png', data: 'aGVsbG8=' }
    ])
    // Used once: the same upload can't be sent again.
    say({ kind: 'chat.send', req: 's3', paneId: 'p1', text: 'again', images: ['u1'] })
    await tick(20)
    expect(heard().at(-1)).toMatchObject({ kind: 'ack', req: 's3', ok: false })
  })

  it('sends a sent message’s thumbnails to a browser that has its chat open', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    const thumb = `data:image/webp;base64,${'Q'.repeat(100)}`
    t.setThread({
      ...EMPTY_THREAD,
      items: [{ kind: 'user', id: 'u1', text: 'look', images: 1, thumbs: [thumb] }]
    })
    // heard() gives only what came since it was last called.
    const got: ToWeb[] = []
    const thumbs = (): ToWeb[] => (got.push(...heard()), got.filter((m) => m.kind === 'chat.thumb'))
    // Not open yet: nothing.
    say({ kind: 'chat.thumbs', paneId: 'p1', item: 'u1' })
    await tick(20)
    expect(thumbs()).toHaveLength(0)
    say({ kind: 'chat.open', paneId: 'p1' })
    await tick(20)
    thumbs()
    const snap = got.find((m) => m.kind === 'chat.snapshot')
    expect(snap?.kind === 'chat.snapshot' && snap.items[0]).toMatchObject({ images: 1, thumbs: 1 })
    say({ kind: 'chat.thumbs', paneId: 'p1', item: 'u1' })
    await tick(20)
    expect(thumbs()).toEqual([
      { kind: 'chat.thumb', paneId: 'p1', item: 'u1', index: 0, part: 0, parts: 1, data: thumb }
    ])
    // Asked again right away: not sent twice. An unknown item: nothing.
    say({ kind: 'chat.thumbs', paneId: 'p1', item: 'u1' })
    say({ kind: 'chat.thumbs', paneId: 'p1', item: 'nope' })
    await tick(20)
    expect(thumbs()).toHaveLength(1)
  })

  it('sends the work left running in the background, and again only when it changes', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    const task = {
      id: 'b1',
      kind: 'shell' as const,
      label: 'gh run watch',
      since: Date.now() - 5000
    }
    t.setThread({ ...EMPTY_THREAD, background: [task] })
    say({ kind: 'chat.open', paneId: 'p1' })
    await tick(20)
    const snap = heard().find((m) => m.kind === 'chat.snapshot')
    expect(snap?.kind === 'chat.snapshot' && snap.background).toEqual([
      { id: 'b1', kind: 'shell', label: 'gh run watch', ms: expect.any(Number) }
    ])
    const ms = snap?.kind === 'chat.snapshot' ? (snap.background?.[0]?.ms ?? 0) : 0
    expect(ms).toBeGreaterThanOrEqual(5000)
    // Same work, only older: nothing new. Ended: an update with an empty list.
    t.setThread({ ...EMPTY_THREAD, background: [{ ...task }] })
    await tick(80)
    expect(heard().some((m) => m.kind === 'chat.update' && m.background)).toBe(false)
    t.setThread({ ...EMPTY_THREAD, background: [] })
    await tick(80)
    expect(heard().find((m) => m.kind === 'chat.update' && m.background)).toMatchObject({
      kind: 'chat.update',
      paneId: 'p1',
      background: []
    })
  })

  it('drops a frame the server replays', async () => {
    const t = setup()
    const { heard, raw } = await paired(t)
    const payload = raw({ kind: 'sync' })
    t.frame({ t: 'data', pairing: PAIR, seq: 2, payload })
    t.frame({ t: 'data', pairing: PAIR, seq: 3, payload })
    await tick()
    expect(heard().filter((m) => m.kind === 'state')).toHaveLength(1)
  })

  it('starts a new chat in a project without leaving the shown project', async () => {
    const t = setup()
    const { say, heard } = await paired(t)
    say({ kind: 'chat.new', req: 'n1', wsId: 'ws1', text: 'Explain the repo' })
    await tick(20)
    expect(t.actions.createPane).toHaveBeenCalledWith({
      accountId: 'a1',
      cwd: '~/code/api',
      args: [],
      shell: false,
      slotId: null
    })
    expect(t.actions.switchWorkspace).not.toHaveBeenCalled()
    expect(t.actions.chatSend).toHaveBeenCalledWith('p2', 'Explain the repo', [])
    expect(heard().at(-1)).toEqual({ kind: 'ack', req: 'n1', ok: true, paneId: 'p2' })
  })

  it('learns a claim it missed while offline from the server’s list, then trusts the code', async () => {
    const t = setup()
    await t.host.start()
    const r = await t.host.pair()
    if (!r.ok) throw new Error(r.error)
    const link = parsePairingLink(r.link)!
    await tick()
    t.sock.onopen?.({} as Event)
    const phone = newKeyPair()
    t.listed.splice(0, 1, {
      id: PAIR,
      status: 'active',
      phoneName: 'Phone',
      phoneKey: phone.publicKey
    })
    // No `pairing.claimed` frame: it happened before we connected.
    t.frame({ t: 'welcome', v: 1, connectionId: 'c', role: 'desktop' })
    await tick()
    expect(t.host.getView().pairings).toMatchObject([{ id: PAIR, confirmed: false }])
    const ch = channel('web', phone.secretKey, link.desktopKey, PAIR)
    t.frame({
      t: 'data',
      pairing: PAIR,
      seq: 1,
      payload: encode(ch, new Counter(), {
        kind: 'pair.confirm',
        code: link.code,
        phoneKey: phone.publicKey,
        phoneName: 'Phone'
      })
    })
    await tick()
    expect(t.host.getView().pairings).toMatchObject([{ id: PAIR, confirmed: true }])
    // Gone from the server (revoked elsewhere): forgotten here on the next sync.
    t.listed.length = 0
    t.frame({ t: 'welcome', v: 1, connectionId: 'c2', role: 'desktop' })
    await tick()
    expect(t.host.getView().pairings).toEqual([])
  })

  it('one-click pairing: a browser asks, the codes match, Allow makes it trusted', async () => {
    const t = setup()
    await t.host.start()
    await tick()
    t.sock.onopen?.({} as Event)
    t.frame({ t: 'welcome', v: 1, connectionId: 'c', role: 'desktop' })
    const web = newKeyPair()
    const REQ = 'pair_req1'
    t.listed.push({ id: REQ, status: 'pending', phoneName: 'Chrome', phoneKey: web.publicKey })
    t.frame({
      t: 'pairing.request',
      pairing: REQ,
      phoneName: 'Chrome on Linux',
      phoneKey: web.publicKey,
      deviceName: 'Work laptop'
    })
    await tick()
    // The desktop posted its key; the dialog shows the code the browser computes too.
    expect(t.requestKey).toHaveBeenCalledTimes(1)
    const [, desktopKey] = t.requestKey.mock.calls[0]!
    const view = t.host.getView()
    expect(view.request).toMatchObject({
      id: REQ,
      name: 'Chrome on Linux',
      code: pairingCode(REQ, desktopKey, web.publicKey)
    })
    // Not listed as paired until allowed.
    expect(view.pairings).toEqual([])

    await t.host.answerRequest(true)
    expect(t.requestAnswer).toHaveBeenCalledWith(REQ, true)
    expect(t.host.getView()).toMatchObject({
      request: null,
      pairings: [{ id: REQ, name: 'Chrome on Linux', confirmed: true }]
    })
    // Trusted straight away: no pair.confirm needed.
    const ch = channel('web', web.secretKey, desktopKey, REQ)
    t.frame({
      t: 'data',
      pairing: REQ,
      seq: 1,
      payload: encode(ch, new Counter(), { kind: 'sync' })
    })
    await tick()
    const inc = new Counter()
    const got = t.sent
      .filter((f) => f.t === 'data')
      .map((f) => decode(ch, inc, String(f.payload), ToWeb))
    expect(got.find((m) => m?.kind === 'state')).toMatchObject({ device: 'Work laptop' })
  })

  it('one-click pairing: Deny forgets it, and nothing it sends is answered', async () => {
    const t = setup()
    await t.host.start()
    await tick()
    t.sock.onopen?.({} as Event)
    const web = newKeyPair()
    t.frame({
      t: 'pairing.request',
      pairing: 'pair_req2',
      phoneName: 'Chrome',
      phoneKey: web.publicKey,
      deviceName: 'Work laptop'
    })
    await tick()
    await t.host.answerRequest(false)
    expect(t.requestAnswer).toHaveBeenCalledWith('pair_req2', false)
    expect(t.host.getView().request).toBeNull()
    expect((t.saved() as { pairings: object }).pairings).toEqual({})
  })
})
