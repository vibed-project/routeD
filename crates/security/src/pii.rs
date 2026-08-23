// SPDX-License-Identifier: Apache-2.0
//! Fast regex + checksum PII detection (emails, phones, IBAN with mod-97,
//! payment cards with Luhn, selected EU national identifiers).
//!
//! Returns entity types, confidences and byte spans only; never the matched
//! text, so results are safe to log.

use std::sync::LazyLock;

use regex::Regex;
use routed_snapshot::PiiEntity;

/// One detected entity.
#[derive(Clone, Debug, PartialEq)]
pub struct PiiMatch {
    /// Entity type.
    pub entity: PiiEntity,
    /// Confidence in `0..=1`.
    pub confidence: f64,
    /// Byte span in the input.
    pub span: (usize, usize),
}

static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9.\-]+\.[a-z]{2,}\b").unwrap_or_else(|e| panic!("{e}"))
});
static PHONE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:(?:\+|00)[1-9]\d{0,2}[\s\-.]?)?(?:\(?\d{2,4}\)?[\s\-.]?)\d{3,4}[\s\-.]?\d{3,4}\b",
    )
    .unwrap_or_else(|e| panic!("{e}"))
});
static IBAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Z]{2}\d{2}(?:[ ]?[A-Z0-9]{4}){2,7}(?:[ ]?[A-Z0-9]{1,4})?\b")
        .unwrap_or_else(|e| panic!("{e}"))
});
static CARD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(?:\d[ \-]?){13,19}\b").unwrap_or_else(|e| panic!("{e}")));
static DE_TAX_ID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b\d{2}[ ]?\d{3}[ ]?\d{3}[ ]?\d{3}\b").unwrap_or_else(|e| panic!("{e}"))
});
static NL_BSN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{9}\b").unwrap_or_else(|e| panic!("{e}")));
static ES_DNI: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[XYZ]?\d{7,8}[A-Z]\b").unwrap_or_else(|e| panic!("{e}")));
static FR_NIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[12]\d{2}(?:0[1-9]|1[0-2])(?:\d{2}|2A|2B)\d{6}(?:[ ]?\d{2})\b")
        .unwrap_or_else(|e| panic!("{e}"))
});
static HEALTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:diagnos(?:is|ed)|prescri(?:ption|bed)|medical record|patient id|icd-?10|blood (?:type|pressure)|hiv\b|chemotherapy|diabet(?:es|ic)|psychiatr)\b").unwrap_or_else(|e| panic!("{e}"))
});

