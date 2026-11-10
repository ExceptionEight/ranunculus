// Settings in groups: the assistant (how it talks, the home, its models, the sounds), access (this account, the
// users), the app (its look, about). On a wide screen the groups stay on the left and a section on the right; on a
// phone the list, then a section with a way back.
import { Navigate, NavLink, Route, Routes, useLocation } from 'react-router'
import {
  Bell,
  ChevronRight,
  House,
  Info,
  MessageCircle,
  Palette,
  Sparkles,
  UserRound,
  Users as UsersIcon,
} from 'lucide-react'
import { PageHead } from '../ui.jsx'
import About from './About.jsx'
import Account from './Account.jsx'
import Connections from './Connections.jsx'
import HomePlace from './HomePlace.jsx'
import Look from './Look.jsx'
import Pane from './Pane.jsx'
import Sounds from './Sounds.jsx'
import Talk from './Talk.jsx'
import Users from './Users.jsx'

const GROUPS = [
  [
    'Ассистент',
    [
      ['talk', 'Общение', MessageCircle],
      ['home', 'Дом', House],
      ['models', 'Модели', Sparkles],
      ['sounds', 'Звуки', Bell],
    ],
  ],
  [
    'Доступ',
    [
      ['account', 'Аккаунт', UserRound],
      ['users', 'Пользователи', UsersIcon],
    ],
  ],
  [
    'Приложение',
    [
      ['look', 'Оформление', Palette],
      ['about', 'О программе', Info],
    ],
  ],
]

export default function Settings() {
  const { pathname } = useLocation()
  const hasPane = pathname.replace(/\/$/, '') !== '/settings'
  return (
    <main className={`page settings-page${hasPane ? ' has-pane' : ''}`}>
      <PageHead title="Настройки" />
      <div className="settings">
        <nav className="card set-nav" aria-label="Разделы настроек">
          {GROUPS.map(([group, items]) => (
            <div key={group} style={{ display: 'contents' }}>
              <p className="set-group">{group}</p>
              {items.map(([path, title, Icon]) => (
                <NavLink key={path} to={`/settings/${path}`}>
                  <span className="ico sm" aria-hidden="true">
                    <Icon size={18} />
                  </span>
                  <span className="label-text">{title}</span>
                  <ChevronRight size={18} aria-hidden="true" />
                </NavLink>
              ))}
            </div>
          ))}
        </nav>
        <div className="set-pane">
          <Routes>
            <Route index element={<DesktopDefault />} />
            <Route path="talk" element={<Talk />} />
            <Route path="home" element={<HomePlace />} />
            <Route path="models" element={<Connections Pane={Pane} />} />
            <Route path="sounds" element={<Sounds />} />
            <Route path="account" element={<Account />} />
            <Route path="users" element={<Users />} />
            <Route path="look" element={<Look />} />
            <Route path="about" element={<About />} />
            <Route path="*" element={<Navigate to="/settings" replace />} />
          </Routes>
        </div>
      </div>
    </main>
  )
}

/** /settings itself: on a wide screen the first section; on a phone the list alone. */
function DesktopDefault() {
  const wide = typeof window !== 'undefined' && window.matchMedia('(min-width: 900px)').matches
  return wide ? <Navigate to="/settings/talk" replace /> : null
}
