import { API_BASE, BASE_URL, STANDALONE } from './config'
import type { RadioTarget } from './radioXrpc'
import type { SyndicatedStation } from './radio'

export const RELATIVE_API_BASE = '__relative__'
export const TUNE_IN_CHANGED_EVENT = 'radio:tune-in-changed'

export interface TuneInStation {
  did: string
  url: string
  apiBase: string
  name: string
  description?: string | null
  updatedAt?: string | null
  indexedAt?: string | null
  local: boolean
}

export function normalizeStationUrl(url: string | null | undefined): string {
  const trimmed = (url ?? '').trim()
  if (!trimmed) return ''

  let normalized = ''
  try {
    const parsed = new URL(trimmed)
    parsed.hash = ''
    parsed.search = ''
    normalized = parsed.href.replace(/\/+$/, '')
  } catch {
    normalized = trimmed.replace(/\/+$/, '')
  }

  if (STANDALONE && typeof window !== 'undefined' && normalized === window.location.origin.replace(/\/+$/, '')) {
    return ''
  }
  return normalized
}

export function stationListKey(url: string): string {
  return normalizeStationUrl(url).toLowerCase()
}

export function labelFromStationUrl(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

function firstConfiguredPublicBase(candidates: string[]): string {
  for (const candidate of candidates) {
    const url = normalizeStationUrl(candidate)
    if (!url) continue
    try {
      const parsed = new URL(url)
      if (parsed.protocol === 'http:' || parsed.protocol === 'https:') return url
    } catch {
      continue
    }
  }
  return ''
}

export function defaultStationUrl(): string {
  return firstConfiguredPublicBase([BASE_URL, API_BASE]) || normalizeStationUrl(window.location.origin)
}

export function defaultStationApiBase(): string {
  const local = window.location.hostname === 'localhost' || window.location.hostname === '127.0.0.1'
  return firstConfiguredPublicBase([API_BASE, BASE_URL]) || normalizeStationUrl(local ? '' : window.location.origin)
}

export function stationStorageKey(defaultUrl: string = defaultStationUrl()): string {
  return `radio_tune_in:${defaultUrl || window.location.origin}`
}

export function readSelectedStationUrl(): string {
  return normalizeStationUrl(localStorage.getItem(stationStorageKey()) || defaultStationUrl())
}

export function writeSelectedStationUrl(url: string): string {
  const normalized = normalizeStationUrl(url)
  if (!normalized) return ''
  localStorage.setItem(stationStorageKey(), normalized)
  window.dispatchEvent(new CustomEvent(TUNE_IN_CHANGED_EVENT, { detail: { url: normalized } }))
  return normalized
}

export function localTuneInStation(): TuneInStation {
  return {
    did: 'local',
    url: defaultStationUrl(),
    apiBase: defaultStationApiBase(),
    name: 'this radio',
    description: null,
    updatedAt: null,
    indexedAt: null,
    local: true,
  }
}

export function tuneInStationsFrom(syndicatedStations: SyndicatedStation[] = []): TuneInStation[] {
  const stations = new Map<string, TuneInStation>()
  const local = localTuneInStation()

  if (!STANDALONE) {
    stations.set(stationListKey(local.url), local)
  }

  for (const station of syndicatedStations) {
    const url = normalizeStationUrl(station.url)
    if (!url) continue
    const key = stationListKey(url)
    if (key === stationListKey(local.url)) continue
    stations.set(key, {
      did: station.did,
      url,
      apiBase: url,
      name: station.name || labelFromStationUrl(url),
      description: station.description,
      updatedAt: station.updatedAt,
      indexedAt: station.indexedAt,
      local: false,
    })
  }

  return [...stations.values()].sort((left, right) => {
    if (left.local) return -1
    if (right.local) return 1
    return left.name.localeCompare(right.name)
  })
}

export function selectedTuneInStationFrom(stations: TuneInStation[], selectedUrl: string = readSelectedStationUrl()): TuneInStation {
  const normalized = normalizeStationUrl(selectedUrl)
  const selectedKey = stationListKey(normalized)
  const found = stations.find((station) => stationListKey(station.url) === selectedKey)
  if (found) return found

  try {
    const parsed = new URL(normalized)
    if (parsed.protocol === 'http:' || parsed.protocol === 'https:') {
      return {
        did: 'remembered',
        url: normalized,
        apiBase: normalized,
        name: labelFromStationUrl(normalized),
        description: null,
        updatedAt: null,
        indexedAt: null,
        local: false,
      }
    }
  } catch {
    // Fall through to the local station when the remembered value is not a URL.
  }

  if (stations.length > 0) {
    return stations.find(s => !s.local) || stations[0]
  }
  if (STANDALONE) {
    return {
      did: 'loading',
      url: '',
      apiBase: '',
      name: 'loading station...',
      description: null,
      updatedAt: null,
      indexedAt: null,
      local: false,
    }
  }
  return localTuneInStation()
}

export function stationResourceKey(station: TuneInStation): string {
  if (station.local) return station.apiBase || RELATIVE_API_BASE
  return (station.apiBase || station.url) || RELATIVE_API_BASE
}

export function stationRadioTarget(station: TuneInStation): RadioTarget {
  return {
    did: station.did === 'local' || station.did === 'remembered' ? undefined : station.did,
    baseUrl: station.local ? station.apiBase : (station.apiBase || station.url),
  }
}
