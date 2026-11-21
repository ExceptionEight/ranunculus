// The frame: three places (Главная, Аналитика, Настройки) in a sidebar on a wide screen, with the stations
// under them, and in a tab bar at the bottom on a phone; a line on top when the server cannot be reached.
// Not signed in: the sign-in form instead; not known yet: that line only.
import { useEffect, useMemo, useState } from 'react'
import { Link, NavLink, Navigate, Route, Routes, useLocation } from 'react-router'
import { ChartColumn, LayoutGrid, Settings as Gear } from 'lucide-react'
import { useLiveStore } from '../shared/hooks.js'
import { deviceList, deviceName, useStore } from '../shared/store.js'
import { devicePath } from './ui.jsx'
import { Glyph } from './Flower.jsx'
import Home from './Home.jsx'
import Device from './Device.jsx'
import Thing from './House.jsx'
import Analytics from './Analytics.jsx'
import Settings from './settings/Settings.jsx'
import Login from './Login.jsx'

const PLACES = [
  ['/', 'Главная', LayoutGrid],
  ['/analytics', 'Аналитика', ChartColumn],
  ['/settings', 'Настройки', Gear],
]

/** Nothing while all is well; after 3 s without the server: what is going on. */
function LinkBanner() {
  const link = useStore((s) => s.link)
  const [show, setShow] = useState(false)
  useEffect(() => {
    if (link === 'live') {
      setShow(false)
      return
    }
    const t = setTimeout(() => setShow(true), 3000)
    return () => clearTimeout(t)
  }, [link])
  if (!show) return null
  return (
    <div className="banner" role="status">
      <span className="dot" aria-hidden="true" />
      {link === 'lost' ? 'Сервер недоступен' : 'Нет связи с сервером. Переподключение…'}
    </div>
  )
}

/** Главная is also where a station's page belongs, and a device's of the home. */
function placeActive(path, pathname) {
  if (path === '/') return pathname === '/' || pathname.startsWith('/devices') || pathname.startsWith('/house')
  return pathname.startsWith(path)
}

function Sidebar() {
  const { pathname } = useLocation()
  const devices = useStore((s) => s.devices)
  const user = useStore((s) => s.auth?.user)
  const list = useMemo(() => deviceList(devices), [devices])
  return (
    <aside className="side">
      <Link to="/" className="brand">
        <img src="/favicon.svg" alt="" width={22} height={22} />
        Ranunculus
      </Link>
      <nav className="nav" aria-label="Разделы">
        {PLACES.map(([path, title, Icon]) => (
          <NavLink
            key={path}
            to={path}
            className={() => (placeActive(path, pathname) && !pathname.startsWith('/devices') ? 'active' : '')}
          >
            <Icon size={18} aria-hidden="true" />
            {title}
          </NavLink>
        ))}
      </nav>
      {list.length > 0 && (
        <>
          <p className="side-label">Колонки</p>
          <nav className="nav" aria-label="Колонки">
            {list.map((d) => (
              <NavLink key={d.id} to={devicePath(d.id)} className={({ isActive }) => `sub${isActive ? ' active' : ''}`}>
                <Glyph device={d} size={20} />
                <span>{deviceName(d)}</span>
              </NavLink>
            ))}
          </nav>
        </>
      )}
      {user && (
        <div className="side-foot">
          <Link to="/settings/account" className="side-me">
            <span className="avatar" aria-hidden="true">
              {[...user.name][0]}
            </span>
            <span>{user.name}</span>
          </Link>
        </div>
      )}
    </aside>
  )
}

function TabBar() {
  const { pathname } = useLocation()
  return (
    <nav className="tabbar" aria-label="Разделы">
      {PLACES.map(([path, title, Icon]) => (
        <NavLink key={path} to={path} className={() => (placeActive(path, pathname) ? 'active' : '')}>
          <Icon size={20} aria-hidden="true" />
          {title}
        </NavLink>
      ))}
    </nav>
  )
}

export default function App() {
  useLiveStore()
  const auth = useStore((s) => s.auth)
  // a new page starts at its top; a station's own tabs keep the place
  const page = useLocation().pathname.split('/').slice(0, 3).join('/')
  // in braces: Chrome's scrollTo returns a promise now, and an effect that returns one crashes React on the next page
  useEffect(() => {
    window.scrollTo(0, 0)
  }, [page])
  if (!auth) return <LinkBanner />
  if (!auth.user) return <Login auth={auth} />
  return (
    <>
      <div className="shell">
        <Sidebar />
        <div className="main">
          <LinkBanner />
          <Routes>
            <Route path="/" element={<Home />} />
            <Route path="/devices/:id/*" element={<Device />} />
            <Route path="/house/:id" element={<Thing />} />
            <Route path="/analytics/*" element={<Analytics />} />
            <Route path="/settings/*" element={<Settings />} />
            <Route path="*" element={<Navigate to="/" replace />} />
          </Routes>
        </div>
      </div>
      <TabBar />
    </>
  )
}
