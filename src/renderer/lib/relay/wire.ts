// The desktop's projects and chats as the relay sends them to a paired web app
// (src/shared/relay/messages.ts): ChatItem → WireItem, the project list, and sizing so every
// message fits one sealed frame. Pure, so it is unit-tested.
import type { Config, Pane } from '@shared/types'
import { CHAT_MODES, type ChatMode, type ToolPreview } from '@shared/chat'
import { AGENT_CLI_SPECS } from '@shared/agent-clis'
import { DEFAULT_PROJECT_ICON, PROJECT_ICON_ID } from '@shared/project-icons'
import { PLAINTEXT_MAX } from '@shared/relay/crypto'
import {
  IMAGE_CHUNK,
  PREVIEW_MAX,
  RELAY_FEATURES,
  THUMB_PARTS_MAX,
  THUMB_URL,
  type WireChat,
  type WireCommand,
  type WireItem,
  type WirePreview,
  type WireProject,
  type WireSettings,
  type ToWeb
} from '@shared/relay/messages'
import type { ChatItem, ChatThread } from '../chat-thread'
import { pendingApproval } from '../chat-thread'
import { buildSlashItems } from '../slash'
import { projectFolder } from '../team'

/** Item text past this is cut (a frame is 64 KiB; 4 bytes per character at worst). */
export const ITEM_TEXT_CAP = 12_000
/** Room left in a frame for the envelope (`{"v":1,"n":…,"m":…}`). */
const FRAME_BUDGET = PLAINTEXT_MAX - 256
/** Items a snapshot starts from (newest); older ones are left for the desktop. */
export const SNAPSHOT_ITEMS = 120
/** At most this much of a frame goes to the CLI's commands; the rest is left out. */
export const COMMANDS_BUDGET = 16_000
/** A command's description past this is cut. */
const COMMAND_TEXT_CAP = 160

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

/** A tool's details, each text cut to PREVIEW_MAX. */
export function wirePreview(p: ToolPreview): WirePreview {
  const cut = (t: string): string => (t.length > PREVIEW_MAX ? t.slice(0, PREVIEW_MAX) : t)
  switch (p.type) {
    case 'code':
      return { type: 'code', text: cut(p.text) }
    case 'diff':
      return {
        type: 'diff',
        path: p.path.slice(0, 1000),
        before: cut(p.before),
        after: cut(p.after)
      }
    case 'todos':
      return {
        type: 'todos',
        items: p.items.slice(0, 100).map((t) => ({ text: t.text.slice(0, 500), status: t.status }))
      }
  }
}

/** An item over this many bytes drops its details, so it still fits one frame. */
const ITEM_BUDGET = FRAME_BUDGET - 2048

/** A sent message's thumbnails the web can be sent: data URLs of an image kind. */
export const wireThumbs = (thumbs: string[] | undefined): string[] =>
  (thumbs ?? []).filter((t) => THUMB_URL.test(t)).slice(0, 20)

/**
 * A user message's thumbnails as `chat.thumb` frames, each data URL cut in IMAGE_CHUNK pieces;
 * one that isn't a data URL of an image kind, or needs over THUMB_PARTS_MAX frames, is left out
 * (the others keep their index).
 */
export function thumbFrames(
  paneId: string,
  item: string,
  thumbs: string[]
): Extract<ToWeb, { kind: 'chat.thumb' }>[] {
  const frames: Extract<ToWeb, { kind: 'chat.thumb' }>[] = []
  thumbs.slice(0, 20).forEach((url, index) => {
    if (!THUMB_URL.test(url)) return
    const parts = Math.ceil(url.length / IMAGE_CHUNK)
    if (parts > THUMB_PARTS_MAX) return
    for (let part = 0; part < parts; part++)
      frames.push({
        kind: 'chat.thumb',
        paneId,
        item,
        index,
        part,
        parts,
        data: url.slice(part * IMAGE_CHUNK, (part + 1) * IMAGE_CHUNK)
      })
  })
  return frames
}

