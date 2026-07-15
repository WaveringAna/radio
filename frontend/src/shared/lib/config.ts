export const API_BASE = import.meta.env.VITE_API_BASE ?? ''
export const BASE_URL = import.meta.env.VITE_BASE_URL ?? ''
export const RADIO_SERVICE_DID = import.meta.env.VITE_RADIO_SERVICE_DID ?? ''
export const RADIO_SERVICE_ID = import.meta.env.VITE_RADIO_SERVICE_ID ?? '#radio_xrpc'
export const SYNDICATION_WORKER_BASE = import.meta.env.VITE_SYNDICATION_WORKER_BASE
  || (import.meta.env.DEV ? 'http://127.0.0.1:3300' : '')
