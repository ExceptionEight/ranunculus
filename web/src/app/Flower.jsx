// The station's glyph: the logo's flower. At rest it is the flower; while the station is busy it moves — which way,
// by what the station does. The motion is shared/flower.js (led by the station's state:
// nothing is cut short, one is brought to rest before another begins); this draws it, straight into the SVG, a frame
// at a time, and only while something moves.
import { useEffect, useRef } from 'react'
import { LAYERS } from '../shared/flower-shapes.js'
import { createFlower, PETALS } from '../shared/flower.js'

/** What the station does → what plays: listening and answering breathe; thinking, a command being carried out
 * and a search are the ring, going round until done; music turns the flower as a record. Anything else: the flower,
 * standing. */
const PLAYS = { listen: 'breathe', speak: 'breathe', think: 'ring', search: 'ring', music: 'record' }

/** A ring going on — an alarm or a timer — is the alarm, whatever the panel says. */
function playing(d) {
  if (!d?.online) return null
  if (d.ring) return 'alarm'
  return PLAYS[d.panel] || null
}

/** A petal `p` of the way to its dot, as path data (to a tenth: the box is 512 for some 40 px). */
function between(petal, dot, p) {
  let d = 'M'
  for (let i = 0; i < petal.length; i++) {
    d += (i === 2 ? 'C' : i ? ' ' : '') + Math.round((petal[i] + (dot[i] - petal[i]) * p) * 10) / 10
  }
  return d + 'Z'
}

/** The flower in `svg` (as <Glyph> lays it out), led by a mode: → {play(mode), stop()}. */
function mount(svg) {
  const flower = createFlower()
  const parts = [...svg.children].map((g) => ({
    g,
    whole: g.firstElementChild,
    petals: g.lastElementChild,
    turn: null,
    split: null,
    p: null,
  }))
  const calm = window.matchMedia?.('(prefers-reduced-motion: reduce)')
  let wanted = null
  let frame = 0
  let last = 0

  function draw() {
    parts.forEach((part, i) => {
      const o = flower.pose[i]
      const rot = Math.round((o.rot % 360) * 100) / 100
      const turn =
        (rot ? `rotate(${rot} 256 256)` : '') +
        (o.scale !== 1 ? `translate(256 256) scale(${Math.round(o.scale * 1e4) / 1e4}) translate(-256 -256)` : '')
      if (turn !== part.turn) {
        part.turn = turn
        part.g.setAttribute('transform', turn)
      }
      const split = o.p > 0
      if (split !== part.split) {
        part.split = split
        part.whole.style.display = split ? 'none' : ''
        part.petals.style.display = split ? '' : 'none'
      }
      if (!split) return
      if (o.p === part.p) return
      part.p = o.p
      const dots = flower.dots(i)
      for (let k = 0; k < dots.length; k++)
        part.petals.children[k].setAttribute('d', between(PETALS[i][k], dots[k], o.p))
    })
  }

  function tick(t) {
    frame = 0
    // a tab not shown gets no frames: back in it, the motion goes on from where it stood
    flower.advance(Math.min(0.1, Math.max(0, (t - last) / 1000)))
    last = t
    draw()
    if (flower.busy()) frame = requestAnimationFrame(tick)
  }

  function play(mode) {
    wanted = mode
    if (calm?.matches) {
      // no motion asked for: the flower, or the ring's dots, standing
      cancelAnimationFrame(frame)
      frame = 0
      flower.jump(mode)
      draw()
      return
    }
    flower.set(mode)
    draw()
    if (!frame && flower.busy()) {
      last = performance.now()
      frame = requestAnimationFrame(tick)
    }
  }

  const again = () => play(wanted)
  calm?.addEventListener?.('change', again)
  draw()
  return {
    play,
    stop() {
      cancelAnimationFrame(frame)
      frame = 0
      calm?.removeEventListener?.('change', again)
    },
  }
}

/** A station as a picture: the flower, grey when the station is not there, moving while it is busy. */
export function Glyph({ device, size = 34 }) {
  const svg = useRef(null)
  const flower = useRef(null)
  const mode = playing(device)
  useEffect(() => {
    const f = (flower.current = mount(svg.current))
    return () => f.stop()
  }, [])
  useEffect(() => {
    flower.current.play(mode)
  }, [mode])
  return (
    <svg
      ref={svg}
      className={`glyph${device?.online ? '' : ' offline'}`}
      width={size}
      height={size}
      viewBox="0 0 512 512"
      aria-hidden="true"
    >
      {LAYERS.map((l, i) => (
        <g key={i}>
          <path d={l.whole} />
          <g>
            {l.petals.map((_, k) => (
              <path key={k} />
            ))}
          </g>
        </g>
      ))}
    </svg>
  )
}
