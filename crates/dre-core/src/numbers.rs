//! Number filters for every template: `number`, `percent`, `signed`, `currency` and `compact`,
//! formatted for the run's `locale:`.
//!
//! DRE carries its own small table of locale conventions (decimal and group separators, where
//! the currency symbol and percent sign go) instead of a full CLDR library: it covers the
//! languages below, and a region changes the conventions only where the table says so.

use minijinja::value::Kwargs;
use minijinja::{Environment, Error, ErrorKind, Value};

/// How numbers are written in one locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Locale {
    /// The tag as written (`de-DE`), for messages.
    pub tag: &'static str,
    decimal: char,
    group: &'static str,
    /// `€12,340` (false) or `12.340 €` (true).
    currency_after: bool,
    /// A space between the symbol and the amount when the symbol comes first (`€ 12.340`).
    currency_space: bool,
    /// `4,1 %` rather than `4.1%`.
    percent_space: bool,
}

const NBSP: &str = "\u{a0}";
const NNBSP: &str = "\u{202f}";

const fn loc(
    tag: &'static str,
    decimal: char,
    group: &'static str,
    currency_after: bool,
    currency_space: bool,
    percent_space: bool,
) -> Locale {
    Locale {
        tag,
        decimal,
        group,
        currency_after,
        currency_space,
        percent_space,
    }
}

/// Languages, then language-region pairs that differ from their language.
const LOCALES: &[Locale] = &[
    loc("en", '.', ",", false, false, false),
    loc("de", ',', ".", true, false, true),
    loc("de-CH", '.', "\u{2019}", false, true, false),
    loc("fr", ',', NNBSP, true, false, true),
    loc("fr-CH", ',', NNBSP, true, false, true),
    loc("it", ',', ".", true, false, false),
    loc("it-CH", '.', "\u{2019}", false, true, false),
    loc("es", ',', ".", true, false, true),
    loc("es-MX", '.', ",", false, false, false),
    loc("es-US", '.', ",", false, false, false),
    loc("nl", ',', ".", false, true, false),
    loc("pt", ',', NBSP, true, false, false),
    loc("pt-BR", ',', ".", false, true, false),
    loc("sv", ',', NBSP, true, false, true),
    loc("da", ',', ".", true, false, true),
    loc("nb", ',', NBSP, true, false, true),
    loc("no", ',', NBSP, true, false, true),
    loc("fi", ',', NBSP, true, false, true),
    loc("pl", ',', NBSP, true, false, false),
    loc("cs", ',', NBSP, true, false, true),
    loc("ja", '.', ",", false, false, false),
    loc("zh", '.', ",", false, false, false),
    loc("ko", '.', ",", false, false, false),
];

impl Default for Locale {
    fn default() -> Self {
        LOCALES[0]
    }
}

impl Locale {
    /// The conventions for a BCP 47 tag like `de`, `de-DE` or `pt_BR`: its language-region
    /// entry, else its language's.
    pub fn parse(tag: &str) -> Result<Locale, String> {
        let norm = tag.replace('_', "-");
        let mut parts = norm.split('-');
        let lang = parts.next().unwrap_or_default().to_ascii_lowercase();
        let region = parts.next().map(str::to_ascii_uppercase);
        let valid = (2..=3).contains(&lang.len())
            && lang.chars().all(|c| c.is_ascii_alphabetic())
            && region
                .as_ref()
                .is_none_or(|r| (2..=3).contains(&r.len()) && r.chars().all(|c| c.is_ascii_alphanumeric()))
            && parts.next().is_none();
        let unknown = || {
            let langs: Vec<&str> = LOCALES.iter().map(|l| l.tag).filter(|t| !t.contains('-')).collect();
            format!(
                "unknown locale `{tag}`; use a language DRE knows ({}), optionally with a region (`de-DE`)",
                langs.join(", ")
            )
        };
        if !valid {
            return Err(unknown());
        }
        if let Some(r) = &region
            && let Some(l) = LOCALES.iter().find(|l| l.tag == format!("{lang}-{r}"))
        {
            return Ok(*l);
        }
        LOCALES.iter().find(|l| l.tag == lang).copied().ok_or_else(unknown)
    }

