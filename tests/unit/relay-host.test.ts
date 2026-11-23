// The desktop end of the relay (src/renderer/lib/relay/host.ts) against a fake server socket,
// a fake store and chat bus, with the real crypto: a browser pairs, proves the pairing link's code,
// then reads and drives chats; anything unproven or replayed is ignored.
import { describe, expect, it, vi } from 'vitest'
import { defaultConfig } from '../../src/shared/schema'
import type { Config, Pane, Workspace } from '../../src/shared/types'
import { channel, newKeyPair } from '../../src/shared/relay/crypto'
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
    accounts: [{ id: 'a1', name: 'work', cli: 'claude' } as Config['accounts'][number]],
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
  return { host, sent, sock, frame, actions, setThread, saved: () => saved, config, listed }
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
})
