// Voice typing in the chat box: mic → main records → local Whisper → text handed to the box
// (`onText`), never sent by itself. The first use offers the speech model's one-time download.
import { useCallback, useEffect, useRef, useState } from 'react'
import type { VoiceApi } from '@shared/ipc-contract'

export type VoiceState =
  | { kind: 'idle' }
  /** The model isn't downloaded: the bar offers it. */
  | { kind: 'offer'; bytes: number }
  | { kind: 'downloading'; done: number; total: number }
  | { kind: 'recording'; since: number }
  | { kind: 'transcribing' }
  | { kind: 'error'; message: string }

export interface VoiceTyping {
  state: VoiceState
  /** The last thing said (a new object each time), for the box to put in. */
  heard: { text: string } | null
  /** 0–1 while recording. */
  level: number
  /** The mic button: start, or finish and write it down. */
  toggle(): void
  /** Esc: stop without writing anything; also closes the offer or an error. */
  cancel(): void
  download(): void
}

export function useVoiceTyping(api: VoiceApi | undefined): VoiceTyping | null {
  const [state, setStateNow] = useState<VoiceState>({ kind: 'idle' })
  const [level, setLevel] = useState(0)
  const [heard, setHeard] = useState<{ text: string } | null>(null)
  // Read by the event callbacks; written with the state, never during render.
  const stateRef = useRef<VoiceState>({ kind: 'idle' })
  const setState = useCallback((s: VoiceState) => {
    stateRef.current = s
    setStateNow(s)
  }, [])

  const finish = useCallback(async () => {
    if (!api) return
    setState({ kind: 'transcribing' })
    setLevel(0)
    const r = await api.stop()
    if (!r.ok) setState({ kind: 'error', message: r.error })
    else {
      setState({ kind: 'idle' })
      if (r.text) setHeard({ text: r.text })
    }
  }, [api, setState])

  const start = useCallback(async () => {
    if (!api) return
    const r = await api.start()
    if (r.ok) setState({ kind: 'recording', since: Date.now() })
    else if (r.code === 'no_model') {
      const s = await api.status()
      setState(
        s.downloading
          ? { kind: 'downloading', done: 0, total: s.bytes }
          : { kind: 'offer', bytes: s.bytes }
      )
    } else setState({ kind: 'error', message: r.error })
  }, [api, setState])

  useEffect(() => {
    if (!api) return
    const offLevel = api.onLevel((l) => {
      if (stateRef.current.kind === 'recording') setLevel(l)
    })
    const offProgress = api.onProgress(({ done, total }) => {
      if (stateRef.current.kind === 'downloading') setState({ kind: 'downloading', done, total })
    })
    // Hit the time limit: write down what was said so far.
    const offEnded = api.onEnded(() => {
      if (stateRef.current.kind === 'recording') void finish()
    })
    return () => {
      offLevel()
      offProgress()
      offEnded()
    }
  }, [api, finish, setState])

  // A chat box going away mid-recording lets the microphone go.
  useEffect(
    () => () => {
      if (stateRef.current.kind === 'recording') void api?.cancel()
    },
    [api]
  )

  const download = useCallback(async () => {
    if (!api) return
    const total = stateRef.current.kind === 'offer' ? stateRef.current.bytes : 0
    setState({ kind: 'downloading', done: 0, total })
    const r = await api.download()
    if (!r.ok) {
      setState(/cancel/i.test(r.error) ? { kind: 'idle' } : { kind: 'error', message: r.error })
      return
    }
    // Downloaded: start listening right away, which is what the click was for.
    await start()
  }, [api, start, setState])

  const toggle = useCallback(() => {
    const k = stateRef.current.kind
    if (k === 'recording') void finish()
    else if (k === 'idle' || k === 'error') void start()
    else if (k === 'offer') void download()
  }, [finish, start, download])

  const cancel = useCallback(() => {
    const k = stateRef.current.kind
    if (k === 'recording') void api?.cancel()
    if (k === 'downloading') void api?.cancelDownload()
    if (k !== 'transcribing') {
      setState({ kind: 'idle' })
      setLevel(0)
    }
  }, [api, setState])

  if (!api) return null
  return { state, heard, level, toggle, cancel, download: () => void download() }
}