    /// `x` rounded half away from zero to `decimals` places, with group and decimal separators.
    /// Negative numbers get a leading `-`.
    pub fn number(&self, x: f64, decimals: usize) -> String {
        let scale = 10f64.powi(decimals as i32);
        let rounded = (x.abs() * scale).round() / scale;
        let text = format!("{rounded:.decimals$}");
        let (int, frac) = match text.split_once('.') {
            Some((i, f)) => (i, Some(f)),
            None => (text.as_str(), None),
        };
        let mut grouped = String::new();
        for (i, c) in int.chars().enumerate() {
            if i > 0 && (int.len() - i).is_multiple_of(3) {
                grouped.push_str(self.group);
            }
            grouped.push(c);
        }
        if let Some(f) = frac {
            grouped.push(self.decimal);
            grouped.push_str(f);
        }
        if x < 0.0 && rounded != 0.0 {
            format!("-{grouped}")
        } else {
            grouped
        }
    }

    pub fn percent(&self, x: f64, decimals: usize) -> String {
        let n = self.number(x * 100.0, decimals);
        if self.percent_space {
            format!("{n}{NBSP}%")
        } else {
            format!("{n}%")
        }
    }

    pub fn currency(&self, x: f64, code: &str, decimals: usize) -> String {
        let symbol = currency_symbol(code);
        let n = self.number(x.abs(), decimals);
        let sign = if x < 0.0 && n.chars().any(|c| c.is_ascii_digit() && c != '0') {
            "-"
        } else {
            ""
        };
        // A code with no symbol (`CHF`) is always followed or preceded by a space.
        let spaced = self.currency_space || symbol.chars().all(|c| c.is_ascii_uppercase());
        if self.currency_after {
            format!("{sign}{n}{NBSP}{symbol}")
        } else if spaced {
            format!("{sign}{symbol}{NBSP}{n}")
        } else {
            format!("{sign}{symbol}{n}")
        }
    }

    /// `1234567` → `1.2M`: thousands (`K`), millions (`M`), billions (`B`) and trillions (`T`),
    /// trailing zeros dropped.
    pub fn compact(&self, x: f64, decimals: usize) -> String {
        const UNITS: &[(f64, &str)] = &[(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")];
        let scale = 10f64.powi(decimals as i32);
        for (size, unit) in UNITS {
            let v = (x.abs() / size * scale).round() / scale;
            if v >= 1.0 {
                let n = self.number(v, decimals);
                let n = trim_zeros(&n, self.decimal);
                let sign = if x < 0.0 { "-" } else { "" };
                return format!("{sign}{n}{unit}");
            }
        }
        trim_zeros(&self.number(x, decimals), self.decimal)
    }
}

fn trim_zeros(n: &str, decimal: char) -> String {
    if n.contains(decimal) {
        n.trim_end_matches('0').trim_end_matches(decimal).to_string()
    } else {
        n.to_string()
    }
}

fn currency_symbol(code: &str) -> &str {
    match code {
        "EUR" => "€",
        "USD" => "$",
        "GBP" => "£",
        "JPY" | "CNY" => "¥",
        "INR" => "₹",
        "KRW" => "₩",
        "BRL" => "R$",
        "AUD" => "A$",
        "CAD" => "CA$",
        "NZD" => "NZ$",
        "MXN" => "MX$",
        "SEK" | "NOK" | "DKK" => "kr",
        "PLN" => "zł",
        "CZK" => "Kč",
        "ILS" => "₪",
        "TRY" => "₺",
        "ZAR" => "R",
        other => other,
    }
}

/// The number in `v`, or `None` for `none`/undefined (rendered as nothing).
fn to_number(filter: &str, v: &Value) -> Result<Option<f64>, Error> {
    if v.is_none() || v.is_undefined() {
        return Ok(None);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<f64>().map(Some).map_err(|_| {
            Error::new(
                ErrorKind::InvalidOperation,
                format!("`{filter}` needs a number, but got the text {s:?}"),
            )
        });
    }
    f64::try_from(v.clone()).map(Some).map_err(|_| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("`{filter}` needs a number, but got {} `{v}`", v.kind()),
        )
    })
}

