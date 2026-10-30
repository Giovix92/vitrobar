//! Quick math for the Start search box: `+ - * / ^ %`, unary minus, parentheses.

use crate::i18n::Lang;

const MAX_TOKENS: usize = 64;
const MAX_DEPTH: usize = 32;

/// True when the text is plausibly an expression: it has a digit, only math characters, and at
/// least one operator (so a bare number such as "2024" stays a normal search).
pub fn looks_like_math(s: &str) -> bool {
    let s = s.trim();
    s.chars().any(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_digit() || " .,+-*/x×÷^%()".contains(c))
        && s.chars().any(|c| "+-*/x×÷^%".contains(c))
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    Num(f64),
    Op(char),
    Open,
    Close,
}

fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if out.len() >= MAX_TOKENS {
            return None;
        }
        match c {
            '0'..='9' | '.' | ',' => {
                let start = i;
                let mut seps = 0;
                while i < chars.len()
                    && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == ',')
                {
                    if !chars[i].is_ascii_digit() {
                        seps += 1;
                    }
                    i += 1;
                }
                if seps > 1 {
                    return None;
                }
                let text: String = chars[start..i]
                    .iter()
                    .map(|&c| if c == ',' { '.' } else { c })
                    .collect();
                if !text.chars().any(|c| c.is_ascii_digit()) {
                    return None;
                }
                out.push(Tok::Num(text.parse().ok()?));
                continue;
            }
            '+' | '-' | '*' | '/' | '^' | '%' => out.push(Tok::Op(c)),
            'x' | '×' => out.push(Tok::Op('*')),
            '÷' => out.push(Tok::Op('/')),
            '(' => out.push(Tok::Open),
            ')' => out.push(Tok::Close),
            _ => return None,
        }
        i += 1;
    }
    Some(out)
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    depth: usize,
}

fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

impl Parser {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.pos).copied()
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        while let Some(Tok::Op(c @ ('+' | '-'))) = self.peek() {
            self.pos += 1;
            let r = self.term()?;
            v = finite(if c == '+' { v + r } else { v - r })?;
        }
        Some(v)
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(Tok::Op(c @ ('*' | '/' | '%'))) = self.peek() {
            self.pos += 1;
            let r = self.unary()?;
            v = match c {
                '*' => v * r,
                _ if r == 0.0 => return None,
                '/' => v / r,
                _ => v % r,
            };
            v = finite(v)?;
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f64> {
        if self.peek() == Some(Tok::Op('-')) {
            self.pos += 1;
            self.depth += 1;
            if self.depth > MAX_DEPTH {
                return None;
            }
            let v = self.unary();
            self.depth -= 1;
            return Some(-v?);
        }
        self.power()
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.atom()?;
        if self.peek() == Some(Tok::Op('^')) {
            self.pos += 1;
            let exp = self.unary()?; // right-associative; allows 2^-1
            return finite(base.powf(exp));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        match self.peek()? {
            Tok::Num(n) => {
                self.pos += 1;
                Some(n)
            }
            Tok::Open => {
                self.pos += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return None;
                }
                let v = self.expr()?;
                self.depth -= 1;
                if self.peek() != Some(Tok::Close) {
                    return None;
                }
                self.pos += 1;
                Some(v)
            }
            _ => None,
        }
    }
}

/// Evaluates an expression; `None` for anything malformed or non-finite.
pub fn eval(s: &str) -> Option<f64> {
    let toks = tokenize(s)?;
    if toks.is_empty() {
        return None;
    }
    let mut p = Parser {
        toks,
        pos: 0,
        depth: 0,
    };
    let v = p.expr()?;
    if p.pos != p.toks.len() {
        return None;
    }
    finite(v)
}

/// Up to 10 significant digits, trailing zeros trimmed, decimal comma in Italian. Very large
/// (|v| >= 1e15) and very small (0 < |v| < 1e-6) values use scientific notation ("1e300",
/// "1,5e-12").
pub fn format_result(v: f64, lang: Lang) -> String {
    let sci = format!("{:.9e}", v);
    let rounded: f64 = sci.parse().unwrap_or(v);
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    let a = rounded.abs();
    let s = if a >= 1e15 || (a > 0.0 && a < 1e-6) {
        // "1.500000000e-12" -> "1.5e-12".
        let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        format!("{mantissa}e{exp}")
    } else {
        rounded.to_string()
    };
    match lang {
        Lang::En => s,
        Lang::It => s.replace('.', ","),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn math_basics() {
        assert_eq!(eval("2+2*3"), Some(8.0));
        assert_eq!(eval("(1+2)^2"), Some(9.0));
        assert_eq!(eval("2^3^2"), Some(512.0));
        assert_eq!(eval("-3*-3"), Some(9.0));
        assert_eq!(eval("7 % 3"), Some(1.0));
        assert_eq!(eval("1,5×2"), Some(3.0));
    }
    #[test]
    fn math_rejects() {
        for s in ["1/0", "2^9999", "((", "", "abc", "1++", &"(".repeat(40)] {
            assert_eq!(eval(s), None, "{s}");
        }
    }
    #[test]
    fn looks_like_math_needs_operator() {
        assert!(looks_like_math("12*4"));
        assert!(!looks_like_math("2024"));
        assert!(!looks_like_math("chrome"));
    }
    #[test]
    fn formats_per_language() {
        assert_eq!(format_result(2.5, Lang::It), "2,5");
        assert_eq!(format_result(1.0 / 3.0, Lang::En), "0.3333333333");
        assert_eq!(format_result(8.0, Lang::En), "8");
    }
    #[test]
    fn huge_and_tiny_use_scientific_notation() {
        assert_eq!(format_result(1e300, Lang::En), "1e300");
        assert_eq!(format_result(1.5e-12, Lang::En), "1.5e-12");
        assert_eq!(format_result(1.5e-12, Lang::It), "1,5e-12");
        assert_eq!(format_result(-2.5e20, Lang::It), "-2,5e20");
        assert_eq!(format_result(1e15, Lang::En), "1e15");
        assert_eq!(format_result(999_999_999_999_999.0, Lang::En), "1e15");
        assert_eq!(format_result(123_456_789_012.0, Lang::En), "123456789000");
        assert_eq!(format_result(0.000_001, Lang::En), "0.000001");
        assert_eq!(format_result(0.0, Lang::En), "0");
        assert!(format_result(eval("10^300").unwrap(), Lang::En).len() < 20);
    }
}
