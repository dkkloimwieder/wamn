//! The one parser of document-number patterns (`docs/plan/platform-config.md`,
//! "Numbering").
//!
//! A pattern is literal text plus tokens:
//!
//! - `{seq:<counter>[,reset=<key>][,per=<key>][,width=N|0N][,pad=C][,hex]}`: one
//!   counter. Options are comma-separated and their order is free.
//! - `{field:<name>}`: a field of the record, as text.
//! - `{date:FMT}`: the transaction date, with `FMT` one of `YYYY`, `YY`, `MM`,
//!   `DD`, `WW` (ISO week) or `Q`.
//!
//! A `<key>` is `date:FMT`, the name of another counter in the same pattern,
//! or else a field of the record. The generator validates a package's
//! patterns with [`parse`] at build, and the `wamn:postgres` renderer renders
//! the same [`Pattern`]. No other parser exists.

use std::error::Error;
use std::fmt;

/// The widest zero-padded counter a pattern may ask for.
pub const MAX_WIDTH: u16 = 32;

/// One parsed pattern: its parts, in pattern order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    parts: Box<[Part]>,
}

/// One part of a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// Text rendered as written.
    Literal(Box<str>),
    /// A `{seq:...}` token.
    Counter(Counter),
    /// A `{field:<name>}` token.
    Field(Box<str>),
    /// A `{date:FMT}` token.
    Date(DateFormat),
}

/// One `{seq:...}` token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Counter {
    name: Box<str>,
    scope: Scope,
    width: Option<u16>,
    pad: char,
    hex: bool,
}

impl Counter {
    /// The counter's name, unique within its pattern.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// When the counter starts again, or whether a key reuses its value.
    pub const fn scope(&self) -> &Scope {
        &self.scope
    }

    /// The width the value is padded to, if the token states one.
    pub const fn width(&self) -> Option<u16> {
        self.width
    }

    /// The pad character: `0` unless the token states `pad=`.
    pub const fn pad(&self) -> char {
        self.pad
    }

    /// Whether the value renders in hexadecimal.
    pub const fn hex(&self) -> bool {
        self.hex
    }

    /// Whether the token is bare: `{seq:<counter>}` with no option.
    pub const fn is_bare(&self) -> bool {
        matches!(self.scope, Scope::Always) && self.width.is_none() && self.pad == '0' && !self.hex
    }
}

/// How a counter advances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// One counter that never resets.
    Always,
    /// `reset=<key>`: advances every call and starts at 1 for each distinct key.
    Reset(Key),
    /// `per=<key>`: one value for each distinct key, assigned on first sight.
    Per(Key),
}

impl Scope {
    /// The key the scope names, if it names one.
    pub const fn key(&self) -> Option<&Key> {
        match self {
            Self::Always => None,
            Self::Reset(key) | Self::Per(key) => Some(key),
        }
    }
}

/// What a `reset=` or `per=` option names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// A field of the record.
    Field(Box<str>),
    /// Another counter of the same pattern, by its rendered value.
    Counter(Box<str>),
    /// The transaction date in one format.
    Date(DateFormat),
}

/// The renderer's own date formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DateFormat {
    /// `YYYY`: four-digit year.
    Year,
    /// `YY`: two-digit year.
    ShortYear,
    /// `MM`: two-digit month.
    Month,
    /// `DD`: two-digit day of the month.
    Day,
    /// `WW`: two-digit ISO week.
    Week,
    /// `Q`: quarter, 1 to 4.
    Quarter,
}

impl DateFormat {
    /// Every format, in the order the pattern table lists them.
    pub const ALL: [Self; 6] = [
        Self::Year,
        Self::ShortYear,
        Self::Month,
        Self::Day,
        Self::Week,
        Self::Quarter,
    ];

    /// The format as a pattern spells it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Year => "YYYY",
            Self::ShortYear => "YY",
            Self::Month => "MM",
            Self::Day => "DD",
            Self::Week => "WW",
            Self::Quarter => "Q",
        }
    }
}

