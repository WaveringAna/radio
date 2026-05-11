import { ChevronDown, Radio, RadioOff } from 'lucide-solid'
import { createMemo, createSignal, For, Index, onCleanup, Show, type Accessor } from 'solid-js'

const EQ_STORAGE_KEY = 'radio_eq_bands'
const EQ_PRESETS_STORAGE_KEY = 'radio_eq_named_presets'
const NORMALIZATION_STORAGE_KEY = 'radio_playback_normalization'
const LEGACY_EQ_STORAGE_KEY = 'radio_eq_gains'
const EQ_MIN_FREQUENCY = 20
const EQ_MAX_FREQUENCY = 20000
const EQ_GRAPH_POINTS = 80
const VISUALIZER_BAR_COUNT = 48
const EQ_FILTER_TYPES = ['peaking', 'lowshelf', 'highshelf'] as const
const EQ_GRAPH_FREQUENCIES = Array.from({ length: EQ_GRAPH_POINTS }, (_, index) => {
  const progress = index / (EQ_GRAPH_POINTS - 1)
  return EQ_MIN_FREQUENCY * (EQ_MAX_FREQUENCY / EQ_MIN_FREQUENCY) ** progress
})

type EqualizerFilterType = (typeof EQ_FILTER_TYPES)[number]

interface EqualizerBand {
  frequency: number
  gain: number
  type: EqualizerFilterType
}

interface EqualizerPreset {
  id: string
  label: string
  bands?: EqualizerBand[]
  gains?: number[]
}

export interface EqualizerController {
  bands: Accessor<EqualizerBand[]>
  customPresets: Accessor<EqualizerPreset[]>
  enabled: Accessor<boolean>
  graphPath: Accessor<string>
  normalizationEnabled: Accessor<boolean>
  /** Normalized waveform bars sampled from the live audio output. */
  visualizerBars: Accessor<number[]>
  applyPreset: (preset: EqualizerPreset) => void
  ensureGraph: () => Promise<void>
  reset: () => void
  savePreset: (name: string) => void
  setEnabled: (enabled: boolean) => void
  setNormalizationEnabled: (enabled: boolean) => void
  updateBand: (index: number, patch: Partial<EqualizerBand>) => void
}

interface EqualizerPanelProps {
  controller: EqualizerController
}

const DEFAULT_EQ_BANDS: EqualizerBand[] = [
  { frequency: 60, gain: 0, type: 'lowshelf' },
  { frequency: 170, gain: 0, type: 'peaking' },
  { frequency: 310, gain: 0, type: 'peaking' },
  { frequency: 600, gain: 0, type: 'peaking' },
  { frequency: 1000, gain: 0, type: 'peaking' },
  { frequency: 3000, gain: 0, type: 'peaking' },
  { frequency: 6000, gain: 0, type: 'peaking' },
  { frequency: 12000, gain: 0, type: 'highshelf' },
]

const EQ_PRESETS: EqualizerPreset[] = [
  { id: 'builtin-rock', label: 'rock', gains: [4, 3, -1, -2, 1, 3, 5, 5] },
  { id: 'builtin-dance', label: 'dance', gains: [5, 4, 1, 0, -2, 1, 4, 5] },
  { id: 'builtin-v', label: 'v', gains: [6, 4, 2, -2, -2, 2, 4, 6] },
  { id: 'builtin-loudness', label: 'loudness', gains: [5, 3, 1, 0, 0, 1, 3, 4] },
]

function clampNumber(value: number, min: number, max: number): number {
  return Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : min
}

function formatFrequency(frequency: number): string {
  return frequency >= 1000 ? `${Number((frequency / 1000).toFixed(1))}k` : `${Math.round(frequency)}`
}

