// Adapter for the bundled ttyd client: its Preact Terminal owns the socket, while
// window.term exposes only xterm. Keep these version-specific details together.
type TtydClient = {
  terminal: unknown
  socket: WebSocket
  resizeOverlay: boolean
  reconnect: boolean
  doReconnect: boolean
  overlayAddon: { overlayNode: HTMLElement; showOverlay: (...args: unknown[]) => void }
  onSocketClose: EventListener
  dispose(): void
}
type VNode = { __c?: { xterm?: TtydClient }; __k?: (VNode | null)[] }

export function configureTtydClient(doc: Document, terminal: unknown, disconnected: () => void, preferencesApplied: () => void = () => {}) {
  function find(node?: VNode | null): TtydClient | undefined {
    const client = node?.__c?.xterm
    if (client?.terminal === terminal) return client
    for (const child of node?.__k || []) {
      const found = find(child)
      if (found) return found
    }
  }
  const client = find((doc.body as HTMLElement & { __k?: VNode }).__k)
  if (!client?.socket) return
  client.resizeOverlay = false
  client.reconnect = false
  client.doReconnect = false
  client.overlayAddon.overlayNode.style.display = 'none'
  client.overlayAddon.overlayNode.remove()
  // ttyd's @bind decorator exposes a getter without a setter.
  Object.defineProperty(client.overlayAddon, 'showOverlay', { configurable: true, value: () => {} })
  // disableReconnect alone still installs ttyd's Enter-to-reconnect handler.
  client.socket.removeEventListener('close', client.onSocketClose)
  const close = () => { client.dispose(); disconnected() }
  client.socket.addEventListener('close', close)
  // Server preference packets (type '2') can arrive after the frame's load event.
  const message = (event: MessageEvent) => {
    if (typeof event.data !== 'string' && new Uint8Array(event.data)[0] === 50) preferencesApplied()
  }
  client.socket.addEventListener('message', message)
  return {
    socket: client.socket,
    cleanup: () => {
      client.socket.removeEventListener('close', close)
      client.socket.removeEventListener('message', message)
    },
  }
}