fn decimals(kwargs: &Kwargs, positional: Option<u32>, default: usize) -> Result<usize, Error> {
    let named: Option<u32> = kwargs.get("decimals")?;
    let d = named.or(positional).map_or(default, |d| d as usize);
    if d > 12 {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "`decimals` must be between 0 and 12",
        ));
    }
    Ok(d)
}

/// Add the number filters, formatting for `locale`.
pub fn register(env: &mut Environment<'static>, locale: Locale) {
    env.add_filter(
        "number",
        move |v: Value, d: Option<u32>, kwargs: Kwargs| -> Result<String, Error> {
            let d = decimals(&kwargs, d, 0)?;
            kwargs.assert_all_used()?;
            Ok(to_number("number", &v)?.map_or_else(String::new, |x| locale.number(x, d)))
        },
    );
    env.add_filter(
        "percent",
        move |v: Value, d: Option<u32>, kwargs: Kwargs| -> Result<String, Error> {
            let d = decimals(&kwargs, d, 1)?;
            kwargs.assert_all_used()?;
            Ok(to_number("percent", &v)?.map_or_else(String::new, |x| locale.percent(x, d)))
        },
    );
    env.add_filter(
        "currency",
        move |v: Value, code: Option<String>, d: Option<u32>, kwargs: Kwargs| -> Result<String, Error> {
            let code = match code {
                Some(c) => Some(c),
                None => kwargs.get::<Option<String>>("code")?,
            };
            let d = decimals(&kwargs, d, 0)?;
            kwargs.assert_all_used()?;
            let code = code.ok_or_else(|| {
                Error::new(
                    ErrorKind::MissingArgument,
                    "`currency` needs a currency code, like `currency('EUR')`",
                )
            })?;
            if code.len() != 3 || !code.chars().all(|c| c.is_ascii_uppercase()) {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    format!("`currency('{code}')`: the code must be three capital letters (ISO 4217), like `EUR`"),
                ));
            }
            Ok(to_number("currency", &v)?.map_or_else(String::new, |x| locale.currency(x, &code, d)))
        },
    );
    env.add_filter(
        "compact",
        move |v: Value, d: Option<u32>, kwargs: Kwargs| -> Result<String, Error> {
            let d = decimals(&kwargs, d, 1)?;
            kwargs.assert_all_used()?;
            Ok(to_number("compact", &v)?.map_or_else(String::new, |x| locale.compact(x, d)))
        },
    );
    // A number gets `+` or `−`; text another filter made (`0.04 | percent | signed`) gets `+`
    // unless it's negative or zero.
    env.add_filter(
        "signed",
        move |v: Value, d: Option<u32>, kwargs: Kwargs| -> Result<String, Error> {
            let d = decimals(&kwargs, d, 0)?;
            kwargs.assert_all_used()?;
            let text = match v.as_str() {
                Some(s) => s.to_string(),
                None => match to_number("signed", &v)? {
                    Some(x) => locale.number(x, d),
                    None => return Ok(String::new()),
                },
            };
            Ok(signed(&text))
        },
    );
}

fn signed(text: &str) -> String {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix('-').or_else(|| t.strip_prefix('\u{2212}')) {
        return format!("\u{2212}{rest}");
    }
    if t.starts_with('+') || !t.chars().any(|c| c.is_ascii_digit() && c != '0') {
        return t.to_string();
    }
    format!("+{t}")
}