export function wireItem(it: ChatItem): WireItem {
  switch (it.kind) {
    case 'user': {
      const thumbs = wireThumbs(it.thumbs).length
      return {
        id: it.id,
        kind: 'user',
        ...cap(it.text, false),
        ...(it.images ? { images: Math.min(it.images, 20) } : {}),
        ...(thumbs ? { thumbs } : {})
      }
    }
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
      const a = it.approval
      const w: WireItem = {
        id: it.id,
        kind: 'tool',
        ...cap(out, true),
        tool: { name: it.name.slice(0, 200), subtitle: it.subtitle.slice(0, 500), state },
        ...(it.preview ? { preview: wirePreview(it.preview) } : {}),
        // Only claude's questions are answered from the web; other approvals stay here.
        ...(a && a.decided === null && a.questions?.length
          ? {
              ask: {
                id: a.reqId,
                questions: a.questions.slice(0, 8).map((q) => ({
                  question: q.question.slice(0, 1000),
                  header: q.header.slice(0, 100),
                  multiSelect: q.multiSelect,
                  options: q.options.slice(0, 12).map((o) => ({
                    label: o.label.slice(0, 300),
                    description: o.description.slice(0, 1000)
                  }))
                }))
              }
            }
          : {})
      }
      if (w.preview && bytes(w) > ITEM_BUDGET) delete w.preview
      return w
    }
  }
}

const commandMemo = new WeakMap<ChatThread['commands'], { term: string; out: WireCommand[] }>()

/**
 * The CLI's commands for the web's `/` menu: what the desktop's own menu lists from the CLI
 * (buildSlashItems, without the terminal-only ones and Wraithgrid's own), cut to fit
 * COMMANDS_BUDGET. Memoised per command list, as flushes run on every streamed token.
 */
export function wireCommands(thread: Pick<ChatThread, 'commands' | 'terminalOnly'>): WireCommand[] {
  const term = thread.terminalOnly.join(' ')
  const hit = commandMemo.get(thread.commands)
  if (hit && hit.term === term) return hit.out
  const out: WireCommand[] = []
  let size = 2
  for (const c of buildSlashItems(thread.commands, thread.terminalOnly)) {
    if (c.group === 'app' || c.sigil === '@' || out.length >= 200) continue
    const w: WireCommand = {
      name: c.name.slice(0, 100),
      description:
        c.description.length > COMMAND_TEXT_CAP
          ? `${c.description.slice(0, COMMAND_TEXT_CAP - 1)}…`
          : c.description,
      hint: c.argumentHint.slice(0, 100),
      sigil: c.sigil === '$' ? '$' : '/',
      builtin: c.group === 'builtin'
    }
    const s = bytes(w) + 1
    if (size + s > COMMANDS_BUDGET) break
    size += s
    out.push(w)
  }
  commandMemo.set(thread.commands, { term, out })
  return out
}

/**
 * A chat's snapshot: its newest items that fit one frame, oldest first. `more` when older
 * ones were left out. The CLI's commands come along once it has said them.
 */
export function snapshot(
  paneId: string,
  thread: ChatThread,
  settings?: WireSettings | null
): Extract<ToWeb, { kind: 'chat.snapshot' }> {
  const all = thread.items
  const items: WireItem[] = []
  const commands = wireCommands(thread)
  const extra = {
    ...(commands.length ? { commands } : {}),
    ...(settings ? { settings } : {})
  }
  let size = bytes({
    kind: 'chat.snapshot',
    paneId,
    items: [],
    busy: thread.busy,
    more: true,
    ...extra
  })
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
    more: items.length < all.length,
    ...extra
  }
}

/** The modes the web may pick: never Full access or Auto (tool approvals stay on the computer). */
export const WEB_MODES: readonly ChatMode[] = ['default', 'acceptEdits', 'plan', 'dontAsk']

/** A chat's model, effort and mode, and what the web may switch them to. */
export function wireSettings(
  pane: Pick<Pane, 'model' | 'effort' | 'chatMode'>,
  thread: Pick<ChatThread, 'models' | 'controls'>
): WireSettings {
  const has = thread.controls?.modes ?? CHAT_MODES
  return {
    model: pane.model?.slice(0, 100) ?? null,
    models: thread.models.slice(0, 40).map((m) => ({
      value: m.value.slice(0, 100),
      label: m.label.slice(0, 100),
      description: m.description.slice(0, 300),
      efforts: m.efforts.slice(0, 10).map((e) => e.slice(0, 20))
    })),
    effort: pane.effort ?? null,
    mode: pane.chatMode ?? 'default',
    modes: WEB_MODES.filter((m) => has.includes(m))
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
    ...(PROJECT_ICON_ID.test(ws.icon) && ws.icon !== DEFAULT_PROJECT_ICON ? { icon: ws.icon } : {}),
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
    more: false,
    features: [...RELAY_FEATURES]
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
