// Light, dark or as the system has it: chosen in Settings → Оформление, kept in this browser. The choice is
// data-theme on <html> (none: the system's); public/theme-boot.js sets it before the page draws, so it never flashes.
import { useSyncExternalStore } from 'react'

const KEY = 'ranunculus.theme'
export const MODES = ['system', 'light', 'dark']
const PAGE = { light: '#FBF0F3', dark: '#12090E' }
const listeners = new Set()

function read() {
  try {
    const v = localStorage.getItem(KEY)
    if (MODES.includes(v)) return v
  } catch {
    /* private mode: the system's */
  }
  return 'system'
}

let mode = read()
const dark = typeof window !== 'undefined' ? window.matchMedia('(prefers-color-scheme: dark)') : null

/** What the page looks like now: 'light' | 'dark'. */
export function shown() {
  return mode === 'system' ? (dark?.matches ? 'dark' : 'light') : mode
}

function apply() {
  const root = document.documentElement
  if (mode === 'system') delete root.dataset.theme
  else root.dataset.theme = mode
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', PAGE[shown()])
}

export function setTheme(m) {
  mode = MODES.includes(m) ? m : 'system'
  try {
    localStorage.setItem(KEY, mode)
  } catch {
    /* kept for this page only */
  }
  apply()
  listeners.forEach((l) => l())
}

function subscribe(l) {
  listeners.add(l)
  return () => listeners.delete(l)
}

export function useTheme() {
  return useSyncExternalStore(subscribe, () => mode)
}

if (typeof window !== 'undefined') {
  apply()
  dark?.addEventListener('change', () => mode === 'system' && apply())
}
