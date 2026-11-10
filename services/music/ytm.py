"""YouTube Music for the music service: search and audio.

search: InnerTube (what music.youtube.com itself calls), no login. «Поставь X»: if the top card is an artist — their
        songs shuffled (the RDAO… playlist behind the «Перемешать» button); a song — the song, then its radio;
        otherwise the first song found and its radio. Spoken transliterations work (айспик -> IC3PEAK, рамштайн ->
        Rammstein), the query just has to be free of command words — the model writes it. A misheard artist name is
        corrected from the songs list (_fix_artist).
audio:  yt-dlp gets the m4a stream URL (no JS player: format 140), ffmpeg decodes it to 48 kHz stereo s16le. The
        connection to YouTube breaks now and then for minutes (TLS «UNEXPECTED_EOF»): searches and URLs are
        retried, and a stream that ends early (no sound, or well before the track's end) is reopened from where it broke
        with a fresh URL — after two silent tries with a new yt-dlp session (a session sometimes gets only URLs that
        googlevideo answers 403). The caller sees one continuous stream. A live stream (24/7 radio) is not played: its
        audio is HLS only, which yt-dlp is told to skip — it is told at once, with no tries and no new session.
A search takes FIND_FOR s at most (the server waits 40); ffmpeg gives up a read that hangs for RW_TIMEOUT s (and is
reopened as a broken stream).
Idea and InnerTube details after Marker284/yandex-station-extended (contrib/station2-media, CC BY 4.0)."""

import difflib
import http.client
import json
import re
import ssl
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

API = 'https://music.youtube.com/youtubei/v1/'
CTX = {'client': {'clientName': 'WEB_REMIX', 'clientVersion': '1.20261026.01.00', 'hl': 'ru', 'gl': 'RU'}}
SONGS = 'EgWKAQIIAWoQEAMQBBAJEAoQBRAREBAQFQ=='  # search filter: songs only
SR, CH = 48000, 2
TRIES = 3  # search / stream URL / a broken stream: tries in all
GONE = re.compile(
    r'video is unavailable|Video unavailable|Private video|has been removed|not available in your country|'
    r'confirm your age|members-only|live stream recording is not available',
    re.IGNORECASE,
)
# yt-dlp: this track will not play, whatever we retry
LIVE = re.compile(r'live event will begin|Premieres in', re.IGNORECASE)  # yt-dlp: a live stream to come
LIVE_STATUS = ('is_live', 'is_upcoming', 'post_live')  # info's live_status: no plain audio file (yet)
FIND_FOR = 30.0  # s for a whole search: the server gives up on it after 40
RW_TIMEOUT = 10  # s ffmpeg waits for a read (a hung TCP) before it gives up
FLAKY = (urllib.error.URLError, ssl.SSLError, http.client.HTTPException, ConnectionError, TimeoutError)
log = print


class Live(Exception):
    """A live stream: no audio for it here (yt-dlp skips HLS). Said at once, with no tries and no new session."""


class Late(Exception):
    """The search took FIND_FOR s: given up (not retried)."""


_search = threading.local()  # .until: the search running in this thread must be done by then (time.monotonic())


def _left():
    until = getattr(_search, 'until', None)
    return None if until is None else until - time.monotonic()


def _retry(f, *args):
    """f(*args), again on a dropped connection (not on an HTTP error answer: that will not change)."""
    for k in range(TRIES):
        try:
            return f(*args)
        except urllib.error.HTTPError:
            raise
        except FLAKY as e:
            left = _left()
            if k == TRIES - 1 or (left is not None and left < 0.4 * (k + 1) + 1):
                raise
            log(f'YouTube: {str(e)[:70]}, once more')
            time.sleep(0.4 * (k + 1))


# ------------------------------------------------------------------ search
def _call(endpoint, body):
    return _retry(_call_once, endpoint, body)