fn iban_mod97_ok(raw: &str) -> bool {
    let s: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() < 15 || s.len() > 34 {
        return false;
    }
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let v = match c {
            '0'..='9' => u32::from(c as u8 - b'0'),
            'A'..='Z' => u32::from(c as u8 - b'A') + 10,
            _ => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

fn luhn_ok(raw: &str) -> bool {
    let digits: Vec<u32> = raw.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            if i % 2 == 1 {
                let x = d * 2;
                if x > 9 { x - 9 } else { x }
            } else {
                *d
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn de_tax_id_ok(raw: &str) -> bool {
    let d: Vec<u32> = raw.chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != 11 || d[0] == 0 {
        return false;
    }
    // one digit appears twice (or thrice) among the first ten, at least one digit never
    let mut counts = [0u8; 10];
    for &x in &d[..10] {
        counts[x as usize] += 1;
    }
    if !counts.contains(&0) || counts.iter().any(|&c| c > 3) {
        return false;
    }
    let mut product = 10u32;
    for &x in &d[..10] {
        let mut sum = (x + product) % 10;
        if sum == 0 {
            sum = 10;
        }
        product = (sum * 2) % 11;
    }
    let check = (11 - product) % 10;
    check == d[10]
}

fn nl_bsn_ok(raw: &str) -> bool {
    let d: Vec<i64> = raw
        .chars()
        .filter_map(|c| c.to_digit(10))
        .map(i64::from)
        .collect();
    if d.len() != 9 {
        return false;
    }
    let sum: i64 = d
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            if i == 8 {
                -x
            } else {
                x * i64::try_from(9 - i).unwrap_or(0)
            }
        })
        .sum();
    sum.rem_euclid(11) == 0
}

fn es_dni_ok(raw: &str) -> bool {
    const LETTERS: &[u8] = b"TRWAGMYFPDXBNJZSQVHLCKE";
    let (num, letter) = raw.split_at(raw.len() - 1);
    let num = match num.chars().next() {
        Some('X') => num.replacen('X', "0", 1),
        Some('Y') => num.replacen('Y', "1", 1),
        Some('Z') => num.replacen('Z', "2", 1),
        _ => num.to_owned(),
    };
    let Ok(n) = num.parse::<usize>() else {
        return false;
    };
    LETTERS[n % 23] == letter.as_bytes()[0]
}

fn fr_nir_ok(raw: &str) -> bool {
    let s: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() != 15 {
        return false;
    }
    let body = s[..13].replace("2A", "19").replace("2B", "18");
    let (Ok(n), Ok(key)) = (body.parse::<u64>(), s[13..].parse::<u64>()) else {
        return false;
    };
    97 - (n % 97) == key
}

/// Detect PII in text. Overlapping matches of different types are all reported.
#[must_use]
pub fn detect_pii(text: &str) -> Vec<PiiMatch> {
    let mut out = Vec::new();
    for m in EMAIL.find_iter(text) {
        out.push(PiiMatch {
            entity: PiiEntity::Email,
            confidence: 0.95,
            span: (m.start(), m.end()),
        });
    }
    for m in IBAN.find_iter(text) {
        if iban_mod97_ok(m.as_str()) {
            out.push(PiiMatch {
                entity: PiiEntity::Iban,
                confidence: 0.98,
                span: (m.start(), m.end()),
            });
        }
    }
    for m in CARD.find_iter(text) {
        if luhn_ok(m.as_str())
            && !out
                .iter()
                .any(|p| p.span.0 <= m.start() && m.end() <= p.span.1)
        {
            out.push(PiiMatch {
                entity: PiiEntity::CreditCard,
                confidence: 0.85,
                span: (m.start(), m.end()),
            });
        }
    }
    let covered =
        |s: usize, e: usize, out: &[PiiMatch]| out.iter().any(|p| p.span.0 <= s && e <= p.span.1);
    for m in DE_TAX_ID.find_iter(text) {
        if de_tax_id_ok(m.as_str()) && !covered(m.start(), m.end(), &out) {
            out.push(PiiMatch {
                entity: PiiEntity::NationalId,
                confidence: 0.8,
                span: (m.start(), m.end()),
            });
        }
    }
    for m in NL_BSN.find_iter(text) {
        if nl_bsn_ok(m.as_str()) && !covered(m.start(), m.end(), &out) {
            // ~9% of random 9-digit numbers pass the elfproef; only trust it with context.
            let ctx_start = m.start().saturating_sub(24);
            let context = text
                .get(ctx_start..m.start())
                .unwrap_or("")
                .to_ascii_lowercase();
            let confidence = if context.contains("bsn") || context.contains("burgerservice") {
                0.9
            } else {
                0.5
            };
            out.push(PiiMatch {
                entity: PiiEntity::NationalId,
                confidence,
                span: (m.start(), m.end()),
            });
        }
    }
    for m in ES_DNI.find_iter(text) {
        if es_dni_ok(m.as_str()) && !covered(m.start(), m.end(), &out) {
            out.push(PiiMatch {
                entity: PiiEntity::NationalId,
                confidence: 0.85,
                span: (m.start(), m.end()),
            });
        }
    }
    for m in FR_NIR.find_iter(text) {
        if fr_nir_ok(m.as_str()) && !covered(m.start(), m.end(), &out) {
            out.push(PiiMatch {
                entity: PiiEntity::NationalId,
                confidence: 0.9,
                span: (m.start(), m.end()),
            });
        }
    }
    for m in PHONE.find_iter(text) {
        let digits = m.as_str().chars().filter(char::is_ascii_digit).count();
        if (8..=15).contains(&digits) && !covered(m.start(), m.end(), &out) {
            out.push(PiiMatch {
                entity: PiiEntity::Phone,
                confidence: 0.6,
                span: (m.start(), m.end()),
            });
        }
    }
    if let Some(m) = HEALTH.find(text) {
        out.push(PiiMatch {
            entity: PiiEntity::Health,
            confidence: 0.6,
            span: (m.start(), m.end()),
        });
    }
    out.sort_by_key(|p| (p.span.0, p.span.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entities(text: &str) -> Vec<PiiEntity> {
        detect_pii(text).into_iter().map(|p| p.entity).collect()
    }

    #[test]
    fn email_and_phone() {
        assert_eq!(
            entities("mail me at jane.doe@example.org please"),
            vec![PiiEntity::Email]
        );
        assert_eq!(entities("call +49 30 1234567"), vec![PiiEntity::Phone]);
        assert!(entities("the year 2024 was fine").is_empty());
    }

    #[test]
    fn iban_checksum() {
        assert_eq!(
            entities("IBAN DE89 3704 0044 0532 0130 00"),
            vec![PiiEntity::Iban]
        );
        assert_eq!(entities("GB82WEST12345698765432"), vec![PiiEntity::Iban]);
        assert!(
            entities("DE89 3704 0044 0532 0130 01")
                .iter()
                .all(|e| *e != PiiEntity::Iban)
        );
    }

    #[test]
    fn card_luhn() {
        assert_eq!(
            entities("card 4111 1111 1111 1111"),
            vec![PiiEntity::CreditCard]
        );
        assert!(
            entities("4111 1111 1111 1112")
                .iter()
                .all(|e| *e != PiiEntity::CreditCard)
        );
    }

    #[test]
    fn national_ids() {
        assert!(de_tax_id_ok("86095742719"));
        assert!(!de_tax_id_ok("86095742718"));
        assert!(nl_bsn_ok("111222333"));
        assert!(!nl_bsn_ok("111222334"));
        assert!(es_dni_ok("12345678Z"));
        assert!(!es_dni_ok("12345678A"));
        assert!(fr_nir_ok("255081416802538"));
        assert_eq!(entities("DNI 12345678Z"), vec![PiiEntity::NationalId]);
    }

    #[test]
    fn health_terms() {
        assert_eq!(
            entities("The patient was diagnosed with diabetes."),
            vec![PiiEntity::Health]
        );
    }
}