impl Pattern {
    /// The parts, in pattern order.
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// The counters, in pattern order.
    pub fn counters(&self) -> impl Iterator<Item = &Counter> {
        self.parts.iter().filter_map(|part| match part {
            Part::Counter(counter) => Some(counter),
            _ => None,
        })
    }

    /// The record fields the pattern names, sorted and once each: every
    /// `{field:...}` token and every field key. A caller passes exactly these.
    pub fn fields(&self) -> Vec<&str> {
        let mut fields = self
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Field(name) => Some(&**name),
                Part::Counter(counter) => match counter.scope.key() {
                    Some(Key::Field(name)) => Some(&**name),
                    _ => None,
                },
                Part::Literal(_) | Part::Date(_) => None,
            })
            .collect::<Vec<_>>();
        fields.sort_unstable();
        fields.dedup();
        fields
    }

    /// Whether the pattern is one bare counter, `{seq:<counter>}`, and nothing
    /// else: the only pattern a numbered field that is not text takes.
    pub fn is_bare_counter(&self) -> bool {
        matches!(&*self.parts, [Part::Counter(counter)] if counter.is_bare())
    }
}

/// Why a pattern does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    kind: PatternErrorKind,
    detail: Box<str>,
}

/// The class of a [`PatternError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PatternErrorKind {
    /// Unbalanced braces, an unknown token, or a name that is not an identifier.
    Syntax,
    /// A `date:` format outside the renderer's set.
    UnknownDateFormat,
    /// An unknown, repeated or malformed `seq` option, or `reset=` with `per=`.
    InvalidOption,
    /// A counter named twice, or a counter keyed by itself or by a cycle.
    InvalidCounter,
}

impl PatternError {
    /// The class of the refusal.
    pub const fn kind(&self) -> PatternErrorKind {
        self.kind
    }

