// Settings → General → Web access: lets the web app (app.wraithgrid.cachewraith.com) work on
// this computer's projects (their folders and chats) through the end-to-end encrypted relay
// (lib/relay/host.ts). Pairing gives a one-time link to open in the browser; paired browsers are
// listed with Remove.
import { useEffect, useState, useSyncExternalStore } from 'react'
import { useServices } from '../app/services'
import { Dialog } from './Dialog'
import { IconClose, IconCopy, IconGlobe } from './icons'
import type { RelayHost, RelayView } from '../lib/relay/host'

const NO_VIEW: RelayView = {
  enabled: false,
  signedIn: false,
  connected: false,
  error: null,
  pairings: [],
  pending: null
}
const noop = (): (() => void) => () => undefined

function useRelay(relay: RelayHost | undefined): RelayView {
  return useSyncExternalStore(relay?.subscribe ?? noop, relay?.getView ?? (() => NO_VIEW))
}

export function RelayAccessRow() {
  const { relay, api } = useServices()
  const view = useRelay(relay)
  const [pairing, setPairing] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  if (!relay || !view.signedIn) return null

  const pair = async (): Promise<void> => {
    setBusy('pair')
    setError(null)
    const r = await relay.pair()
    setBusy(null)
    if (r.ok) setPairing(true)
    else setError(r.error)
  }
  const remove = async (id: string): Promise<void> => {
    setBusy(id)
    const r = await relay.unpair(id)
    setBusy(null)
    if (!r.ok) setError(r.error)
  }

  return (
    <div className="srow">
      <div>
        <h3 id="relay-h">Web access</h3>
        <p className="ex">
          Work on this computer’s projects from app.wraithgrid.cachewraith.com in any browser: their
          folders and chats, while Wraithgrid runs here. End to end encrypted: our server passes
          messages on but can’t read them. Tool approvals stay on this computer.
        </p>
      </div>
      <div className="ctl relay" aria-labelledby="relay-h">
        <div className="swrow">
          <button
            type="button"
            className={`sw${view.enabled ? ' on' : ''}`}
            role="switch"
            aria-checked={view.enabled}
            aria-labelledby="relay-on"
            onClick={() => void relay.setEnabled(!view.enabled)}
          />
          <span id="relay-on">Allow paired browsers</span>
        </div>
        {view.pairings.length ? (
          <ul className="relay-list" aria-label="Paired browsers">
            {view.pairings.map((p) => (
              <li key={p.id}>
                <span
                  className={`relay-dot${p.online && view.connected ? ' on' : ''}`}
                  aria-hidden
                />
                <span className="nm">{p.name ?? 'Browser'}</span>
                <span className="hint">
                  {!p.confirmed ? 'Confirming…' : p.online && view.connected ? 'Online' : 'Offline'}
                </span>
                <button
                  type="button"
                  className="btn gh"
                  disabled={busy === p.id}
                  onClick={() => void remove(p.id)}
                >
                  Remove
                </button>
              </li>
            ))}
          </ul>
        ) : null}
        <div className="relay-act">
          <button
            type="button"
            className="btn"
            disabled={!view.enabled || busy === 'pair'}
            onClick={() => void pair()}
          >
            {busy === 'pair' ? 'Getting a code…' : 'Pair a browser'}
          </button>
          {view.enabled && view.pairings.length > 0 && !view.connected ? (
            <span className="hint">Connecting to the relay…</span>
          ) : null}
        </div>
        {error || view.error ? (
          <span className="hint err" role="alert">
            {error ?? view.error}
          </span>
        ) : null}
      </div>
      {pairing ? (
        <PairDialog
          view={view}
          openLink={(url) => void api.shell.openExternal(url)}
          onClose={() => {
            relay.cancelPair()
            setPairing(false)
          }}
        />
      ) : null}
    </div>
  )
}

function PairDialog({
  view,
  onClose,
  openLink
}: {
  view: RelayView
  onClose: () => void
  openLink: (url: string) => void
}) {
  const [copied, setCopied] = useState(false)
  const [now, setNow] = useState(() => Date.now())
  const [shown] = useState(view.pending)
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(t)
  }, [])
  const claimed = shown && !view.pending ? view.pairings.find((p) => p.id === shown.id) : null
  const left = shown ? Math.max(0, shown.expiresAt - now) : 0

  return (
    <Dialog onClose={onClose} labelledBy="pair-t" className="pair-dlg">
      <div className="dlg-hd">
        <h2 id="pair-t">Pair a browser</h2>
        <button type="button" className="icon-btn" aria-label="Close (Esc)" onClick={onClose}>
          <IconClose />
        </button>
      </div>
      <div className="dlg-bd">
        {claimed ? (
          <p role="status">
            Paired with <b>{claimed.name ?? 'a browser'}</b>
            {claimed.confirmed ? '.' : '… confirming.'} It can chat with this computer’s projects
            now.
          </p>
        ) : shown && left > 0 ? (
          <>
            <p className="ex">
              Open this link in the browser where you’re signed in to the same Wraithgrid account,
              then press Pair there.
            </p>
            <button type="button" className="btn pri" onClick={() => openLink(shown.link)}>
              <IconGlobe /> Open in browser
            </button>
            <div className="pair-link">
              <input
                className="inpt"
                readOnly
                value={shown.link}
                aria-label="Pairing link"
                onFocus={(e) => e.target.select()}
              />
              <button
                type="button"
                className="btn"
                onClick={() => {
                  void navigator.clipboard.writeText(shown.link).then(() => setCopied(true))
                }}
              >
                <IconCopy /> {copied ? 'Copied' : 'Copy link'}
              </button>
            </div>
            <p className="hint" role="status">
              Waiting for the browser… the code works once, for {Math.floor(left / 60_000)}:
              {String(Math.floor((left % 60_000) / 1000)).padStart(2, '0')}. Anyone with it could
              pair while signed in to your account: don’t share it.
            </p>
          </>
        ) : (
          <p role="status">This code has expired. Close and pair again for a new one.</p>
        )}
      </div>
      <div className="dlg-ft">
        <button type="button" className="btn pri" onClick={onClose}>
          {claimed ? 'Done' : 'Close'}
        </button>
      </div>
    </Dialog>
  )
}
