// The home's time zone (the settings' home.zone): every time and day on the pages is in it, wherever the browser is.
// Until the settings come: the browser's own.
let zone = Intl.DateTimeFormat().resolvedOptions().timeZone

export const tz = () => zone

export function setZone(z) {
  if (z) zone = z
}
