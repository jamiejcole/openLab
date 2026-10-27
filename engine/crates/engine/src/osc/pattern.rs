// SPDX-License-Identifier: GPL-3.0-or-later

//! OSC address patterns (`* ? [..] {a,b}`) for the cue-number part of an address, matched against
//! cue numbers. Pure and bounded: the pattern is compiled first (a bad one is an error, not a
//! guess), alternatives are expanded up to a cap, and matching runs on a step budget so a
//! hostile pattern against hostile numbers cannot hold the OSC task.

/// Most alternatives a pattern may expand to (the product of its `{..}` groups).
pub const MAX_ALTERNATIVES: usize = 64;
/// Longest cue number a pattern is tried against, in characters. A longer one can still be
/// addressed exactly.
pub const MAX_MATCH_CHARS: usize = 256;
/// Steps one message may spend matching, over all cues and alternatives. The matcher's steps are
/// single comparisons, so this is a few milliseconds at most.
pub const MATCH_BUDGET: u64 = 2_000_000;

/// Whether `text` has a character that makes it a pattern.
pub fn has_pattern_chars(text: &str) -> bool {
    text.contains(['*', '?', '[', ']', '{', '}'])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PatternError {
    #[error("unclosed or empty [..] or {{..}}")]
    Unclosed,
    #[error("{{..}} inside {{..}}, or a stray ] or }}")]
    Nested,
    #[error("more than {MAX_ALTERNATIVES} alternatives")]
    TooManyAlternatives,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `?`
    One,
    /// `*`
    Star,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Tok {
    fn accepts(&self, c: char) -> bool {
        match self {
            Self::Lit(l) => *l == c,
            Self::One => true,
            Self::Star => false,
            Self::Class { negated, ranges } => {
                ranges.iter().any(|(lo, hi)| (*lo..=*hi).contains(&c)) != *negated
            }
        }
    }
}

/// A compiled pattern: matches if any of its alternatives matches the whole text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CuePattern {
    alternatives: Vec<Vec<Tok>>,
}

/// Matching ran out of its step budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetSpent;

impl CuePattern {
    pub fn compile(pattern: &str) -> Result<Self, PatternError> {
        let mut alternatives: Vec<Vec<Tok>> = vec![Vec::new()];
        let mut chars = pattern.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    let mut options: Vec<Vec<Tok>> = vec![Vec::new()];
                    loop {
                        match chars.next().ok_or(PatternError::Unclosed)? {
                            '}' => break,
                            ',' => options.push(Vec::new()),
                            '{' => return Err(PatternError::Nested),
                            other => {
                                let tok = simple_token(other, &mut chars)?;
                                if let Some(last) = options.last_mut() {
                                    last.push(tok);
                                }
                            }
                        }
                    }
                    if alternatives.len().saturating_mul(options.len()) > MAX_ALTERNATIVES {
                        return Err(PatternError::TooManyAlternatives);
                    }
                    let mut product = Vec::with_capacity(alternatives.len() * options.len());
                    for prefix in &alternatives {
                        for option in &options {
                            let mut next = prefix.clone();
                            next.extend(option.iter().cloned());
                            product.push(next);
                        }
                    }
                    alternatives = product;
                }
                '}' => return Err(PatternError::Nested),
                other => {
                    let tok = simple_token(other, &mut chars)?;
                    for alternative in &mut alternatives {
                        alternative.push(tok.clone());
                    }
                }
            }
        }
        Ok(Self { alternatives })
    }

    /// Whether `text` matches. `budget` is decremented by the work done; `Err` when it is spent
    /// (the answer is then unknown and the caller must stop matching). A text longer than
    /// [`MAX_MATCH_CHARS`] does not match.
    pub fn is_match(&self, text: &str, budget: &mut u64) -> Result<bool, BudgetSpent> {
        let text: Vec<char> = text.chars().take(MAX_MATCH_CHARS + 1).collect();
        if text.len() > MAX_MATCH_CHARS {
            return Ok(false);
        }
        for alternative in &self.alternatives {
            if glob(alternative, &text, budget)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// A token outside `{..}` alternatives' structure: `*`, `?`, a `[..]` class or a literal.
fn simple_token(
    c: char,
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<Tok, PatternError> {
    match c {
        '*' => Ok(Tok::Star),
        '?' => Ok(Tok::One),
        ']' => Err(PatternError::Nested),
        '[' => {
            let negated = chars.next_if_eq(&'!').is_some();
            let mut ranges = Vec::new();
            loop {
                let lo = chars.next().ok_or(PatternError::Unclosed)?;
                if lo == ']' {
                    break;
                }
                // `a-z`; a `-` last (or first) is itself.
                if chars.peek() == Some(&'-') {
                    let mut ahead = chars.clone();
                    ahead.next();
                    match ahead.next() {
                        Some(']') | None => ranges.push((lo, lo)),
                        Some(hi) => {
                            chars.next();
                            chars.next();
                            ranges.push((lo.min(hi), lo.max(hi)));
                        }
                    }
                } else {
                    ranges.push((lo, lo));
                }
            }
            if ranges.is_empty() {
                return Err(PatternError::Unclosed);
            }
            Ok(Tok::Class { negated, ranges })
        }
        literal => Ok(Tok::Lit(literal)),
    }
}

/// Whole-text match with `*` backtracking to the last star only: O(tokens x text).
fn glob(tokens: &[Tok], text: &[char], budget: &mut u64) -> Result<bool, BudgetSpent> {
    let (mut t, mut s) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while s < text.len() {
        *budget = budget.checked_sub(1).ok_or(BudgetSpent)?;
        let c = text.get(s).copied().unwrap_or_default();
        match tokens.get(t) {
            Some(Tok::Star) => {
                star = Some((t, s));
                t += 1;
            }
            Some(tok) if tok.accepts(c) => {
                t += 1;
                s += 1;
            }
            _ => match star {
                Some((star_t, star_s)) => {
                    t = star_t + 1;
                    s = star_s + 1;
                    star = Some((star_t, star_s + 1));
                }
                None => return Ok(false),
            },
        }
    }
    while matches!(tokens.get(t), Some(Tok::Star)) {
        t += 1;
    }
    Ok(t == tokens.len())
}
