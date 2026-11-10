// Settings → the look: light, dark or as the system has it.
import { Check } from 'lucide-react'
import { setTheme, useTheme } from '../../shared/theme.js'
import Pane from './Pane.jsx'

const THEMES = [
  ['light', 'Светлая'],
  ['dark', 'Тёмная'],
  ['system', 'Системная'],
]

/** A tiny screen of the app in a theme: the backdrop, the raspberry tile, a glass card. */
function Screen({ mode }) {
  return (
    <span className={`t-screen t-${mode}`}>
      <span className="t-hero" />
      <span className="t-card">
        <b />
        <b />
      </span>
    </span>
  )
}

export default function Look() {
  const theme = useTheme()
  return (
    <Pane title="Оформление">
      <section className="card" aria-labelledby="look-theme">
        <h3 id="look-theme">Тема</h3>
        <div className="themes">
          {THEMES.map(([id, name]) => (
            <button
              key={id}
              type="button"
              className="theme-opt"
              aria-pressed={theme === id}
              onClick={() => setTheme(id)}
            >
              <span className="t-prev" aria-hidden="true">
                {id === 'system' ? (
                  <>
                    <Screen mode="light" />
                    <Screen mode="dark" />
                  </>
                ) : (
                  <Screen mode={id} />
                )}
                {theme === id && (
                  <span className="t-mark">
                    <Check size={13} strokeWidth={3} />
                  </span>
                )}
              </span>
              <span className="t-name">{name}</span>
            </button>
          ))}
        </div>
      </section>
    </Pane>
  )
}
