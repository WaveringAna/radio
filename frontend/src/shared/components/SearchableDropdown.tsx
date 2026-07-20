import { createSignal, createMemo, Show, For, onCleanup } from 'solid-js'

interface SearchableDropdownProps {
  options: string[]
  placeholder: string
  onSelect: (value: string) => void
  /** Current selection, shown in the input while the menu is closed. */
  value?: string
  /** Optional per-option annotation (e.g. song counts), shown right-aligned. */
  counts?: Record<string, number>
}

export function SearchableDropdown(props: SearchableDropdownProps) {
  const [isOpen, setIsOpen] = createSignal(false)
  const [search, setSearch] = createSignal('')
  const [activeIndex, setActiveIndex] = createSignal(-1)
  let containerRef: HTMLDivElement | undefined
  let inputRef: HTMLInputElement | undefined

  const filteredOptions = createMemo(() => {
    const term = search().toLowerCase().trim()
    if (!term) return props.options
    return props.options.filter(opt => opt.toLowerCase().includes(term))
  })

  // Limit displayed options in DOM for performance
  const displayedOptions = createMemo(() => {
    return filteredOptions().slice(0, 100)
  })

  const selectOption = (option: string) => {
    props.onSelect(option)
    setIsOpen(false)
    setSearch('')
    setActiveIndex(-1)
    if (inputRef) {
      inputRef.blur()
    }
  }

  const handleClickOutside = (e: MouseEvent) => {
    if (containerRef && !containerRef.contains(e.target as Node)) {
      setIsOpen(false)
      setSearch('')
      setActiveIndex(-1)
    }
  }

  document.addEventListener('click', handleClickOutside)
  onCleanup(() => {
    document.removeEventListener('click', handleClickOutside)
  })

  const handleKeyDown = (e: KeyboardEvent) => {
    if (!isOpen()) {
      if (e.key === 'ArrowDown' || e.key === 'Enter') {
        setIsOpen(true)
        e.preventDefault()
      }
      return
    }

    const maxIdx = displayedOptions().length - 1

    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActiveIndex((prev) => (prev < maxIdx ? prev + 1 : 0))
      const activeEl = containerRef?.querySelector(`[data-index="${activeIndex()}"]`)
      activeEl?.scrollIntoView({ block: 'nearest' })
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActiveIndex((prev) => (prev > 0 ? prev - 1 : maxIdx))
      const activeEl = containerRef?.querySelector(`[data-index="${activeIndex()}"]`)
      activeEl?.scrollIntoView({ block: 'nearest' })
    } else if (e.key === 'Enter') {
      e.preventDefault()
      const idx = activeIndex()
      if (idx >= 0 && idx <= maxIdx) {
        selectOption(displayedOptions()[idx])
      } else if (displayedOptions().length > 0) {
        selectOption(displayedOptions()[0])
      }
    } else if (e.key === 'Escape') {
      e.preventDefault()
      setIsOpen(false)
      setSearch('')
      setActiveIndex(-1)
    }
  }

  return (
    <div ref={containerRef} class="searchable-dropdown-container">
      <div class="searchable-dropdown-input-wrapper">
        <input
          ref={inputRef}
          type="text"
          placeholder={props.placeholder}
          value={isOpen() ? search() : (props.value ?? '')}
          onInput={(e) => {
            setSearch(e.currentTarget.value)
            setIsOpen(true)
            setActiveIndex(-1)
          }}
          onFocus={() => setIsOpen(true)}
          onKeyDown={handleKeyDown}
          class="searchable-dropdown-input"
        />
        <span class="searchable-dropdown-arrow" onClick={() => setIsOpen(!isOpen())}>▾</span>
      </div>
      <Show when={isOpen() && (displayedOptions().length > 0 || search().trim() !== '')}>
        <ul class="searchable-dropdown-menu">
          <For each={displayedOptions()}>
            {(option, idx) => (
              <li
                data-index={idx()}
                onClick={() => selectOption(option)}
                class="searchable-dropdown-item"
                classList={{ active: idx() === activeIndex() }}
              >
                <span class="searchable-dropdown-item-label">{option}</span>
                <Show when={props.counts?.[option] !== undefined}>
                  <span class="searchable-dropdown-item-count">{props.counts?.[option]}</span>
                </Show>
              </li>
            )}
          </For>
          <Show when={filteredOptions().length > 100}>
            <li class="searchable-dropdown-item-muted">
              ...and {filteredOptions().length - 100} more (type to filter)
            </li>
          </Show>
          <Show when={displayedOptions().length === 0}>
            <li class="searchable-dropdown-item-empty">
              no matches found
            </li>
          </Show>
        </ul>
      </Show>
    </div>
  )
}
