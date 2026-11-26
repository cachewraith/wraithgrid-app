// The relay end to end against a real wraithgrid-server: this desktop's RelayHost (fake store
// and chat, real crypto and WebSocket) and a scripted phone/web client signed in to the same
// account. Skipped unless WRAITHGRID_RELAY_SERVER, WG_TEST_EMAIL and WG_TEST_PASSWORD are set:
//   WRAITHGRID_RELAY_SERVER=http://localhost:3000 WG_TEST_EMAIL=… WG_TEST_PASSWORD=… \
//     ./node_modules/.bin/vitest run tests/unit/relay-live.test.ts
// (a verified user: `bun run admin:create <email>` in wraithgrid-server makes one).
import { describe, expect, it, vi } from 'vitest'
import { defaultConfig } from '../../src/shared/schema'
import type { Config, Pane, Workspace } from '../../src/shared/types'
import { channel, newKeyPair, pairingCode } from '../../src/shared/relay/crypto'
import { Counter, decode, encode, parsePairingLink, ToWeb } from '../../src/shared/relay/messages'
import { RelayHost, type RelayApi } from '../../src/renderer/lib/relay/host'
import { EMPTY_THREAD, type ChatThread } from '../../src/renderer/lib/chat-thread'

const SERVER = process.env.WRAITHGRID_RELAY_SERVER ?? ''
const EMAIL = process.env.WG_TEST_EMAIL ?? ''
const PASSWORD = process.env.WG_TEST_PASSWORD ?? ''

async function call(method: string, path: string, token: string | null, body?: unknown) {
  const res = await fetch(SERVER + path, {
    method,
    headers: {
      'content-type': 'application/json',
      // Better Auth wants a browser's Origin: the web app's (in the server's APP_ORIGINS).
      ...(path.startsWith('/api/auth/') ? { origin: 'http://localhost:5175' } : {}),
      ...(token ? { authorization: `Bearer ${token}` } : {})
    },
    body: body === undefined ? undefined : JSON.stringify(body)
  })
  const json = await res.json().catch(() => null)
  if (!res.ok) throw new Error(`${method} ${path}: ${res.status} ${JSON.stringify(json)}`)
  return json
}

const wsUrl = (ticket: string): string =>
  `${SERVER.replace(/^http/, 'ws')}/v1/relay/connect?ticket=${encodeURIComponent(ticket)}`

