// The desktop's projects and chats as the relay sends them to a paired web app
// (src/shared/relay/messages.ts): ChatItem → WireItem, the project list, and sizing so every
// message fits one sealed frame. Pure, so it is unit-tested.
import type { Config } from '@shared/types'
import { AGENT_CLI_SPECS } from '@shared/agent-clis'
import { PLAINTEXT_MAX } from '@shared/relay/crypto'
import type { WireChat, WireItem, WireProject, ToWeb } from '@shared/relay/messages'
import type { ChatItem, ChatThread } from '../chat-thread'
import { pendingApproval } from '../chat-thread'
import { projectFolder } from '../team'

/** Item text past this is cut (a frame is 64 KiB; 4 bytes per character at worst). */
export const ITEM_TEXT_CAP = 12_000
/** Room left in a frame for the envelope (`{"v":1,"n":…,"m":…}`). */
const FRAME_BUDGET = PLAINTEXT_MAX - 256
/** Items a snapshot starts from (newest); older ones are left for the desktop. */
export const SNAPSHOT_ITEMS = 120

const bytes = (v: unknown): number => new TextEncoder().encode(JSON.stringify(v)).length

/** Cuts long text: messages keep their start, tool output its end (where errors are). */
function cap(text: string, keepEnd: boolean, max = ITEM_TEXT_CAP): { text: string; cut?: true } {
  if (text.length <= max) return { text }
  return { text: keepEnd ? text.slice(-max) : text.slice(0, max), cut: true }
}

function secs(ms: number | null): string {
  if (ms === null) return ''
  return ms < 60_000 ? `${Math.round(ms / 1000)}s` : `${Math.round(ms / 60_000)}m`
}

export function wireItem(it: ChatItem): WireItem {
  switch (it.kind) {
    case 'user':
      return { id: it.id, kind: 'user', ...cap(it.text, false) }
    case 'assistant':
      return { id: it.id, kind: 'assistant', ...cap(it.text, false) }
    case 'thinking':
      return { id: it.id, kind: 'thinking', ...cap(it.text, false) }
    case 'handoff':
      return { id: it.id, kind: 'notice', level: 'divider', ...cap(it.divider, false, 500) }
    case 'notice':
      return { id: it.id, kind: 'notice', level: it.level, ...cap(it.text, false) }
    case 'turn': {
      const took = secs(it.durationMs)
      const text = it.error ?? (took ? `Done in ${took}` : 'Done')
      return { id: it.id, kind: 'turn', ...cap(text, false, 500) }
    }
    case 'tool': {
      const state =
        it.approval && it.approval.decided === null
          ? 'approval'
          : it.output === null
            ? 'running'
            : it.isError
              ? 'error'
              : 'done'
      const out = it.result?.text ?? it.output ?? ''
      return {
        id: it.id,
        kind: 'tool',
        ...cap(out, true),
        tool: { name: it.name.slice(0, 200), subtitle: it.subtitle.slice(0, 500), state }
      }
    }
  }
}

/**
 * A chat's snapshot: its newest items that fit one frame, oldest first. `more` when older
 * ones were left out.
 */
export function snapshot(
  paneId: string,
  thread: ChatThread
): Extract<ToWeb, { kind: 'chat.snapshot' }> {
  const all = thread.items
  const items: WireItem[] = []
  let size = bytes({ kind: 'chat.snapshot', paneId, items: [], busy: thread.busy, more: true })
  for (let i = all.length - 1; i >= 0 && items.length < SNAPSHOT_ITEMS; i--) {
    const w = wireItem(all[i]!)
    const s = bytes(w) + 1
    if (size + s > FRAME_BUDGET) break
    size += s
    items.unshift(w)
  }
  return {
    kind: 'chat.snapshot',
    paneId,
    items,
    busy: thread.busy,
    more: items.length < all.length
  }
}

/**
 * What changed since `sent` (item id → its last sent JSON), in frames that each fit; `sent`
 * is updated. Only the newest items are compared: older ones don't change.
 */
