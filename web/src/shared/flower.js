// The station's glyph, moving: the flower in four ways (60 frames a second; the frame numbers, the sizes and the
// easings are below):
//
//   breathe  the layers swell one after another from the heart outwards and settle; 3.2 s, and again;
//   alarm    twice in 1.6 s the layers shake from side to side, less and less, and the heart beats;
//   ring     the flower comes apart into eight dots, the dots go round — for as long as it takes — and only then
//            it closes into the flower again;
//   record   the whole flower turns evenly, as a record does: it gets up to speed, and slows down to a stand.
//
// An animation file loops on a fixed clock; here the station decides. So this is their motion with a will: `set(mode)` says
// what is wanted, and nothing is ever cut short for it. What plays is brought to the flower at rest first — the ring
// goes on to where its dots stand on the petals' places (at most 0.15 s away) and closes as it always does; a breath
// or a shake is let down within 0.3 s, or sooner if it was about to rest anyway; the record slows down (0.4 … 1 s)
// — and only then the next one begins, from its beginning. A change that comes while the ring opens or closes waits
// for that to end. What was wanted meanwhile and is not any more is skipped.
//
// The record stops where the outer layer's petals stand on petals' places (every 45°: the flower's outline is then
// the logo's), not after a whole turn; so the flower at rest may stand turned by some eighths (`stands`), and what
// plays next plays on it as it stands. The ring puts it upright again: its dots are alike, and are taken for the
// petals that close upright.
//
// No DOM here: `advance(dt)` moves the clock, `pose` says where each layer is (app/Flower.jsx draws it).
import { LAYERS } from './flower-shapes.js'

const F = 1 / 60 // one frame
/** The flower at rest between one mode and another: long enough to be seen as the flower. */
const GAP = 8 * F
/** The longest a swell or a shake takes to be let down. */
const FADE = 18 * F
const C = 256 // the centre of the 512 box

/** CSS's cubic-bezier(x1, y1, x2, y2) as a function of 0 … 1 (SMIL's keySplines are the same curve). */
function bezier(x1, y1, x2, y2) {
  const cx = 3 * x1
  const bx = 3 * (x2 - x1) - cx
  const ax = 1 - cx - bx
  const cy = 3 * y1
  const by = 3 * (y2 - y1) - cy
  const ay = 1 - cy - by
  const xAt = (s) => ((ax * s + bx) * s + cx) * s
  return (x) => {
    if (x <= 0) return 0
    if (x >= 1) return 1
    let lo = 0
    let hi = 1
    let s = x
    for (let i = 0; i < 24; i++) {
      const e = xAt(s) - x
      if (Math.abs(e) < 1e-7) break
      if (e > 0) hi = s
      else lo = s
      const slope = (3 * ax * s + 2 * bx) * s + cx
      const next = slope > 1e-6 ? s - e / slope : -1
      s = next > lo && next < hi ? next : (lo + hi) / 2 // Newton while it stays inside, else halve
    }
    return ((ay * s + by) * s + cy) * s
  }
}

const SWELL = bezier(0.37, 0, 0.63, 1) // a breath in or out; a shake from one side to the other
const HIT = bezier(0.25, 0.6, 0.45, 1) // the alarm's heart: up at once
const FALL = bezier(0.22, 1, 0.36, 1) // and slowly down
const MORPH = bezier(0.65, 0, 0.35, 1) // a petal into a dot and back
const SPEED_UP = bezier(0.5, 0, 0.75, 0.5) // the turn while the ring opens: ends at the speed of going round
const SLOW_DOWN = bezier(0.25, 0.5, 0.5, 1) // the turn while it closes: begins at it
const SHRINK = bezier(0.55, 0, 0.7, 0.4) // the layers that go away, into the centre
const GROW = bezier(0.3, 0.6, 0.45, 1)

const smooth = (x) => (x <= 0 ? 0 : x >= 1 ? 1 : x * x * (3 - 2 * x))

// ---------------------------------------------------------------- the three that stay the flower

/** A layer grows to `to` in `up` frames from frame `at` and comes back in `down`: → its keys
 * [frame, degrees, scale, the easing to the next]. */