def _call_once(endpoint, body):
    left = _left()
    if left is not None and left < 1:
        raise Late(f'поиск дольше {FIND_FOR:.0f} с')
    req = urllib.request.Request(
        API + endpoint + '?prettyPrint=false',
        json.dumps(dict(body, context=CTX)).encode(),
        {'Content-Type': 'application/json', 'User-Agent': 'Mozilla/5.0', 'Origin': 'https://music.youtube.com'},
    )
    with urllib.request.urlopen(req, timeout=6 if left is None else min(6, left)) as r:
        return json.load(r)


def _walk(o, key):
    if isinstance(o, dict):
        for k, v in o.items():
            if k == key:
                yield v
            else:
                yield from _walk(v, key)
    elif isinstance(o, list):
        for v in o:
            yield from _walk(v, key)


def _runs(r):
    return ''.join(x.get('text', '') for x in (r or {}).get('runs', []))


def _first(gen, default=None):
    return next(gen, default)


def _seconds(s):
    """«3:41» / «1:02:03» -> seconds, else None."""
    try:
        parts = [int(p) for p in str(s).split(':')]
    except ValueError:
        return None
    n = 0
    for p in parts:
        n = n * 60 + p
    return n if parts else None


def _panel(playlist_id, video_id=None):
    """The queue the web player itself builds for a playlist / radio: [(videoId, title, artist, seconds or None)]."""
    body = {'playlistId': playlist_id, 'isAudioOnly': True}
    if video_id:
        body['videoId'] = video_id
    return [
        (it['videoId'], _runs(it.get('title')), _runs(it.get('shortBylineText')), _seconds(_runs(it.get('lengthText'))))
        for it in _walk(_call('next', body), 'playlistPanelVideoRenderer')
        if 'videoId' in it
    ]


def _songs(query, n=8):
    """Songs-only search: [(videoId, title, artist)]."""
    out = []
    for it in _walk(_call('search', {'query': query, 'params': SONGS}), 'musicResponsiveListItemRenderer'):
        vid = _first(_walk(it.get('playlistItemData', {}), 'videoId')) or _first(_walk(it, 'videoId'))
        cols = [
            _runs(c['musicResponsiveListItemFlexColumnRenderer']['text'])
            for c in it.get('flexColumns', [])
            if 'musicResponsiveListItemFlexColumnRenderer' in c
        ]
        if vid and cols:
            artist = (cols[1].split(' • ')[0] if len(cols) > 1 else '').strip()
            out.append((vid, cols[0], artist))
        if len(out) >= n:
            break
    return out


def _sim(a, b):
    return difflib.SequenceMatcher(None, a.lower(), b.lower()).ratio()


def _artist_in(query, artist):
    """How well some run of the query's words matches the artist's name (0..1), and the query without that run."""
    words = query.split()
    best, rest = 0.0, query
    n = len(artist.split())
    for k in range(max(1, n - 1), n + 2):
        for i in range(len(words) - k + 1):
            sim = _sim(' '.join(words[i : i + k]), artist)
            if sim > best:
                best, rest = sim, ' '.join(words[:i] + words[i + k :])
    return best, rest


def _title_ok(rest, title):
    """The query's title part names this song: «каждый раз» ~ «Каждый раз (original)», «песня каждый раз», a word
    misheard."""
    t = re.sub(r'\s*[(\[].*?[)\]]', '', title).strip().lower()
    r = rest.strip().lower()
    if not t or not r:
        return False
    return r in t or (t in r if len(t) >= 3 else t == r) or _sim(r, t) >= 0.6


def _fix_artist(query):
    """Speech recognition garbles foreign names (an artist heard as a similar-sounding name), and an exact title match
    then wins the top card (the same title by someone else). The songs list usually still names the right artist: if
    one of them is close to a part of the query (but not exact), ask again with the right name and take that artist's
    song — the one the rest of the query names, when it names one. -> (videoId, title, artist) or None."""
    songs = _songs(query)
    for _, _, artist in songs[:5]:
        if not artist:
            continue
        sim, rest = _artist_in(query, artist)
        if 0.72 <= sim < 0.999 or (sim >= 0.999 and rest):
            again = [s for s in _songs(f'{artist} {rest}'.strip()) if s[2].lower() == artist.lower()]
            if rest:  # a title asked for: that song or none (not the artist's best one: the title may be no song of
                # theirs here but a video someone uploaded, which the plain search finds)
                again = [s for s in again if _title_ok(rest, s[1])]
            if again:
                return again[0]
    return None