export function updates(
  paneId: string,
  thread: ChatThread,
  sent: Map<string, string>,
  lastBusy: boolean | null
): Extract<ToWeb, { kind: 'chat.update' }>[] {
  const changed: WireItem[] = []
  for (const it of thread.items.slice(-SNAPSHOT_ITEMS)) {
    const w = wireItem(it)
    const json = JSON.stringify(w)
    if (sent.get(w.id) === json) continue
    sent.set(w.id, json)
    changed.push(w)
  }
  if (!changed.length && lastBusy === thread.busy) return []
  const frames: Extract<ToWeb, { kind: 'chat.update' }>[] = []
  let cur: WireItem[] = []
  let size = bytes({ kind: 'chat.update', paneId, items: [], busy: thread.busy })
  const base = size
  for (const w of changed) {
    const s = bytes(w) + 1
    if (cur.length && size + s > FRAME_BUDGET) {
      frames.push({ kind: 'chat.update', paneId, items: cur, busy: thread.busy })
      cur = []
      size = base
    }
    cur.push(w)
    size += s
  }
  frames.push({ kind: 'chat.update', paneId, items: cur, busy: thread.busy })
  return frames
}

/** Remembers a snapshot's items as sent, so the next `updates` only sends what changed. */
export function markSent(sent: Map<string, string>, items: WireItem[]): void {
  sent.clear()
  for (const w of items) sent.set(w.id, JSON.stringify(w))
}

export interface StateInput {
  config: Config
  /** Is this pane a chat (store's `paneView`)? */
  isChat: (paneId: string) => boolean
  thread: (paneId: string) => ChatThread
  activeAt: (paneId: string) => number | null
}

/** The account a new chat in this project uses: its own, else the app's default, if it chats. */
export function newChatAccount(config: Config, wsId: string): string | null {
  const ws = config.workspaces.find((w) => w.id === wsId)
  if (!ws) return null
  const chats = (id: string | null): boolean => {
    const a = config.accounts.find((x) => x.id === id)
    return !!a && AGENT_CLI_SPECS[a.cli].chat
  }
  if (chats(ws.accountId)) return ws.accountId
  if (chats(config.settings.defaultAccountId)) return config.settings.defaultAccountId
  return null
}

/** The `state` message: projects and their chats, newest chats dropped first to fit one frame. */
export function state(device: string, s: StateInput): Extract<ToWeb, { kind: 'state' }> {
  const accounts = new Map(s.config.accounts.map((a) => [a.id, a]))
  const projects: WireProject[] = s.config.workspaces.slice(0, 100).map((ws) => ({
    id: ws.id,
    name: ws.name.slice(0, 100),
    folder: projectFolder(ws)?.slice(0, 4096) ?? null,
    canChat: !ws.machineId && !!projectFolder(ws) && !!newChatAccount(s.config, ws.id),
    chats: ws.panes
      .filter((p) => s.isChat(p.id))
      .slice(0, 200)
      .map((p): WireChat => {
        const t = s.thread(p.id)
        const a = accounts.get(p.accountId ?? '')
        return {
          paneId: p.id,
          title: p.title.slice(0, 200),
          cli: a?.cli ?? '',
          account: (a?.name ?? '').slice(0, 100),
          busy: t.busy,
          waiting: pendingApproval(t) !== null,
          activeAt: s.activeAt(p.id)
        }
      })
  }))
  const msg: Extract<ToWeb, { kind: 'state' }> = {
    kind: 'state',
    device: device.slice(0, 100),
    projects,
    more: false
  }
  // Too big: drop the least recently active chats until it fits.
  while (bytes(msg) > FRAME_BUDGET) {
    let worst: { p: WireProject; i: number; at: number } | null = null
    for (const p of msg.projects)
      p.chats.forEach((c, i) => {
        const at = c.activeAt ?? 0
        if (!worst || at < worst.at) worst = { p, i, at }
      })
    if (!worst) break
    const w: { p: WireProject; i: number } = worst
    w.p.chats.splice(w.i, 1)
    msg.more = true
  }
  return msg
}