function readEqualizerBands(): EqualizerBand[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(EQ_STORAGE_KEY) ?? 'null')
    if (Array.isArray(parsed) && parsed.length === DEFAULT_EQ_BANDS.length) {
      return parsed.map((band, index) => ({
        frequency: clampNumber(Number(band?.frequency), EQ_MIN_FREQUENCY, EQ_MAX_FREQUENCY) || DEFAULT_EQ_BANDS[index].frequency,
        gain: Number.isFinite(Number(band?.gain)) ? clampNumber(Number(band.gain), -12, 12) : 0,
        type: EQ_FILTER_TYPES.includes(band?.type) ? band.type : DEFAULT_EQ_BANDS[index].type,
      }))
    }

    const legacyGains = JSON.parse(localStorage.getItem(LEGACY_EQ_STORAGE_KEY) ?? '[]')
    if (Array.isArray(legacyGains) && legacyGains.length === DEFAULT_EQ_BANDS.length) {
      return DEFAULT_EQ_BANDS.map((band, index) => ({
        ...band,
        gain: Number.isFinite(Number(legacyGains[index])) ? clampNumber(Number(legacyGains[index]), -12, 12) : 0,
      }))
    }
  } catch {
    // ignore cooked localStorage and fall back to defaults.
  }
  return DEFAULT_EQ_BANDS.map((band) => ({ ...band }))
}

function writeEqualizerBands(bands: EqualizerBand[]): void {
  localStorage.setItem(EQ_STORAGE_KEY, JSON.stringify(bands))
}

function normalizeEqualizerBand(band: Partial<EqualizerBand>, fallback: EqualizerBand): EqualizerBand {
  return {
    frequency: clampNumber(Number(band.frequency), EQ_MIN_FREQUENCY, EQ_MAX_FREQUENCY) || fallback.frequency,
    gain: Number.isFinite(Number(band.gain)) ? clampNumber(Number(band.gain), -12, 12) : fallback.gain,
    type: EQ_FILTER_TYPES.includes(band.type as EqualizerFilterType) ? (band.type as EqualizerFilterType) : fallback.type,
  }
}

function readCustomPresets(): EqualizerPreset[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(EQ_PRESETS_STORAGE_KEY) ?? '[]')
    if (!Array.isArray(parsed)) return []
    return parsed
      .filter((preset) => typeof preset?.label === 'string' && Array.isArray(preset?.bands))
      .map((preset) => ({
        id: typeof preset.id === 'string' ? preset.id : `custom-${preset.label}`,
        label: preset.label.trim(),
        bands: preset.bands.map((band: Partial<EqualizerBand>, index: number) => normalizeEqualizerBand(band, DEFAULT_EQ_BANDS[index] ?? DEFAULT_EQ_BANDS[0])),
      }))
      .filter((preset) => preset.label && preset.bands?.length === DEFAULT_EQ_BANDS.length)
  } catch {
    return []
  }
}

function writeCustomPresets(presets: EqualizerPreset[]): void {
  localStorage.setItem(EQ_PRESETS_STORAGE_KEY, JSON.stringify(presets))
}

function readNormalizationEnabled(): boolean {
  return localStorage.getItem(NORMALIZATION_STORAGE_KEY) !== 'off'
}

function writeNormalizationEnabled(enabled: boolean): void {
  localStorage.setItem(NORMALIZATION_STORAGE_KEY, enabled ? 'on' : 'off')
}

function equalizerBandGainAt(band: EqualizerBand, frequency: number): number {
  const distance = Math.log2(frequency / band.frequency)
  if (band.type === 'lowshelf') return band.gain / (1 + Math.exp(distance * 5))
  if (band.type === 'highshelf') return band.gain / (1 + Math.exp(-distance * 5))
  return band.gain * Math.exp(-0.5 * (distance / 0.55) ** 2)
}

/**
 * Creates a Web Audio equalizer controller for the provided audio element.
 * @param getAudioElement Accessor returning the managed HTML audio element.
 * @returns Equalizer state and imperative graph controls.
 */
