// Settings → about: the version, the server.
import { useEffect, useState } from 'react'
import { api } from '../../shared/api.js'
import { useStore } from '../../shared/store.js'
import Pane from './Pane.jsx'

export default function About() {
  const version = useStore((s) => s.version)
  const [info, setInfo] = useState(null)
  useEffect(() => {
    api.info().then(setInfo, () => {})
  }, [])
  return (
    <Pane title="О программе">
      <section className="card">
        <div className="r" style={{ borderTop: 0, paddingTop: 0 }}>
          <img src="/icon.svg" alt="" width={56} height={56} style={{ borderRadius: 14 }} />
          <div className="r-text">
            <b style={{ fontSize: 18 }}>Ranunculus</b>
          </div>
        </div>
        <div className="rows">
          <div className="r">
            <div className="r-text">
              <b>Версия сервера</b>
            </div>
            <div className="r-ctl num">{info?.version || version}</div>
          </div>
          <div className="r">
            <div className="r-text">
              <b>Адрес</b>
            </div>
            <div className="r-ctl">
              <code>{location.host}</code>
            </div>
          </div>
        </div>
      </section>
    </Pane>
  )
}
