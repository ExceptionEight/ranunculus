"""Text put into words for Silero TTS, which silently skips digits and Latin letters.

Numbers become words in the case the context asks for (the preposition before, «год» or a month after), Latin words
are transliterated, symbols (%, °, +, -) are spoken. Rules and pymorphy3, no model."""

import re

import pymorphy3
from num2words import num2words

M = pymorphy3.MorphAnalyzer()
MONTHS = (
    'января',
    'февраля',
    'марта',
    'апреля',
    'мая',
    'июня',
    'июля',
    'августа',
    'сентября',
    'октября',
    'ноября',
    'декабря',
)
YEAR = {'год': 'nomn', 'года': 'gent', 'году': 'loct', 'годом': 'ablt', 'годов': 'gent', 'годах': 'loct'}
PREP_CASE = {
    'до': 'gent',
    'от': 'gent',
    'с': 'gent',
    'из': 'gent',
    'около': 'gent',
    'без': 'gent',
    'для': 'gent',
    'после': 'gent',
    'у': 'gent',
    'к': 'datv',
    'по': 'datv',
    'о': 'loct',
    'об': 'loct',
    'при': 'loct',
    'на': 'accs',
    'через': 'accs',
    'за': 'accs',
    'про': 'accs',
}
# «один/два» agree in gender with the noun after them: «две минуты», «одна секунда», «одну минуту», «одно яблоко»
GENDER_FIX = {
    'femn': {'один': 'одна', 'два': 'две', 'одного': 'одной', 'одному': 'одной', 'одним': 'одной', 'одном': 'одной'},
    'neut': {'один': 'одно'},
}
GENDER_FIX_ACCS = {'femn': {'один': 'одну', 'два': 'две'}, 'neut': {'один': 'одно'}}
VERB_GENT = ('родился', 'родилась', 'родились', 'умер', 'умерла', 'умерли', 'начиная', 'вплоть')
WORDS = {
    'youtube': 'ютуб',
    'iphone': 'айфон',
    'ipad': 'айпад',
    'macbook': 'макбук',
    'google': 'гугл',
    'windows': 'виндоус',
    'aliexpress': 'алиэкспресс',
    'ok': 'окей',
    'wi-fi': 'вай-фай',
    'wifi': 'вай-фай',
    'pyrokinesis': 'пирокинезис',
    'chanel': 'шанель',
    'gemma': 'джемма',
    'gemini': 'джемини',
}
DIGRAPHS = [
    ('sch', 'ш'),
    ('tch', 'ч'),
    ('sh', 'ш'),
    ('ch', 'ч'),
    ('th', 'т'),
    ('ph', 'ф'),
    ('kh', 'х'),
    ('zh', 'ж'),
    ('ts', 'ц'),
    ('oo', 'у'),
    ('ee', 'и'),
    ('ea', 'и'),
    ('ou', 'ау'),
    ('qu', 'кв'),
    ('ck', 'к'),
    ('ya', 'я'),
    ('yu', 'ю'),
    ('yo', 'йо'),
    ('ye', 'е'),
]
LETTERS = dict(
    zip(
        'abcdefghijklmnopqrstuvwxyz',
        [
            'а',
            'б',
            'к',
            'д',
            'е',
            'ф',
            'г',
            'х',
            'и',
            'дж',
            'к',
            'л',
            'м',
            'н',
            'о',
            'п',
            'к',
            'р',
            'с',
            'т',
            'у',
            'в',
            'в',
            'кс',
            'и',
            'з',
        ],
    )
)


def inflect_words(words, case, gender=None):
    out = []
    ws = words.split()
    for i, w in enumerate(ws):
        p = M.parse(w)[0]
        if w in BIG and i > 0 and case != 'nomn':  # «двух тысяч», not «двух тысячи»
            f = p.inflect({case, 'sing'} if ws[i - 1] in ('один', 'одна') else {case, 'plur'})
        else:
            tags = {case} | ({gender} if gender and 'ADJF' in p.tag else set())
            f = p.inflect(tags) or p.inflect({case})
        out.append(f.word if f else w)
    return ' '.join(out)


BIG = ('тысяча', 'тысячи', 'тысяч', 'миллион', 'миллиона', 'миллионов', 'миллиард', 'миллиарда', 'миллиардов')


def ordinal(n, case='nomn', gender='masc'):
    words = num2words(n, lang='ru', to='ordinal').split()
    last = M.parse(words[-1])[0].inflect({case, gender} if gender != 'plur' else {case, 'plur'})
    words[-1] = last.word if last else words[-1]
    return ' '.join(words)