    fn new(kind: PatternErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for PatternError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl Error for PatternError {}

/// Parse one pattern.
///
/// # Errors
///
/// [`PatternError`] when the pattern is not in the grammar of the module
/// documentation.
pub fn parse(pattern: &str) -> Result<Pattern, PatternError> {
    let mut parts = Vec::new();
    let mut raw_keys = Vec::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        let next = rest.find(['{', '}']).unwrap_or(rest.len());
        if next > 0 {
            parts.push(Part::Literal(rest[..next].into()));
            rest = &rest[next..];
            continue;
        }
        if rest.starts_with('}') {
            return Err(syntax(format!(
                "pattern {pattern:?} has a '}}' outside a token"
            )));
        }
        let end = rest
            .find('}')
            .ok_or_else(|| syntax(format!("pattern {pattern:?} has an unclosed token")))?;
        let body = &rest[1..end];
        if body.contains('{') {
            return Err(syntax(format!("pattern {pattern:?} nests a token")));
        }
        let (part, key) = token(body)?;
        parts.push(part);
        raw_keys.push(key);
        rest = &rest[end + 1..];
    }

    let counters = parts
        .iter()
        .filter_map(|part| match part {
            Part::Counter(counter) => Some(counter.name.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for (index, name) in counters.iter().enumerate() {
        if counters[..index].contains(name) {
            return Err(PatternError::new(
                PatternErrorKind::InvalidCounter,
                format!("pattern {pattern:?} names counter {name} twice"),
            ));
        }
    }
    // Resolve each named key now that every counter of the pattern is known.
    let tokens = parts
        .iter_mut()
        .filter(|part| !matches!(part, Part::Literal(_)));
    for (part, key) in tokens.zip(raw_keys) {
        let (Part::Counter(counter), Some(name)) = (part, key) else {
            continue;
        };
        let key = if counters.contains(&name) {
            Key::Counter(name)
        } else {
            Key::Field(name)
        };
        counter.scope = match &counter.scope {
            Scope::Reset(_) => Scope::Reset(key),
            Scope::Per(_) => Scope::Per(key),
            Scope::Always => unreachable!("only a reset= or per= option names a key"),
        };
    }
    let pattern_value = Pattern {
        parts: parts.into_boxed_slice(),
    };
    refuse_counter_cycles(&pattern_value, pattern)?;
    Ok(pattern_value)
}

/// Parse one token body: the part, and the key name its `reset=` or `per=`
/// option names, unresolved, when the option names no date.
fn token(body: &str) -> Result<(Part, Option<Box<str>>), PatternError> {
    let (kind, rest) = body
        .split_once(':')
        .ok_or_else(|| syntax(format!("token {{{body}}} has no kind")))?;
    match kind {
        "field" => Ok((Part::Field(identifier(rest, "field")?.into()), None)),
        "date" => Ok((Part::Date(date_format(rest)?), None)),
        "seq" => counter(rest),
        _ => Err(syntax(format!(
            "token {{{body}}} is not seq, field or date"
        ))),
    }
}

fn counter(body: &str) -> Result<(Part, Option<Box<str>>), PatternError> {
    let mut options = body.split(',');
    let name = identifier(options.next().unwrap_or_default(), "counter")?;
    let mut scope = Scope::Always;
    let mut key = None;
    let mut width = None;
    let mut pad = None;
    let mut hex = false;
    for option in options {
        let invalid = |detail: &str| {
            PatternError::new(
                PatternErrorKind::InvalidOption,
                format!("counter {name} option {option:?} {detail}"),
            )
        };
        if let Some(value) = option
            .strip_prefix("reset=")
            .or(option.strip_prefix("per="))
        {
            if scope != Scope::Always {
                return Err(invalid("is a second reset= or per="));
            }
            // A named key is a placeholder until the pattern's counters are known.
            let resolved = if let Some(format) = value.strip_prefix("date:") {
                Key::Date(date_format(format)?)
            } else {
                key = Some(identifier(value, "key")?.into());
                Key::Field("".into())
            };
            scope = if option.starts_with("reset=") {
                Scope::Reset(resolved)
            } else {
                Scope::Per(resolved)
            };
        } else if let Some(value) = option.strip_prefix("width=") {
            if width.is_some() {
                return Err(invalid("states the width twice"));
            }
            width = Some(width_value(value).ok_or_else(|| invalid("is not a width"))?);
        } else if option.len() > 1 && option.starts_with('0') {
            if width.is_some() {
                return Err(invalid("states the width twice"));
            }
            width = Some(width_value(&option[1..]).ok_or_else(|| invalid("is not a width"))?);
        } else if let Some(value) = option.strip_prefix("pad=") {
            let mut characters = value.chars();
            let (Some(character), None) = (characters.next(), characters.next()) else {
                return Err(invalid("is not one character"));
            };
            if pad.is_some() {
                return Err(invalid("states the pad twice"));
            }
            pad = Some(character);
        } else if option == "hex" {
            if hex {
                return Err(invalid("states hex twice"));
            }
            hex = true;
        } else {
            return Err(invalid("is not reset=, per=, width=, 0N, pad= or hex"));
        }
    }
    Ok((
        Part::Counter(Counter {
            name: name.into(),
            scope,
            width,
            pad: pad.unwrap_or('0'),
            hex,
        }),
        key,
    ))
}

fn width_value(value: &str) -> Option<u16> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|width| (1..=MAX_WIDTH).contains(width))
}

fn date_format(format: &str) -> Result<DateFormat, PatternError> {
    DateFormat::ALL
        .into_iter()
        .find(|candidate| candidate.as_str() == format)
        .ok_or_else(|| {
            PatternError::new(
                PatternErrorKind::UnknownDateFormat,
                format!("date format {format:?} is not YYYY, YY, MM, DD, WW or Q"),
            )
        })
}

/// A field, counter or key name: lowercase ASCII letters, digits and `_`,
/// not starting with a digit.
fn identifier<'a>(name: &'a str, what: &str) -> Result<&'a str, PatternError> {
    let mut bytes = name.bytes();
    let valid = bytes
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(name)
    } else {
        Err(syntax(format!("{what} name {name:?} is not an identifier")))
    }
}