export function createEqualizerController(getAudioElement: () => HTMLAudioElement | undefined): EqualizerController {
  const [equalizerBands, setEqualizerBands] = createSignal(readEqualizerBands())
  const [customPresets, setCustomPresets] = createSignal(readCustomPresets())
  const [enabled, setEnabledSignal] = createSignal(true)
  const [normalizationEnabled, setNormalizationEnabledSignal] = createSignal(readNormalizationEnabled())
  const [visualizerBars, setVisualizerBars] = createSignal(Array.from({ length: VISUALIZER_BAR_COUNT }, () => 0.08))
  let audioContext: AudioContext | null = null
  let audioSource: MediaElementAudioSourceNode | null = null
  let analyser: AnalyserNode | null = null
  let compressor: DynamicsCompressorNode | null = null
  let normalizationGain: GainNode | null = null
  let equalizerFilters: BiquadFilterNode[] = []
  let persistenceTimer: number | null = null
  let visualizerFrame: number | null = null

  const scheduleEqualizerPersistence = (bands: EqualizerBand[]) => {
    if (persistenceTimer !== null) window.clearTimeout(persistenceTimer)
    persistenceTimer = window.setTimeout(() => {
      persistenceTimer = null
      writeEqualizerBands(bands)
    }, 180)
  }

  const effectiveGain = (gain: number) => (enabled() ? gain : 0)

  const applyNormalization = () => {
    if (!audioContext || !compressor || !normalizationGain) return
    const now = audioContext.currentTime
    if (normalizationEnabled()) {
      compressor.threshold.setTargetAtTime(-24, now, 0.02)
      compressor.knee.setTargetAtTime(24, now, 0.02)
      compressor.ratio.setTargetAtTime(8, now, 0.02)
      compressor.attack.setTargetAtTime(0.003, now, 0.02)
      compressor.release.setTargetAtTime(0.25, now, 0.02)
      normalizationGain.gain.setTargetAtTime(1.16, now, 0.02)
      return
    }

    compressor.threshold.setTargetAtTime(0, now, 0.02)
    compressor.knee.setTargetAtTime(0, now, 0.02)
    compressor.ratio.setTargetAtTime(1, now, 0.02)
    normalizationGain.gain.setTargetAtTime(1, now, 0.02)
  }

  const applyFilters = (bands: EqualizerBand[]) => {
    equalizerFilters.forEach((filter, index) => {
      const band = bands[index]
      if (!band) return
      filter.type = band.type
      filter.frequency.setTargetAtTime(band.frequency, audioContext?.currentTime ?? 0, 0.015)
      filter.gain.setTargetAtTime(effectiveGain(band.gain), audioContext?.currentTime ?? 0, 0.015)
    })
  }

  const graphPath = createMemo(() => {
    const bands = equalizerBands()
    return EQ_GRAPH_FREQUENCIES.map((frequency, index) => {
      const gain = clampNumber(bands.reduce((total, band) => total + equalizerBandGainAt(band, frequency), 0), -12, 12)
      const x = (index / (EQ_GRAPH_POINTS - 1)) * 100
      const y = 50 - (gain / 12) * 40
      return `${index === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`
    }).join(' ')
  })

  const updateVisualizer = () => {
    if (!analyser) return
    const samples = new Uint8Array(analyser.fftSize)
    analyser.getByteTimeDomainData(samples)
    const bucketSize = Math.floor(samples.length / VISUALIZER_BAR_COUNT)
    setVisualizerBars((current) => current.map((previous, bucketIndex) => {
      const start = bucketIndex * bucketSize
      const end = Math.min(samples.length, start + bucketSize)
      let peak = 0
      for (let index = start; index < end; index += 1) {
        peak = Math.max(peak, Math.abs(samples[index] - 128) / 128)
      }
      return previous * 0.32 + Math.max(0.025, Math.min(1, peak * 4.2)) * 0.68
    }))
    visualizerFrame = window.requestAnimationFrame(updateVisualizer)
  }

  const startVisualizer = () => {
    if (visualizerFrame === null) {
      visualizerFrame = window.requestAnimationFrame(updateVisualizer)
    }
  }

  const ensureGraph = async (): Promise<void> => {
    const audioElement = getAudioElement()
    if (!audioElement) return
    audioContext ??= new AudioContext()
    if (!audioSource) {
      audioSource = audioContext.createMediaElementSource(audioElement)
      analyser = audioContext.createAnalyser()
      analyser.fftSize = 1024
      analyser.smoothingTimeConstant = 0.42
      compressor = audioContext.createDynamicsCompressor()
      normalizationGain = audioContext.createGain()
      applyNormalization()
      equalizerFilters = equalizerBands().map((band) => {
        const filter = audioContext!.createBiquadFilter()
        filter.type = band.type
        filter.frequency.value = band.frequency
        filter.Q.value = 1
        filter.gain.value = effectiveGain(band.gain)
        return filter
      })
      const chain = [audioSource, ...equalizerFilters, compressor, normalizationGain, analyser, audioContext.destination]
      chain.slice(0, -1).forEach((node, index) => node.connect(chain[index + 1]))
    }
    if (audioContext.state === 'suspended') {
      await audioContext.resume().catch(() => undefined)
    }
    startVisualizer()
  }

  const updateBand = (index: number, patch: Partial<EqualizerBand>) => {
    const next = equalizerBands().map((band, currentIndex) => {
      if (currentIndex !== index) return band
      return {
        frequency: patch.frequency === undefined ? band.frequency : clampNumber(patch.frequency, EQ_MIN_FREQUENCY, EQ_MAX_FREQUENCY),
        gain: patch.gain === undefined ? band.gain : clampNumber(patch.gain, -12, 12),
        type: patch.type ?? band.type,
      }
    })
    setEqualizerBands(next)
    applyFilters(next)
    scheduleEqualizerPersistence(next)
  }

  const applyBands = (bands: EqualizerBand[]) => {
    setEqualizerBands(bands)
    applyFilters(bands)
    scheduleEqualizerPersistence(bands)
  }

  const applyPreset = (preset: EqualizerPreset) => {
    if (preset.bands) {
      applyBands(preset.bands.map((band) => ({ ...band })))
      return
    }
    applyBands(equalizerBands().map((band, index) => ({ ...band, gain: clampNumber(preset.gains?.[index] ?? 0, -12, 12) })))
  }

  const savePreset = (name: string) => {
    const label = name.trim()
    if (!label) return
    const preset = {
      id: `custom-${label.toLowerCase().replace(/[^a-z0-9]+/g, '-') || Date.now()}`,
      label,
      bands: equalizerBands().map((band) => ({ ...band })),
    }
    const next = [...customPresets().filter((current) => current.label.toLowerCase() !== label.toLowerCase()), preset]
    setCustomPresets(next)
    writeCustomPresets(next)
  }

  const reset = () => {
    applyBands(DEFAULT_EQ_BANDS.map((band) => ({ ...band })))
  }

  const setEnabled = (isEnabled: boolean) => {
    setEnabledSignal(isEnabled)
    applyFilters(equalizerBands())
  }

  const setNormalizationEnabled = (isEnabled: boolean) => {
    setNormalizationEnabledSignal(isEnabled)
    writeNormalizationEnabled(isEnabled)
    applyNormalization()
  }

  onCleanup(() => {
    if (persistenceTimer !== null) window.clearTimeout(persistenceTimer)
    if (visualizerFrame !== null) window.cancelAnimationFrame(visualizerFrame)
    writeEqualizerBands(equalizerBands())
    equalizerFilters.forEach((filter) => filter.disconnect())
    normalizationGain?.disconnect()
    compressor?.disconnect()
    analyser?.disconnect()
    audioSource?.disconnect()
    void audioContext?.close().catch(() => undefined)
  })

  return {
    bands: equalizerBands,
    customPresets,
    enabled,
    graphPath,
    normalizationEnabled,
    visualizerBars,
    applyPreset,
    ensureGraph,
    reset,
    savePreset,
    setEnabled,
    setNormalizationEnabled,
    updateBand,
  }
}