def cardinal(n, case='nomn'):
    w = num2words(n, lang='ru')
    return w if case == 'nomn' else inflect_words(w, case)


def translit(word):
    lw = word.lower()
    if lw in WORDS:
        return WORDS[lw]
    out, i = '', 0
    while i < len(lw):
        for a, b in DIGRAPHS:
            if lw.startswith(a, i):
                out += b
                i += len(a)
                break
        else:
            c = lw[i]
            if c == 'c' and i + 1 < len(lw) and lw[i + 1] in 'eiy':
                out += 'с'
            elif c == 'y' and i > 0 and lw[i - 1] in 'aeiou':
                out += 'й'
            elif c == 'e' and i == len(lw) - 1 and i > 1:
                pass  # silent final e: "Pyrokinesis" is fine, "rose" -> "роз"
            else:
                out += LETTERS.get(c, c)
            i += 1
    return out


def _number(m, text):
    raw = m.group(0)
    before = [re.sub(r'[^а-яё]', '', w.lower()) for w in text[: m.start()].split()]
    while before and before[-1] in ('плюс', 'минус', ''):  # «до плюс 1 градуса»: the preposition is before the sign
        before.pop()
    prev = before[-1] if before else ''
    after = text[m.end() :].split()
    nxt = re.sub(r'[^а-яё]', '', after[0].lower()) if after else ''
    if ':' in raw:  # time 23:30
        h, mi = raw.split(':')
        return cardinal(int(h)) + (
            ' ноль ' + cardinal(int(mi))
            if mi.startswith('0') and mi != '00'
            else ''
            if mi == '00'
            else ' ' + cardinal(int(mi))
        )
    if ',' in raw or '.' in raw:
        a, b = re.split(r'[.,]', raw)
        if b == '5' and len(b) == 1:
            return cardinal(int(a)) + ' с половиной'
        return num2words(float(raw.replace(',', '.')), lang='ru')
    n = int(raw.replace(' ', '').replace(' ', ''))
    if nxt in YEAR:
        case = YEAR[nxt]
        if case == 'loct' and prev == 'к':
            case = 'datv'
        return ordinal(n, case, 'masc')
    if nxt in MONTHS:
        case = 'gent' if prev in VERB_GENT or PREP_CASE.get(prev) == 'gent' else 'datv' if prev == 'к' else 'nomn'
        return ordinal(n, case, 'neut')
    case = PREP_CASE.get(prev, 'nomn')
    words = cardinal(n, 'nomn' if case == 'accs' else case).split()
    noun = M.parse(nxt)[0] if nxt else None
    gender = next((g for g in ('femn', 'neut') if noun and 'NOUN' in noun.tag and g in noun.tag), None)
    if gender:
        fix = (GENDER_FIX_ACCS if case == 'accs' else GENDER_FIX)[gender]
        words[-1] = fix.get(words[-1], words[-1])
    return ' '.join(words)


def agree(noun, digits):
    return M.parse(noun)[0].make_agree_with_number(int(digits)).word


def normalize(text):
    t = text
    t = re.sub(r'(\d+)\s*%', lambda m: m.group(1) + ' ' + agree('процент', m.group(1)), t)
    t = re.sub(r'(\d+)\s*°\s*[CС]?', lambda m: m.group(1) + ' ' + agree('градус', m.group(1)), t)
    t = re.sub(r'\b([48])\s*[KК]\b', r'\1 кей', t)  # 4K / 8K video
    t = t.replace('№', 'номер ')
    t = re.sub(r'(?<![\w])\+(\d)', r'плюс \1', t)
    t = re.sub(r'(?<![\w])[-−–](\d)', r'минус \1', t)
    # numbers one by one, left to right, so each sees the already-normalized words before it
    while True:
        m = re.search(r'\d{1,3}(?:[  ]\d{3})+(?!\d)|\d+:\d{2}|\d+[.,]\d+|\d+', t)
        if not m:
            break
        t = t[: m.start()] + _number(m, t) + t[m.end() :]
    t = re.sub(r'[A-Za-z][A-Za-z\'-]*', lambda m: translit(m.group(0)), t)
    t = t.replace('«', '').replace('»', '').replace('…', '.')
    t = re.sub(r'\s*[—–]\s*', ', ', t)
    t = re.sub(r'\s+([,.!?])', r'\1', t)
    return re.sub(r'\s+', ' ', t).strip()