def find(query):
    """-> (what was found, as a phrase for the assistant to confirm, [(videoId, title, artist, seconds or None)]).
    Raises Late after FIND_FOR s: a search makes up to seven calls, each retried with a 6-s timeout — with YouTube
    slow it could go on for minutes, long after the server gave up at 40 s."""
    _search.until = time.monotonic() + FIND_FOR
    try:
        return _find(query)
    finally:
        _search.until = None


def _find(query):
    fixed = _fix_artist(query)
    if fixed:  # a named artist (possibly misheard) and a song of theirs
        q = _panel('RDAMVM' + fixed[0], fixed[0])
        if q:
            return f'{q[0][1]} — {q[0][2]}', q
    d = _call('search', {'query': query})
    card = _first(_walk(d, 'musicCardShelfRenderer'))
    if card:
        title = _runs(card.get('title'))
        bid = _first(_walk(card.get('title', {}), 'browseId'), '')
        if bid.startswith('UC'):  # an artist: their songs shuffled
            pid = _first(
                x.get('playlistId')
                for x in _walk(card, 'watchPlaylistEndpoint')
                if x.get('playlistId', '').startswith('RDAO')
            )
            if pid:
                q = _panel(pid)
                if q:
                    return f'{title}, песни вперемешку', q
        vid = _first(x.get('videoId') for x in _walk(card, 'watchEndpoint') if x.get('videoId'))
        if vid:  # a song (or video): it, then its radio
            q = _panel('RDAMVM' + vid, vid)
            if q:
                return f'{q[0][1]} — {q[0][2]}', q
    for vid, _, _ in _songs(query, 1):
        q = _panel('RDAMVM' + vid, vid)
        if q:
            return f'{q[0][1]} — {q[0][2]}', q
    return None, []


# ------------------------------------------------------------------ audio
class Resolver:
    def __init__(self):
        self.lock = threading.Lock()
        self.reset()

    def reset(self):
        """A new yt-dlp session (a new visitor). Now and then a session gets only stream URLs that googlevideo answers
        403, every track (they lack «pcm2»; a new session gets good ones at once): the whole queue would skip by in a
        minute."""
        import yt_dlp

        self.ydl = yt_dlp.YoutubeDL(
            {
                'quiet': True,
                'no_warnings': True,
                'format': '140/bestaudio[ext=m4a]',
                'js_runtimes': {},
                'extractor_retries': TRIES - 1,
                'socket_timeout': 8,
                'extractor_args': {
                    'youtube': {'skip': ['translated_subs', 'hls', 'dash'], 'player_skip': ['configs', 'js']}
                },
            }
        )
        self.cache = {}

    def url(self, vid, fresh=False):
        """-> (stream URL, headers, duration s or None). fresh: not the cached URL (its stream just broke). Raises Live
        for a live stream with no audio file (asked before the format is chosen: that failed as «No video formats
        found!», like a refused session)."""
        with self.lock:
            now = time.time()
            hit = self.cache.get(vid)
            if hit and hit[2] > now + 600 and not fresh:
                return hit[0], hit[1], hit[3]
            info = self.ydl.extract_info('https://music.youtube.com/watch?v=' + vid, download=False, process=False)
            if (info.get('is_live') or info.get('live_status') in LIVE_STATUS) and not info.get('formats'):
                raise Live(f'прямой эфир ({info.get("live_status")})')
            info = self.ydl.process_ie_result(info, download=False)
            u = info['url']
            exp = int(urllib.parse.parse_qs(urllib.parse.urlparse(u).query).get('expire', ['0'])[0] or 0) or now + 3600
            # the expired out, or every track's link since the start is kept
            self.cache = {k: v for k, v in self.cache.items() if v[2] > now}
            self.cache[vid] = (u, info.get('http_headers', {}), exp, info.get('duration'))
            return u, info.get('http_headers', {}), info.get('duration')


