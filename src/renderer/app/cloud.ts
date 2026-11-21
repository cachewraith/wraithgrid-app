// The Wraithgrid account's state for the whole window: the sign-in gate (AuthGate), the
// sidebar's profile button and Settings read one store, fed by `cloud:changed`. Absent in the
// Electron build (no `api.cloud`): nothing is gated there.
import { useStore } from 'zustand'
import { createStore, type StoreApi } from 'zustand/vanilla'
import type { CloudStatus, WraithApi } from '@shared/ipc-contract'
import { useServices } from './services'

export type CloudApi = NonNullable<WraithApi['cloud']>

interface CloudState {
  /** null while the first answer is on its way. */
  status: CloudStatus | null
}

const stores = new WeakMap<CloudApi, StoreApi<CloudState>>()
const none = createStore<CloudState>(() => ({ status: null }))
let current: StoreApi<CloudState> | null = null

function storeFor(cloud: CloudApi): StoreApi<CloudState> {
  let store = stores.get(cloud)
  if (!store) {
    const s = createStore<CloudState>(() => ({ status: null }))
    store = s
    stores.set(cloud, s)
    void cloud.status().then((status) => s.setState({ status }))
    cloud.onChanged((status) => s.setState({ status }))
    // Picks up a new name or plan, and notices a session ended elsewhere.
    void cloud.refresh().then((status) => s.setState({ status }))
  }
  current = store
  return store
}

/** The account API and its status; null in a build without accounts. */
export function useCloud(): { cloud: CloudApi; status: CloudStatus | null } | null {
  const { api } = useServices()
  const status = useStore(api.cloud ? storeFor(api.cloud) : none, (s) => s.status)
  return api.cloud ? { cloud: api.cloud, status } : null
}

/** The sign-in screen covers the window (not signed in, or not known yet). */
export function isGated(): boolean {
  if (!current) return false
  const { status } = current.getState()
  return !status || !status.signedIn
}