/// Refuse a counter keyed by itself, or by a chain of counters that returns
/// to it: nothing could render first.
fn refuse_counter_cycles(pattern: &Pattern, text: &str) -> Result<(), PatternError> {
    let key_of = |name: &str| {
        pattern
            .counters()
            .find(|counter| &*counter.name == name)
            .and_then(|counter| match counter.scope.key() {
                Some(Key::Counter(next)) => Some(next.clone()),
                _ => None,
            })
    };
    for counter in pattern.counters() {
        let mut current = counter.name.clone();
        for _ in 0..pattern.counters().count() {
            match key_of(&current) {
                Some(next) if next == counter.name => {
                    return Err(PatternError::new(
                        PatternErrorKind::InvalidCounter,
                        format!("pattern {text:?} keys counter {} by itself", counter.name),
                    ));
                }
                Some(next) => current = next,
                None => break,
            }
        }
    }
    Ok(())
}

fn syntax(detail: String) -> PatternError {
    PatternError::new(PatternErrorKind::Syntax, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spec_examples_parse() {
        let pattern = parse("P-{seq:po,width=8}").unwrap();
        assert_eq!(pattern.parts()[0], Part::Literal("P-".into()));
        let po = pattern.counters().next().unwrap();
        assert_eq!((po.name(), po.width(), po.pad()), ("po", Some(8), '0'));
        assert!(pattern.fields().is_empty());

        let pattern = parse("{field:facility}-PO-{seq:po,reset=facility,width=6}").unwrap();
        assert_eq!(pattern.fields(), ["facility"]);
        assert_eq!(
            pattern.counters().next().unwrap().scope(),
            &Scope::Reset(Key::Field("facility".into()))
        );

        let pattern = parse("{date:YY}-{seq:po,reset=date:YY}").unwrap();
        assert_eq!(
            pattern.counters().next().unwrap().scope(),
            &Scope::Reset(Key::Date(DateFormat::ShortYear))
        );
        assert!(pattern.fields().is_empty());

        let pattern =
            parse("B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}").unwrap();
        let counters = pattern.counters().collect::<Vec<_>>();
        assert_eq!(
            counters[0].scope(),
            &Scope::Per(Key::Field("batch_id".into()))
        );
        assert_eq!(
            counters[1].scope(),
            &Scope::Reset(Key::Counter("batch".into()))
        );
        assert_eq!(pattern.fields(), ["batch_id"]);
    }

    #[test]
    fn options_are_order_free_and_take_the_shorthand_width() {
        let pattern = parse("{seq:n,hex,pad=_,08}").unwrap();
        let n = pattern.counters().next().unwrap();
        assert_eq!((n.width(), n.pad(), n.hex()), (Some(8), '_', true));
        assert!(!pattern.is_bare_counter());
        assert!(parse("{seq:n}").unwrap().is_bare_counter());
        assert!(!parse("N{seq:n}").unwrap().is_bare_counter());
    }

    #[test]
    fn refusals_carry_their_kind() {
        for (pattern, kind) in [
            ("{date:YYYYMM}", PatternErrorKind::UnknownDateFormat),
            ("{seq:n,reset=date:HH}", PatternErrorKind::UnknownDateFormat),
            ("{seq:n", PatternErrorKind::Syntax),
            ("a}b", PatternErrorKind::Syntax),
            ("{when:now}", PatternErrorKind::Syntax),
            ("{seq:N}", PatternErrorKind::Syntax),
            ("{seq:n,step=2}", PatternErrorKind::InvalidOption),
            ("{seq:n,width=0}", PatternErrorKind::InvalidOption),
            ("{seq:n,width=4,05}", PatternErrorKind::InvalidOption),
            ("{seq:n,reset=a,per=b}", PatternErrorKind::InvalidOption),
            ("{seq:n,pad=ab}", PatternErrorKind::InvalidOption),
            ("{seq:n}{seq:n}", PatternErrorKind::InvalidCounter),
            ("{seq:n,reset=n}", PatternErrorKind::InvalidCounter),
            (
                "{seq:a,reset=b}{seq:b,per=a}",
                PatternErrorKind::InvalidCounter,
            ),
        ] {
            assert_eq!(parse(pattern).expect_err(pattern).kind(), kind, "{pattern}");
        }
    }
}
