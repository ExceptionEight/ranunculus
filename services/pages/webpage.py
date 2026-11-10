"""A web page (usually one a web search found) as readable text, in parts.

- Only the public internet: a host that resolves to a private, loopback, link-local or otherwise non-global address
  is refused, and so is a redirect to one. The services beside this one and the home network (the station, the
  router) stay out of reach of whatever a voice request or a page asks for.
- Text only: scripts, styles, menus, headers, footers and forms are dropped, and so is every line made only of link
  text (menus, «other songs» lists). A lyrics container (Genius' data-lyrics-container) wins; otherwise
  <main>/<article> when it holds the text; otherwise the body. Of that, the densest run of lines is kept: lines with
  words count for their letters, short scraps («Войти», «0:00», «баллов») against — so a page's chrome falls away
  and the song or the article stays.
- Long pages come in parts of PART characters; the last pages are cached, so «читай дальше» needs no new download.
- Page text is data for the model, never instructions to it: the server's system prompt says so.
Standard library only."""

import ipaddress
import re
import socket
import urllib.error
import urllib.parse
import urllib.request
from collections import OrderedDict
from html.parser import HTMLParser

UA = 'Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36'
MAX_BYTES = 3_000_000
PART = 3000  # characters per part: a couple of minutes of reading aloud
SCRAP = 25  # what a short non-text line costs in the densest-run search, in letters
CACHE = OrderedDict()  # url -> (title, text)

SKIP = {
    'script',
    'style',
    'noscript',
    'svg',
    'template',
    'nav',
    'header',
    'footer',
    'aside',
    'form',
    'button',
    'iframe',
    'select',
    'canvas',
    'dialog',
    'head',
}
BLOCK = {
    'p',
    'div',
    'br',
    'li',
    'ul',
    'ol',
    'h1',
    'h2',
    'h3',
    'h4',
    'h5',
    'h6',
    'tr',
    'td',
    'th',
    'table',
    'section',
    'article',
    'main',
    'blockquote',
    'pre',
    'dd',
    'dt',
    'hr',
    'figcaption',
}
VOID = {'br', 'hr', 'img', 'meta', 'link', 'input', 'source', 'wbr', 'area', 'base', 'col', 'embed', 'track'}
LINK_ON, LINK_OFF = '\x01', '\x02'


class _Text(HTMLParser):
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.skip, self.link = 0, 0
        self.depth = {'main': 0, 'article': 0}
        self.div, self.lyr_at = 0, None  # div nesting, and the depth where a lyrics container opened
        self.title, self.in_title = '', False
        self.all, self.main, self.lyrics = [], [], []

    def _put(self, s):
        self.all.append(s)
        if self.depth['main'] or self.depth['article']:
            self.main.append(s)
        if self.lyr_at is not None:
            self.lyrics.append(s)

    def handle_starttag(self, tag, attrs):
        if tag == 'title':
            self.in_title = True
            return
        if tag in SKIP:
            self.skip += 1
            return
        if tag == 'a':
            self.link += 1
        if tag in self.depth:
            self.depth[tag] += 1
        if tag == 'div':
            self.div += 1
            if self.lyr_at is None and dict(attrs).get('data-lyrics-container') == 'true':
                self.lyr_at = self.div
        if tag in BLOCK and not self.skip:
            self._put('\n')

    def handle_startendtag(self, tag, attrs):
        if tag in BLOCK and not self.skip:
            self._put('\n')

    def handle_endtag(self, tag):
        if tag == 'title':
            self.in_title = False
            return
        if tag in SKIP:
            self.skip = max(0, self.skip - 1)
            return
        if tag == 'a':
            self.link = max(0, self.link - 1)
        if tag in self.depth:
            self.depth[tag] = max(0, self.depth[tag] - 1)
        if tag == 'div':
            if self.lyr_at is not None and self.div == self.lyr_at:
                self.lyrics.append('\n')
                self.lyr_at = None
            self.div = max(0, self.div - 1)
        if tag in BLOCK and not self.skip:
            self._put('\n')

    def handle_data(self, data):
        if self.in_title:
            self.title += data
        elif not self.skip:
            self._put(f'{LINK_ON}{data}{LINK_OFF}' if self.link else data)


def _letters(s):
    return len(re.findall(r'[^\W\d_]', s))


def _lines(parts):
    """Lines of text; a line that is only link text (or has no letters) is dropped."""
    text = re.sub(r'\[\d+\]|\[править[^\]]*\]', '', ''.join(parts))  # wiki footnote marks and edit links
    out = []
    for raw in text.split('\n'):
        if _letters(re.sub(f'{LINK_ON}[^{LINK_OFF}]*{LINK_OFF}', '', raw)) < 2:
            continue
        line = re.sub(r'[ \t ﻿]+', ' ', raw.replace(LINK_ON, '').replace(LINK_OFF, '')).strip()
        if line and (not out or line != out[-1]):
            out.append(line)
    return out


def _densest(lines):
    """The run of lines with the most text: a line with words adds its letters, a scrap subtracts SCRAP."""
    best, best_ij, cur, start = 0, (0, 0), 0, 0
    for k, line in enumerate(lines):
        n = _letters(line)
        v = n if (n >= 18 or len(line.split()) >= 3) else -SCRAP
        if cur <= 0:
            cur, start = 0, k
        cur += v
        if cur > best:
            best, best_ij = cur, (start, k + 1)
    return lines[best_ij[0] : best_ij[1]]