/**
 * Renders the collapsible 8-band equalizer controls and curve preview.
 * @param props Equalizer controller props.
 * @returns The equalizer panel UI.
 */
export function EqualizerPanel(props: EqualizerPanelProps) {
  const [open, setOpen] = createSignal(false)
  const [presetName, setPresetName] = createSignal('')
  const presets = createMemo(() => [...EQ_PRESETS, ...props.controller.customPresets()])

  // Attach the Web Audio graph the first time the user actually interacts with
  // the EQ. Doing it earlier breaks iOS background playback, since routing the
  // <audio> through MediaElementSource makes Safari treat it as Web Audio and
  // suspend it on lock screen / app switch.
  const ensureGraph = () => void props.controller.ensureGraph()

  const togglePanel = () => {
    const next = !open()
    setOpen(next)
    if (next) ensureGraph()
  }

  const applyPresetById = (id: string) => {
    const preset = presets().find((candidate) => candidate.id === id)
    if (preset) {
      ensureGraph()
      props.controller.applyPreset(preset)
    }
  }

  const saveCurrentPreset = () => {
    props.controller.savePreset(presetName())
    setPresetName('')
  }

  return (
    <section class="equalizer-panel" classList={{ open: open() }}>
      <div class="equalizer-header">
        <span class="equalizer-title">equalizer</span>
        <button
          class="normalization-toggle"
          type="button"
          aria-pressed={props.controller.normalizationEnabled()}
          aria-label={props.controller.normalizationEnabled() ? 'disable playback normalization' : 'enable playback normalization'}
          onClick={() => {
            ensureGraph()
            props.controller.setNormalizationEnabled(!props.controller.normalizationEnabled())
          }}
        >
          normalize
        </button>
        <button
          class="equalizer-power"
          type="button"
          aria-pressed={props.controller.enabled()}
          aria-label={props.controller.enabled() ? 'disable equalizer' : 'enable equalizer'}
          onClick={() => {
            ensureGraph()
            props.controller.setEnabled(!props.controller.enabled())
          }}
        >
          <Show when={props.controller.enabled()} fallback={<RadioOff size={20} />}>
            <Radio size={20} />
          </Show>
        </button>
        <button class="equalizer-toggle" type="button" aria-expanded={open()} aria-label="toggle equalizer controls" onClick={togglePanel}>
          <ChevronDown size={19} />
        </button>
      </div>
      <Show when={open()}>
        <div class="equalizer-controls" aria-label="8-band equalizer">
          <div class="equalizer-presets" aria-label="equalizer presets">
            <label>
              <span>preset</span>
              <select class="equalizer-preset-select" value="" onChange={(event) => applyPresetById(event.currentTarget.value)}>
                <option value="" disabled>
                  select preset
                </option>
                <For each={presets()}>
                  {(preset) => <option value={preset.id}>{preset.label}</option>}
                </For>
              </select>
            </label>
            <label>
              <span>save as</span>
              <input
                class="equalizer-preset-name"
                type="text"
                value={presetName()}
                placeholder="preset name"
                onInput={(event) => setPresetName(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') saveCurrentPreset()
                }}
              />
            </label>
            <button class="equalizer-preset" type="button" disabled={!presetName().trim()} onClick={saveCurrentPreset}>
              save
            </button>
          </div>
          <Index each={props.controller.bands()}>
            {(band, index) => (
              <div class="equalizer-band">
                <span class="equalizer-band-label">{formatFrequency(band().frequency)}hz</span>
                <input
                  class="equalizer-gain"
                  aria-label={`${formatFrequency(band().frequency)}hz gain`}
                  type="range"
                  min="-12"
                  max="12"
                  step="0.5"
                  value={band().gain}
                  onInput={(event) => {
                    ensureGraph()
                    props.controller.updateBand(index, { gain: event.currentTarget.valueAsNumber })
                  }}
                />
                <small>{band().gain.toFixed(1)} db</small>
                <input
                  class="equalizer-frequency"
                  aria-label={`band ${index + 1} frequency`}
                  type="number"
                  min={EQ_MIN_FREQUENCY}
                  max={EQ_MAX_FREQUENCY}
                  step="10"
                  value={Math.round(band().frequency)}
                  onInput={(event) => props.controller.updateBand(index, { frequency: event.currentTarget.valueAsNumber })}
                />
                <select
                  class="equalizer-filter"
                  aria-label={`band ${index + 1} filter type`}
                  value={band().type}
                  onChange={(event) => props.controller.updateBand(index, { type: event.currentTarget.value as EqualizerFilterType })}
                >
                  <option value="peaking">peak</option>
                  <option value="lowshelf">low shelf</option>
                  <option value="highshelf">high shelf</option>
                </select>
              </div>
            )}
          </Index>
          <div class="equalizer-graph" aria-label="equalizer curve preview">
            <svg viewBox="0 0 100 100" preserveAspectRatio="none" role="img">
              <line class="equalizer-graph-zero" x1="0" y1="50" x2="100" y2="50" />
              <path class="equalizer-graph-curve" d={props.controller.graphPath()} />
            </svg>
            <div class="equalizer-graph-labels" aria-hidden="true">
              <span>20hz</span>
              <span>0db</span>
              <span>20khz</span>
            </div>
          </div>
          <button class="equalizer-reset" type="button" onClick={props.controller.reset}>
            reset eq
          </button>
        </div>
      </Show>
    </section>
  )
}