const until = async (ok: () => boolean, what = '', ms = 8000): Promise<void> => {
  const end = Date.now() + ms
  while (!ok()) {
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`)
    await new Promise((r) => setTimeout(r, 50))
  }
}

describe.skipIf(!SERVER || !EMAIL || !PASSWORD)('relay against a real server', () => {
  /** A desktop (RelayHost over a real server session, fake store/chat) and a browser session. */
  async function setUp() {
    // The desktop's device session (what cloud-session.json holds) and the browser's session.
    const desk = await call('POST', '/v1/desktop/email/sign-in', null, {
      email: EMAIL,
      password: PASSWORD,
      device: { name: 'Relay smoke desktop', platform: 'linux', appVersion: '9.9.9' }
    })
    const deskToken: string = desk.session.token
    const web = await call('POST', '/api/auth/sign-in/email', null, {
      email: EMAIL,
      password: PASSWORD
    })
    const webToken: string = web.token

    // The desktop: RelayHost over main's calls, done here with fetch like desktop-rs does.
    const pane = {
      id: 'p1',
      accountId: 'a1',
      cwd: '~/x',
      title: 'Smoke chat',
      view: 'chat',
      shell: false
    } as unknown as Pane
    const ws = {
      id: 'ws1',
      name: 'smoke',
      folder: '~/x',
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
    const runtime: Record<string, unknown> = {}
    const chatSend = vi.fn()
    const store = {
      getState: () => ({
        config,
        runtime,
        requestStart: (id: string) => (runtime[id] = { started: true, exit: null, activeAt: null }),
        chatSend,
        chatInterrupt: vi.fn(),
        switchWorkspace: vi.fn(),
        createPane: vi.fn()
      })
    }
    let thread: ChatThread = {
      ...EMPTY_THREAD,
      items: [{ kind: 'user', id: 'u1', text: 'hi', images: 0 }]
    }
    const subs = new Set<() => void>()
    const chat = {
      thread: () => thread,
      load: async () => undefined,
      subscribe: (_: string, cb: () => void) => (subs.add(cb), () => subs.delete(cb))
    }
    let saved: unknown = null
    const api: RelayApi = {
      relayPair: async () => ({
        ok: true,
        ...(await call('POST', '/v1/relay/pairings', deskToken, {})),
        server: SERVER,
        appUrl: 'http://localhost:5175'
      }),
      relayPairings: async () => ({
        ok: true,
        ...(await call('GET', '/v1/relay/pairings', deskToken))
      }),
      relayUnpair: async (id) => (
        await call('DELETE', `/v1/relay/pairings/${id}`, deskToken),
        { ok: true }
      ),
      relayConnect: async () => ({ ok: true }),
      relaySend: async () => ({ ok: true }),
      relayClose: async () => ({ ok: true }),
      onRelay: () => () => undefined,
      relayLoad: async () => saved,
      relaySave: async (d) => ((saved = JSON.parse(JSON.stringify(d))), { ok: true }),
      relayRequestKey: async (id, desktopKey) => (
        await call('POST', `/v1/relay/requests/${id}/key`, deskToken, { desktopKey }),
        { ok: true }
      ),
      relayRequestAnswer: async (id, allow) => (
        await call('POST', `/v1/relay/requests/${id}/${allow ? 'approve' : 'deny'}`, deskToken, {}),
        { ok: true }
      ),
      status: async () => ({
        server: SERVER,
        signedIn: true,
        user: null,
        deviceId: desk.device.id,
        pending: false
      }),
      onChanged: () => () => undefined
    }
    const host = new RelayHost({
      api,
      // A real WebSocket here (main's socket in the app).
      openSocket: async () =>
        new WebSocket(wsUrl((await call('POST', '/v1/relay/ticket', deskToken, {})).ticket)),
      store: store as never,
      chat: chat as never,
      timers: { state: 500, flush: 50, startWait: 2000 }
    })
    const setThread = (t: ChatThread): void => {
      thread = t
      for (const cb of subs) cb()
    }
    return { desk, deskToken, webToken, host, chatSend, thread: () => thread, setThread }
  }

  it('pairs a browser, which then reads and drives a chat on this desktop', async () => {
    const { desk, deskToken, webToken, host, chatSend, thread, setThread } = await setUp()
    await host.start()
    const pair = await host.pair()
    if (!pair.ok) throw new Error(pair.error)
    const link = parsePairingLink(pair.link)!
    await until(() => host.getView().pending !== null, 'the pairing link')

    // The browser: claim with its own key, connect, prove the code, then talk.
    const phone = newKeyPair()
    await call('POST', `/v1/relay/pairings/${link.pairingId}/claim`, webToken, {
      code: link.code,
      phoneName: 'Smoke browser',
      phoneKey: phone.publicKey
    })
    const ch = channel('web', phone.secretKey, link.desktopKey, link.pairingId)
    const out = new Counter()
    const inc = new Counter()
    const heard: ToWeb[] = []
    const ticket = (await call('POST', '/v1/relay/ticket', webToken, {})).ticket
    const sock = new WebSocket(wsUrl(ticket))
    let seq = 0
    let joined = false
    sock.onmessage = (e) => {
      const f = JSON.parse(String(e.data))
      if (f.t === 'welcome') sock.send(JSON.stringify({ t: 'join', pairings: [link.pairingId] }))
      if (f.t === 'joined') joined = true
      if (f.t === 'data') {
        const m = decode(ch, inc, f.payload, ToWeb)
        if (m) heard.push(m)
      }
    }
    sock.onopen = () => sock.send(JSON.stringify({ t: 'hello', v: [1] }))
    const say = (m: unknown): void =>
      sock.send(
        JSON.stringify({
          t: 'data',
          pairing: link.pairingId,
          seq: ++seq,
          payload: encode(ch, out, m as never)
        })
      )
    try {
      await until(() => joined && host.getView().pairings.some((p) => p.online), 'both ends online')
      say({
        kind: 'pair.confirm',
        code: link.code,
        phoneKey: phone.publicKey,
        phoneName: 'Smoke browser'
      })
      await until(() => host.getView().pairings[0]?.confirmed === true, 'confirm')

      say({ kind: 'sync' })
      await until(() => heard.some((m) => m.kind === 'state'), 'state')
      expect(heard.find((m) => m.kind === 'state')).toMatchObject({
        device: 'Relay smoke desktop',
        projects: [{ name: 'smoke', chats: [{ paneId: 'p1', title: 'Smoke chat' }] }]
      })

      say({ kind: 'chat.open', paneId: 'p1' })
      await until(() => heard.some((m) => m.kind === 'chat.snapshot'), 'snapshot')
      setThread({
        ...thread(),
        busy: true,
        items: [...thread().items, { kind: 'assistant', id: 'a1', text: 'streamed' }]
      })
      await until(
        () =>
          heard.some((m) => m.kind === 'chat.update' && m.items.some((i) => i.text === 'streamed')),
        'update'
      )

      say({ kind: 'chat.send', req: 'r1', paneId: 'p1', text: 'from the web' })
      await until(() => heard.some((m) => m.kind === 'ack' && m.req === 'r1'), 'ack')
      expect(chatSend).toHaveBeenCalledWith('p1', 'from the web', [])
    } finally {
      sock.close()
      host.stop()
      await call('DELETE', `/v1/devices/${desk.device.id}`, deskToken).catch(() => undefined)
    }
  }, 30_000)

  it('one-click: the browser asks, both compute the same code, Allow pairs them', async () => {
    const { desk, deskToken, webToken, host } = await setUp()
    // Nothing paired yet: with web access on, the desktop connects anyway.
    await host.start()
    try {
      await until(() => host.getView().connected, 'the desktop on the relay')
      const list = await call('GET', '/v1/relay/desktops', webToken)
      expect(list.desktops).toContainEqual(
        expect.objectContaining({ id: desk.device.id, name: 'Relay smoke desktop', online: true })
      )
      const web = newKeyPair()
      const asked = await call('POST', '/v1/relay/requests', webToken, {
        deviceId: desk.device.id,
        phoneName: 'Live browser',
        phoneKey: web.publicKey
      })
      await until(() => host.getView().request !== null, 'the Allow dialog')
      let status = await call('GET', `/v1/relay/requests/${asked.pairing}`, webToken)
      const deadline = Date.now() + 5000
      while (!status.desktopKey && Date.now() < deadline) {
        await new Promise((r) => setTimeout(r, 100))
        status = await call('GET', `/v1/relay/requests/${asked.pairing}`, webToken)
      }
      // The browser's code and the desktop's dialog agree.
      expect(pairingCode(asked.pairing, status.desktopKey, web.publicKey)).toBe(
        host.getView().request?.code
      )
      await host.answerRequest(true)
      status = await call('GET', `/v1/relay/requests/${asked.pairing}`, webToken)
      expect(status.state).toBe('active')

      // Paired: the browser talks to it straight away, no pair.confirm.
      const ch = channel('web', web.secretKey, status.desktopKey, asked.pairing)
      const inc = new Counter()
      const heard: ToWeb[] = []
      const sock = new WebSocket(
        wsUrl((await call('POST', '/v1/relay/ticket', webToken, {})).ticket)
      )
      let joined = false
      sock.onopen = () => sock.send(JSON.stringify({ t: 'hello', v: [1] }))
      sock.onmessage = (e) => {
        const f = JSON.parse(String(e.data))
        if (f.t === 'welcome') sock.send(JSON.stringify({ t: 'join', pairings: [asked.pairing] }))
        if (f.t === 'joined') joined = f.pairings[0]?.online === true
        if (f.t === 'data') {
          const m = decode(ch, inc, f.payload, ToWeb)
          if (m) heard.push(m)
        }
      }
      await until(() => joined, 'the browser joined, desktop online')
      sock.send(
        JSON.stringify({
          t: 'data',
          pairing: asked.pairing,
          seq: 1,
          payload: encode(ch, new Counter(), { kind: 'sync' })
        })
      )
      await until(() => heard.some((m) => m.kind === 'state'), 'state')
      sock.close()
    } finally {
      host.stop()
      await call('DELETE', `/v1/devices/${desk.device.id}`, deskToken).catch(() => undefined)
    }
  }, 30_000)
})