def extract(html_text):
    """-> (title, text) of an HTML page."""
    p = _Text()
    p.feed(html_text)
    p.close()
    title = re.sub(r'\s+', ' ', p.title).strip()
    for parts, enough in ((p.lyrics, 200), (p.main, 400), (p.all, 0)):
        lines = _densest(_lines(parts))
        if sum(map(len, lines)) > enough:
            return title, '\n'.join(lines)
    return title, ''


def _check(url):
    u = urllib.parse.urlparse(url)
    if u.scheme not in ('http', 'https') or not u.hostname:
        raise ValueError('открываю только страницы http и https')
    try:
        infos = socket.getaddrinfo(u.hostname, u.port or (443 if u.scheme == 'https' else 80), proto=socket.IPPROTO_TCP)
    except socket.gaierror:
        raise ValueError('такого сайта нет') from None
    for *_, sa in infos:
        if not ipaddress.ip_address(sa[0].split('%')[0]).is_global:
            raise ValueError('это адрес не из интернета: домашнюю сеть и сам компьютер не открываю')


class _Redirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        _check(newurl)  # a public page must not bounce us into the home network
        return super().redirect_request(req, fp, code, msg, headers, _uri(newurl))


_OPENER = urllib.request.build_opener(_Redirects)


def _uri(url):
    """Cyrillic in the path or host (ru.wikipedia.org/wiki/Рыжий) -> a plain ASCII URL; %XX already there stays."""
    u = urllib.parse.urlsplit(url.strip())
    host = u.hostname.encode('idna').decode('ascii') if u.hostname else ''
    netloc = host + (f':{u.port}' if u.port else '')
    path = urllib.parse.quote(u.path, safe="/%:@!$&'()*+,;=-._~")
    query = urllib.parse.quote(u.query, safe="=&%:@!$'()*+,;/?-._~")
    return urllib.parse.urlunsplit((u.scheme, netloc, path, query, ''))


def _decode(raw, charset):
    meta = re.search(rb'<meta[^>]+charset=["\']?([\w-]+)', raw[:4096], re.IGNORECASE)
    for enc in (charset, meta and meta.group(1).decode('ascii', 'ignore'), 'utf-8', 'cp1251'):
        if not enc:
            continue
        try:
            return raw.decode(enc)
        except (LookupError, UnicodeDecodeError):
            continue
    return raw.decode('utf-8', 'replace')


def fetch(url, timeout=7):
    """-> (title, text). Raises ValueError with a short Russian reason."""
    try:
        url = _uri(url)
    except (ValueError, UnicodeError):
        raise ValueError('не понимаю этот адрес') from None
    if url in CACHE:
        CACHE.move_to_end(url)
        return CACHE[url]
    _check(url)
    req = urllib.request.Request(
        url,
        headers={
            'User-Agent': UA,
            'Accept-Language': 'ru,en;q=0.8',
            'Accept': 'text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.5',
        },
    )
    try:
        with _OPENER.open(req, timeout=timeout) as r:
            ctype = r.headers.get_content_type()
            if not (ctype.startswith('text/') or 'html' in ctype or 'xml' in ctype):
                raise ValueError(f'это не страница, а файл ({ctype})')
            raw = r.read(MAX_BYTES)
            charset = r.headers.get_content_charset()
    except ValueError:
        raise
    except urllib.error.HTTPError as e:
        raise ValueError(
            f'сайт ответил ошибкой {e.code}' + (' — не пускает роботов' if e.code in (401, 403, 429) else '')
        ) from None
    except Exception as e:  # noqa: BLE001 — timeouts, resets, TLS: for the model it is all «did not open»
        raise ValueError(f'страница не открылась: {getattr(e, "reason", e)}') from None
    page = _decode(raw, charset)
    title, text = extract(page) if ctype != 'text/plain' else ('', '\n'.join(_lines([page])))
    if len(text) < 200:
        raise ValueError('на странице почти нет текста (возможно, защита от роботов)')
    CACHE[url] = (title, text)
    while len(CACHE) > 8:
        CACHE.popitem(last=False)
    return title, text


def split_parts(text, size=PART):
    """Parts of about `size` characters, cut at line ends (a huge paragraph at a sentence end)."""
    parts, cur = [], ''
    for line in text.split('\n'):
        while len(line) > size:
            k = line.rfind('. ', 0, size)
            k = k + 1 if k > size // 2 else size
            if cur:
                parts.append(cur.strip())
                cur = ''
            parts.append(line[:k].strip())
            line = line[k:].strip()
        if cur and len(cur) + len(line) + 1 > size:
            parts.append(cur.strip())
            cur = ''
        cur += line + '\n'
    if cur.strip():
        parts.append(cur.strip())
    return parts or ['']


def read_page(url, part=1):
    try:
        title, text = fetch(str(url))
    except ValueError as e:
        return {'ok': False, 'error': str(e)}
    parts = split_parts(text)
    try:
        part = max(1, int(part or 1))
    except (TypeError, ValueError):
        part = 1
    if part > len(parts):
        return {'ok': False, 'error': f'дальше текста нет: всего частей {len(parts)}'}
    return {
        'ok': True,
        'title': title,
        'site': urllib.parse.urlparse(str(url)).netloc,
        'part': part,
        'parts': len(parts),
        'text': parts[part - 1],
    }