class Stream:
    """One track's audio from `start` s: 48 kHz stereo s16le, read() by pieces; reopened where it broke. close() stops it
    (and its ffmpeg) at any time. .duration: the track's length (s) once known; .played: the position up to which audio
    was given out — not what sounded: the listener is behind it by the socket's buffers (12…19 s).
    .error: why there is no audio, for the caller (None: no reason better than «no sound»)."""

    BYTES_S = SR * CH * 2

    def __init__(self, resolver, vid, start=0.0):
        self.res, self.vid, self.start = resolver, vid, float(start)
        self.ff, self.closed, self.duration = None, False, None
        self.played = self.start  # position (s) of the next byte
        self.tries, self.from_ = 0, self.start
        self.ended_how = None  # 'end' | 'broken' | 'live' (a live stream: not played) | 'cut' (closed by the caller)
        self.error = None

    def _open(self):
        url, headers, dur = self.res.url(self.vid, fresh=self.tries > 0)
        self.duration = dur
        hdr = ''.join(f'{k}: {v}\r\n' for k, v in headers.items())
        self.ff = subprocess.Popen(
            [
                'ffmpeg',
                '-v',
                'error',
                '-reconnect',
                '1',
                '-reconnect_streamed',
                '1',
                '-reconnect_delay_max',
                '5',
                '-rw_timeout',
                str(RW_TIMEOUT * 1_000_000),  # a hung connection: given up, reopened
                '-headers',
                hdr,
                '-ss',
                f'{self.played:.2f}',
                '-i',
                url,
                '-f',
                's16le',
                '-ac',
                str(CH),
                '-ar',
                str(SR),
                '-',
            ],
            stdout=subprocess.PIPE,
            stdin=subprocess.DEVNULL,
        )
        self.from_ = self.played

    def read(self, n=65536):
        """-> PCM bytes (a whole number of frames), b'' at the end."""
        while not self.closed:
            if self.ff is None:
                try:
                    self._open()
                except Exception as e:  # noqa: BLE001 — yt-dlp / ffmpeg failed to start: counts as a broken try
                    log(f'the stream does not open ({str(e)[:80]})')
                    if isinstance(e, Live) or LIVE.search(str(e)):  # said at once: no tries, no new session for all
                        self.ended_how, self.error = 'live', 'прямой эфир — не играю'
                        return b''
                    if GONE.search(str(e)):  # removed, private, blocked: no new session or URL will change that
                        self.ended_how = 'broken'
                        return b''
                    if not self._broken(0.0):
                        return b''
                    continue
            raw = self.ff.stdout.read(n - n % 4)
            if raw:
                raw = raw[: len(raw) - len(raw) % 4]
                self.played += len(raw) / self.BYTES_S
                return raw
            self.ff.wait()
            self.ff = None
            # the stream ended, but did the song? (no sound at all, or well before its end: the connection broke). At
            # its end (its last 5 s, as the server counts) it did, sound or not: a start right at the end gives none,
            # and is no refused session to answer with tries and a new session. Its length unknown: some sound, then
            # the end.
            got = self.played - self.from_
            if (self.played >= self.duration - 5) if self.duration else got >= 1.0:
                self.ended_how = 'end'
                return b''
            log(
                f'the stream broke at {self.played:.0f} s'
                + (f' of {self.duration:.0f}' if self.duration else '')
                + ', once more'
            )
            if not self._broken(got):
                return b''
        return b''

    def _broken(self, got):
        """A try failed (got: s of audio it gave). -> try again?"""
        if self.tries >= 1 and got < 1.0:  # a fresh URL gave no sound either: the session's URLs are refused
            with self.res.lock:
                self.res.reset()
            log('a new YouTube session')
        self.tries += 1
        if self.tries >= TRIES:
            self.ended_how = 'broken'
            return False
        time.sleep(0.5 * self.tries)
        return not self.closed

    def close(self):
        self.closed = True
        if self.ended_how is None:
            self.ended_how = 'cut'
        if self.ff:
            try:
                self.ff.kill()
                self.ff.wait(timeout=3)
            except (OSError, subprocess.TimeoutExpired):
                pass
            self.ff = None
