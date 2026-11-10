//! The calculator: + - * / // % ** and brackets over whole and real numbers, by Python's rules (floor division, the
//! sign of a remainder, whole numbers for as long as they fit) — models write expressions that way. Anything else —
//! a name, a call, a comparison — is refused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Num {
    I(i128),
    F(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
}

const SYNTAX: &str = "syntax error";
const UNSUPPORTED: &str = "unsupported expression";
const ZERO: &str = "division by zero";

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    N(Num),
    Op(&'static str),
    Open,
    Close,
    Other, // a name, a complex number, another operator: read, and refused as unsupported
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            if ch == '0' && matches!(c.get(i + 1), Some('x' | 'X' | 'o' | 'O' | 'b' | 'B')) {
                let radix = match c[i + 1] {
                    'x' | 'X' => 16,
                    'o' | 'O' => 8,
                    _ => 2,
                };
                i += 2;
                let ds = i;
                while i < c.len() && (c[i].is_digit(radix) || c[i] == '_') {
                    i += 1;
                }
                let digits: String = c[ds..i].iter().filter(|x| **x != '_').collect();
                out.push(Tok::N(Num::I(i128::from_str_radix(&digits, radix).map_err(|_| SYNTAX.to_string())?)));
                continue;
            }
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
                i += 1;
            }
            let mut float = false;
            if i < c.len() && c[i] == '.' {
                float = true;
                i += 1;
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
                    i += 1;
                }
            }
            if i < c.len() && (c[i] == 'e' || c[i] == 'E') {
                let save = i;
                i += 1;
                if i < c.len() && (c[i] == '+' || c[i] == '-') {
                    i += 1;
                }
                if i < c.len() && c[i].is_ascii_digit() {
                    float = true;
                    while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
                        i += 1;
                    }
                } else {
                    i = save;
                }
            }
            let lit: String = c[st..i].iter().filter(|x| **x != '_').collect();
            if i < c.len() && (c[i] == 'j' || c[i] == 'J') {
                i += 1;
                out.push(Tok::Other); // a complex number
                continue;
            }
            if i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                return Err(SYNTAX.into()); // «2x»
            }
            if float {
                out.push(Tok::N(Num::F(lit.parse().map_err(|_| SYNTAX.to_string())?)));
            } else {
                if lit.len() > 1 && lit.starts_with('0') && lit.chars().any(|d| d != '0') {
                    return Err(SYNTAX.into()); // «0123»
                }
                out.push(match lit.parse::<i128>() {
                    Ok(v) => Tok::N(Num::I(v)),
                    Err(_) => Tok::N(Num::F(lit.parse().map_err(|_| SYNTAX.to_string())?)),
                });
            }
            continue;
        }
        if ch.is_alphabetic() || ch == '_' {
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push(Tok::Other);
            continue;
        }
        let two: String = c[i..(i + 2).min(c.len())].iter().collect();
        let op: Option<&'static str> = match two.as_str() {
            "**" => Some("**"),
            "//" => Some("//"),
            _ => None,
        };
        if let Some(op) = op {
            out.push(Tok::Op(op));
            i += 2;
            continue;
        }
        out.push(match ch {
            '+' => Tok::Op("+"),
            '-' => Tok::Op("-"),
            '*' => Tok::Op("*"),
            '/' => Tok::Op("/"),
            '%' => Tok::Op("%"),
            '(' => Tok::Open,
            ')' => Tok::Close,
            '~' | '&' | '|' | '^' | '<' | '>' | '=' | '!' | '@' => Tok::Other,
            _ => return Err(SYNTAX.into()),
        });
        i += 1;
    }
    Ok(out)
}

/// The expression is evaluated as it is parsed; something unsupported in it is remembered and reported once the
/// whole expression has parsed: a syntax error comes first.
struct P {
    t: Vec<Tok>,
    i: usize,
    unsupported: bool,
}