const swell = (at, to, up, down, rise, fall) => [
  [at, 0, 1, rise],
  [at + up, 0, to, fall],
  [at + up + down, 0, 1],
]

/** A layer shakes from frame `at`: `by` degrees one way, as far the other, and less each 4 frames till it stands. */
const shake = (at, by) => [
  [at, 0, 1, SWELL],
  ...[1, -1, 0.9, -0.8, 0.6, -0.4, 0.2, 0].map((k, i) => [at + 4 * (i + 1), by * k, 1, SWELL]),
]

/** A mode that never leaves the flower: `frames` long and again; `layers` (outer, middle, inner, heart): what each
 * does in it — strokes (swell, shake), each from rest to rest; between them the layer rests. */
function pulse(frames, layers) {
  // when anything moves: the strokes' spans, joined where they overlap
  const busy = []
  for (const [from, to] of layers
    .flatMap((strokes) => strokes.map((s) => [s[0][0], s[s.length - 1][0]]))
    .sort((a, b) => a[0] - b[0])) {
    const last = busy[busy.length - 1]
    if (last && from <= last[1]) last[1] = Math.max(last[1], to)
    else busy.push([from, to])
  }
  return {
    open: 0,
    close: 0,
    /** Seconds into it (not before `u`) by which the flower is at rest: at once if it rests, else let down. */
    leave(u) {
      const f = (u / F) % frames
      const span = busy.find(([from, to]) => f > from && f < to)
      return span ? u + Math.min(FADE, (span[1] - f) * F) : u
    },
    /** `letDown`: when it began to be let down (seconds into it), or null. */
    pose(phase, u, out, letDown) {
      const f = (u / F) % frames
      const keep = letDown == null ? 1 : 1 - smooth((u - letDown) / FADE)
      layers.forEach((strokes, i) => {
        const keys = strokes.find((s) => f >= s[0][0] && f < s[s.length - 1][0])
        if (!keys) return
        let k = 0
        while (f >= keys[k + 1][0]) k++
        const [f0, r0, s0, ease] = keys[k]
        const [f1, r1, s1] = keys[k + 1]
        const x = ease((f - f0) / (f1 - f0))
        out[i].rot += (r0 + (r1 - r0) * x) * keep
        out[i].scale = 1 + (s0 + (s1 - s0) * x - 1) * keep
      })
    },
  }
}

// ---------------------------------------------------------------- the ring

const numbers = (d) => d.match(/-?\d+\.?\d*/g).map(Number)

/** Each layer's petals as 50 numbers (M x y, then eight C of six); only the outer layer has them. */
export const PETALS = LAYERS.map((l) => l.petals.map(numbers))

const KAPPA = (4 / 3) * Math.tan(Math.PI / 16) // the handle of a 45° arc, for a radius of 1

/** A dot as its petal is drawn, so that one morphs into the other point by point: eight arcs clockwise from the
 * point that faces the centre. `at`: degrees (0 — to the right, −90 — up), `far`: how far from the centre. */
function dot(at, far, r) {
  const a0 = (at * Math.PI) / 180
  const x0 = C + far * Math.cos(a0)
  const y0 = C + far * Math.sin(a0)
  const out = []
  for (let i = 0; i <= 8; i++) {
    const a = a0 + Math.PI + (i * Math.PI) / 4
    const x = x0 + r * Math.cos(a)
    const y = y0 + r * Math.sin(a)
    if (i === 0) {
      out.push(x, y)
      continue
    }
    const b = a - Math.PI / 4 // where the arc began
    const k = KAPPA * r
    out.push(
      x0 + r * Math.cos(b) - k * Math.sin(b),
      y0 + r * Math.sin(b) + k * Math.cos(b),
      x + k * Math.sin(a),
      y - k * Math.cos(a),
      x,
      y,
    )
  }
  return out
}

const OPEN = 36 * F
const CLOSE = 36 * F
const EIGHTH = 0.15 // seconds: 45° at 300°/s — the dots stand on the petals' places again

