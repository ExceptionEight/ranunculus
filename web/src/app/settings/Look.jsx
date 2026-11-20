// Settings → the look: light, dark or as the system has it.
import { setTheme, useTheme } from '../../shared/theme.js'
import Pane from './Pane.jsx'

const THEMES = [
  ['light', 'Светлая'],
  ['dark', 'Тёмная'],
  ['system', 'Системная'],
]

/** A tiny screen of the app in a theme: the page, the sidebar's edge, a card with the flower's dot and two lines. */
function Screen({ mode }) {
  return (
    <span className={`t-screen t-${mode}`}>
      <span className="t-card">
        <b />
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
              </span>
              <span className="t-name">{name}</span>
            </button>
          ))}
        </div>
      </section>
    </Pane>
  )
}