type R = Result<Option<Num>, String>; // None: an unsupported node inside

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }

    fn expr(&mut self) -> R {
        let mut a = self.term()?;
        while let Some(Tok::Op(op @ ("+" | "-"))) = self.peek().cloned() {
            self.i += 1;
            let b = self.term()?;
            a = bin(op, a, b)?;
        }
        Ok(a)
    }

    fn term(&mut self) -> R {
        let mut a = self.factor()?;
        while let Some(Tok::Op(op @ ("*" | "/" | "//" | "%"))) = self.peek().cloned() {
            self.i += 1;
            let b = self.factor()?;
            a = bin(op, a, b)?;
        }
        Ok(a)
    }

    fn factor(&mut self) -> R {
        match self.peek().cloned() {
            Some(Tok::Op("-")) => {
                self.i += 1;
                Ok(self.factor()?.map(|v| match v {
                    Num::I(i) => Num::I(-i),
                    Num::F(f) => Num::F(-f),
                }))
            }
            Some(Tok::Op("+")) => {
                self.i += 1;
                self.factor()
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> R {
        let base = self.atom()?;
        if let Some(Tok::Op("**")) = self.peek() {
            self.i += 1;
            let exp = self.factor()?;
            return bin("**", base, exp);
        }
        Ok(base)
    }

    fn atom(&mut self) -> R {
        let r = match self.peek().cloned() {
            Some(Tok::N(n)) => {
                self.i += 1;
                Some(n)
            }
            Some(Tok::Open) => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(&Tok::Close) {
                    return Err(SYNTAX.into());
                }
                self.i += 1;
                v
            }
            Some(Tok::Other) => {
                self.i += 1;
                self.unsupported = true;
                None
            }
            _ => return Err(SYNTAX.into()),
        };
        // a call after it («sqrt(2)», «2(3)»): read to its end, and refused as unsupported
        if self.peek() == Some(&Tok::Open) {
            let mut depth = 0;
            while let Some(t) = self.peek() {
                match t {
                    Tok::Open => depth += 1,
                    Tok::Close => {
                        depth -= 1;
                        if depth == 0 {
                            self.i += 1;
                            break;
                        }
                    }
                    _ => {}
                }
                self.i += 1;
            }
            if depth != 0 {
                return Err(SYNTAX.into());
            }
            self.unsupported = true;
            return Ok(None);
        }
        Ok(r)
    }
}

fn bin(op: &str, a: Option<Num>, b: Option<Num>) -> R {
    let (Some(a), Some(b)) = (a, b) else { return Ok(None) };
    use Num::{F, I};
    let r = match (op, a, b) {
        ("+", I(x), I(y)) => x.checked_add(y).map(I).unwrap_or(F(x as f64 + y as f64)),
        ("-", I(x), I(y)) => x.checked_sub(y).map(I).unwrap_or(F(x as f64 - y as f64)),
        ("*", I(x), I(y)) => x.checked_mul(y).map(I).unwrap_or(F(x as f64 * y as f64)),
        ("+", x, y) => F(x.f() + y.f()),
        ("-", x, y) => F(x.f() - y.f()),
        ("*", x, y) => F(x.f() * y.f()),
        ("/", x, y) => {
            if y.f() == 0.0 {
                return Err(ZERO.into());
            }
            F(x.f() / y.f())
        }
        ("//", I(x), I(y)) => {
            if y == 0 {
                return Err(ZERO.into());
            }
            let q = x / y;
            I(if x % y != 0 && ((x < 0) != (y < 0)) { q - 1 } else { q })
        }
        ("//", x, y) => {
            if y.f() == 0.0 {
                return Err(ZERO.into());
            }
            F((x.f() / y.f()).floor())
        }
        ("%", I(x), I(y)) => {
            if y == 0 {
                return Err(ZERO.into());
            }
            let r = x % y;
            I(if r != 0 && ((r < 0) != (y < 0)) { r + y } else { r })
        }
        ("%", x, y) => {
            let (x, y) = (x.f(), y.f());
            if y == 0.0 {
                return Err(ZERO.into());
            }
            let r = x % y;
            F(if r != 0.0 && ((r < 0.0) != (y < 0.0)) { r + y } else { r })
        }
        ("**", x, y) => {
            if y.f().abs() > 100.0 {
                return Err("слишком большая степень".into());
            }
            match (x, y) {
                (I(b), I(e)) if e >= 0 => b.checked_pow(e as u32).map(I).unwrap_or(F((b as f64).powf(e as f64))),
                (x, y) => {
                    let (b, e) = (x.f(), y.f());
                    if b == 0.0 && e < 0.0 {
                        return Err(ZERO.into());
                    }
                    if b < 0.0 && e.fract() != 0.0 {
                        return Err("no real result".into());
                    }
                    F(b.powf(e))
                }
            }
        }
        _ => return Err(UNSUPPORTED.into()),
    };
    Ok(Some(r))
}

pub fn calc(s: &str) -> Result<Num, String> {
    let t = lex(s)?;
    // with a name, another operator or a complex number in it, whatever else is wrong, it is unsupported
    let other = t.contains(&Tok::Other);
    let n = t.len();
    let mut p = P { t, i: 0, unsupported: false };
    match p.expr() {
        Err(_) if other => Err(UNSUPPORTED.into()),
        Err(e) => Err(e),
        Ok(_) if p.i != n => Err(if other { UNSUPPORTED } else { SYNTAX }.into()),
        Ok(Some(v)) if !p.unsupported => Ok(v),
        Ok(_) => Err(UNSUPPORTED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        use Num::{F, I};
        for (e, want) in [
            ("-2**2", I(-4)),
            ("2**-1", F(0.5)),
            ("7//-2", I(-4)),
            ("-7//2", I(-4)),
            ("-7%3", I(2)),
            ("7%-3", I(-2)),
            ("7.5//2", F(3.0)),
            ("-7.5%2", F(0.5)),
            ("10/4", F(2.5)),
            ("2**10", I(1024)),
            ("0.1+0.2", F(0.30000000000000004)),
            ("(1+2)*3", I(9)),
            ("1_000*2", I(2000)),
            ("3**0.5", F(1.7320508075688772)),
            ("2*3.0", F(6.0)),
            ("17*23", I(391)),
            (".5+1", F(1.5)),
            ("0.15*2400", F(360.0)),
            ("2 + 2 * 2", I(6)),
            ("1e3", F(1000.0)),
            ("0x10", I(16)),
            ("(15*60+40) - (50 + 10)", I(880)),
        ] {
            assert_eq!(calc(e), Ok(want), "{e}");
        }
        for e in ["2 +", "(1+2", "0123", "2x"] {
            assert_eq!(calc(e), Err(SYNTAX.into()), "{e}");
        }
        for e in ["abc+1", "sqrt(2)", "1<2", "3j"] {
            assert_eq!(calc(e), Err(UNSUPPORTED.into()), "{e}");
        }
        for e in ["1/0", "1.5//0", "7%0", "0**-1"] {
            assert_eq!(calc(e), Err(ZERO.into()), "{e}");
        }
        assert_eq!(calc("2**101"), Err("слишком большая степень".into()));
        assert_eq!(calc("(-8)**0.5"), Err("no real result".into()));
    }
}