/** The outer layer's petals turn into eight dots (r 26, 160 from the centre) while the flower makes a quarter turn
 * and the three inner layers go away into the centre (26 frames); the dots go round at 300°/s; closing is that
 * backwards, the inner layers coming back in the last 26 frames. The dots are alike, so standing on the petals'
 * places they are taken for the petals that end upright (`stands`: 0 from then on): the flower closes exactly as
 * the logo is drawn, however far it has turned and however it stood before. */
const ring = {
  open: OPEN,
  close: CLOSE,
  dots: Array.from({ length: 8 }, (_, i) => dot(-90 + i * 45, 160, 26)),
  leave: (u) => Math.ceil((u - 1e-9) / EIGHTH) * EIGHTH,
  stands: () => 0,
  pose(phase, u, [outer, ...inner]) {
    if (phase === 'open') {
      const turn = 90 * SPEED_UP(u / OPEN)
      const size = 1 - 0.999 * SHRINK(u / (26 * F))
      outer.rot += turn
      outer.p = MORPH(u / OPEN)
      for (const o of inner) {
        o.rot += turn
        o.scale = size
      }
    } else if (phase === 'run') {
      outer.rot += 90 + 300 * u
      outer.p = 1
      for (const o of inner) o.scale = 0.001
    } else {
      const turn = 90 * SLOW_DOWN(u / CLOSE) - 90
      const size = 0.001 + 0.999 * GROW((u - 10 * F) / (26 * F))
      outer.rot += turn
      outer.p = 1 - MORPH(u / CLOSE)
      for (const o of inner) {
        o.rot += turn
        o.scale = size
      }
    }
  },
}

// ---------------------------------------------------------------- the record

/** Seconds a turn takes. A real one takes 1.8 (33⅓ a minute): too busy for a glyph that turns for hours. */
const TURN = 5
const SPEED = 360 / TURN // degrees a second
const UP = 48 * F // getting up to speed
const PLACE = 45 // the outer layer's petals stand on petals' places every 45°

/** `u` seconds after it began to turn: degrees gone, and the speed — rising smoothly for UP, then even. */
function gone(u) {
  const x = Math.min(1, u / UP)
  return { by: SPEED * (u < UP ? UP * (x ** 3 - x ** 4 / 2) : u - UP / 2), speed: SPEED * smooth(x) }
}

/** Told to stop at `u`: it goes on `more` degrees, to the next place it has room to slow down to, for `time`
 * seconds — what slowing down evenly takes, within 0.4 … 1 s (so it is never turned back, and hardly ever hurried:
 * to a place 59° away it gets a tenth faster for a moment). */
function halt(u) {
  const { by, speed } = gone(u)
  let more = (PLACE - (by % PLACE)) % PLACE
  while (more < speed * 0.2) more += PLACE
  return { by, speed, more, time: more ? Math.min(1, Math.max(0.4, (2 * more) / speed)) : 0 }
}

const record = {
  open: 0,
  close: 0,
  leave: (u) => u + halt(u).time,
  /** Where the flower stands when it has stopped: so much further round. */
  stands: (was, letDown) => was + halt(letDown).by + halt(letDown).more,
  pose(phase, u, out, letDown) {
    let by
    if (letDown == null) by = gone(u).by
    else {
      // from the speed it had to a stand on the place: the cubic with those two slopes
      const h = halt(letDown)
      const x = h.time ? Math.min(1, (u - letDown) / h.time) : 1
      by = h.by + h.more * (3 * x * x - 2 * x ** 3) + h.speed * h.time * (x - 2 * x * x + x ** 3)
    }
    for (const o of out) o.rot += by
  },
}

/** Layers: outer, middle, inner, heart. */
export const MODES = {
  // from the heart outwards, 14 frames apart; in for 40 frames, out for 40; then rest till 192
  breathe: pulse(192, [
    [swell(42, 1.025, 40, 40, SWELL, SWELL)],
    [swell(28, 1.035, 40, 40, SWELL, SWELL)],
    [swell(14, 1.05, 40, 40, SWELL, SWELL)],
    [swell(0, 1.12, 40, 40, SWELL, SWELL)],
  ]),
  // the inner layer first and widest, the outer one 2 frames later and least; again 44 frames on; then rest till 96
  alarm: pulse(96, [
    [shake(2, 8.5), shake(46, 8.5)],
    [shake(1, 10), shake(45, 10)],
    [shake(0, 11.5), shake(44, 11.5)],
    [swell(0, 1.16, 4, 20, HIT, FALL), swell(44, 1.16, 4, 20, HIT, FALL)],
  ]),
  ring,
  record,
}

