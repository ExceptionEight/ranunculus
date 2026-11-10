import { Link } from 'react-router'
import { ChevronLeft } from 'lucide-react'

/** A section's title, with a way back to the list on a phone. */
export default function Pane({ title, children }) {
  return (
    <>
      <div className="head-text">
        <Link to="/settings" className="back">
          <ChevronLeft size={18} aria-hidden="true" />
          Настройки
        </Link>
        <h2 className="pane-title">{title}</h2>
      </div>
      {children}
    </>
  )
}