const turnOf = (x) => ((x % 360) + 360) % 360

/** One glyph's motion. `set(mode)`: what is wanted now (a key of MODES, or null — the flower at rest);
 * `advance(dt)`: `dt` seconds on; `pose`: per layer {rot: degrees round the centre, scale: about it, p: 0 — whole,
 * … 1 — dots} (the same objects every time); `dots(layer)`: what its petals turn into now, if anything; `busy()`:
 * whether anything moves or will. `jump(mode)`: there at once, standing — for those who asked for no motion. */
export function createFlower() {
  let now = 0
  let want = null
  let mode = null // what plays
  let phase = 'idle' // idle | open | run | close | gap (the flower, before the next)
  let since = 0 // when the phase began
  let until = Infinity // run: how long it goes on (seconds from `since`)
  let letDown = null // run, of those that stay the flower: since when it is being let down, or slowing down
  let stands = 0 // degrees the flower at rest is turned by: eighths of a turn (the record's doing)
  const pose = LAYERS.map(() => ({ rot: 0, scale: 1, p: 0 }))

  function begin(m) {
    mode = m
    phase = MODES[m].open ? 'open' : 'run'
    until = Infinity
    letDown = null
  }

  function rest() {
    mode = null
    phase = want ? 'gap' : 'idle'
  }

  /** What plays is over for what is wanted now: when, and how it goes there. */
  function leave() {
    const u = now - since
    until = MODES[mode].leave(u)
    if (!MODES[mode].dots) letDown = u
  }

  /** Phases that are over give way to the next ones, each beginning when the one before it ended. */
  function settle() {
    for (;;) {
      const u = now - since
      if (phase === 'open') {
        if (u < MODES[mode].open) return
        since += MODES[mode].open
        phase = 'run'
        if (want !== mode) until = 0
      } else if (phase === 'run') {
        if (u < until) return
        since += until
        if (MODES[mode].stands) stands = turnOf(Math.round(MODES[mode].stands(stands, letDown) / PLACE) * PLACE)
        if (MODES[mode].close) phase = 'close'
        else rest()
      } else if (phase === 'close') {
        if (u < MODES[mode].close) return
        since += MODES[mode].close
        rest()
      } else if (phase === 'gap') {
        if (u < GAP) return
        since += GAP
        if (want) begin(want)
        else phase = 'idle'
      } else return
    }
  }

  function read() {
    for (const o of pose) {
      o.rot = stands
      o.scale = 1
      o.p = 0
    }
    if (mode) MODES[mode].pose(phase, now - since, pose, letDown)
  }

  return {
    pose,
    dots: (i) => (i === 0 && mode && MODES[mode].dots) || null,
    busy: () => phase !== 'idle',
    /** For tests and the curious: what plays and where in it. */
    state: () => ({ mode, phase, want, at: now - since, stands }),
    set(m) {
      m = MODES[m] ? m : null
      if (m === want) return
      want = m
      if (phase === 'idle') {
        if (m) {
          since = now
          begin(m)
        }
      } else if (phase === 'run') {
        // wanted again before it left: the ring goes on; one being let down or slowing down ends, and begins anew
        if (m === mode) until = letDown == null ? Infinity : until
        else if (until === Infinity) leave()
      }
      settle()
      read()
    },
    advance(dt) {
      now += Math.max(0, dt)
      settle()
      read()
    },
    jump(m) {
      // only the ring has a picture of its own standing: the others, standing, are the flower
      mode = MODES[m]?.dots ? m : null
      want = mode
      since = now
      until = Infinity
      letDown = null
      phase = mode ? 'run' : 'idle'
      read()
    },
  }
}
