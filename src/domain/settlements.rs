//! Primorye settlements gazetteer.
//!
//! Generated once from OSM (`place=city|town|village|hamlet`) and embedded as
//! `data/geo/primorye_settlements.json`. Used to recognise the city or village
//! named in a free-form query without a hand-written short list.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::domain::geo::haversine_km;

#[derive(Debug, Clone, Deserialize)]
pub struct Settlement {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

static SETTLEMENTS: OnceLock<Vec<Settlement>> = OnceLock::new();

fn all() -> &'static Vec<Settlement> {
    SETTLEMENTS.get_or_init(|| {
        serde_json::from_str(include_str!("../../data/geo/primorye_settlements.json"))
            .unwrap_or_default()
    })
}

fn normalize(value: &str) -> String {
    value
        .to_lowercase()
        .replace('ё', "е")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn ambiguous(word: &str) -> bool {
    matches!(
        word,
        "лесное"
            | "полевое"
            | "новое"
            | "красное"
            | "озерное"
            | "речное"
            | "горное"
            | "степное"
            | "луговое"
            | "тихое"
            | "дальнее"
            | "верхнее"
            | "нижнее"
            | "малое"
            | "большое"
            | "среднее"
            | "восточное"
            | "западное"
            | "северное"
            | "южное"
            | "центральное"
            | "рабочее"
            | "заречное"
            | "прибрежное"
    )
}

/// Russian cases make exact equality too strict and plain prefixes too loose
/// ("Владивосток" vs "Владимировка"). Long names compare by their consonant
/// skeleton, which survives case endings; short and ambiguous ones must match
/// the whole word.
fn consonant_key(word: &str, limit: usize) -> String {
    word.chars()
        .filter(|c| {
            c.is_alphabetic()
                && !matches!(
                    c,
                    'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я' | 'ь' | 'ъ'
                )
        })
        .take(limit)
        .collect()
}

fn is_adjectival(word: &str) -> bool {
    [
        "ое", "ая", "яя", "ый", "ий", "ой", "ые", "ых", "ом", "ем", "ее", "ими",
    ]
    .iter()
    .any(|ending| word.ends_with(ending))
}

fn word_matches(text_word: &str, name_word: &str, multi_word_name: bool) -> bool {
    let name_length = name_word.chars().count();
    if name_length < 5 || ambiguous(name_word) {
        return text_word == name_word;
    }
    // A single adjective-like village name ("Хорошее") must match exactly, so
    // "хорошая" is not treated as that village.
    if !multi_word_name && is_adjectival(name_word) {
        return text_word == name_word;
    }
    let limit = if name_length >= 8 { 4 } else { 3 };
    let name_key = consonant_key(name_word, limit);
    let text_key = consonant_key(text_word, limit);
    !name_key.is_empty() && text_key.starts_with(&name_key)
}

fn settlement_matches(haystack: &str, name: &str) -> bool {
    let region_context = haystack.split_whitespace().any(|word| {
        matches!(
            word,
            "приморский"
                | "приморском"
                | "приморье"
                | "приморья"
                | "приморью"
                | "край"
                | "крае"
        )
    });
    if region_context && (name.contains("приморск") || name.contains("приморь")) {
        return false;
    }
    let text_words: Vec<&str> = haystack.split_whitespace().collect();
    let name_words: Vec<&str> = name.split_whitespace().collect();
    if name_words.is_empty() {
        return false;
    }
    let multi_word_name = name_words.len() > 1;
    text_words.windows(name_words.len()).any(|window| {
        window
            .iter()
            .zip(&name_words)
            .all(|(text, name)| word_matches(text, name, multi_word_name))
    })
}

/// Every settlement whose name appears in the text. Longer names win over
/// their own substrings ("Большой Камень" over "Камень").
pub fn find_all_in_text(text: &str) -> Vec<&'static Settlement> {
    let haystack = normalize(text);
    let mut matches: Vec<&Settlement> = all()
        .iter()
        .filter(|settlement| settlement_matches(&haystack, &normalize(&settlement.name)))
        .collect();
    matches.sort_by(|left, right| {
        normalize(&right.name)
            .len()
            .cmp(&normalize(&left.name).len())
    });
    let mut result: Vec<&Settlement> = Vec::new();
    for candidate in matches {
        let name = normalize(&candidate.name);
        if result
            .iter()
            .any(|kept| normalize(&kept.name).contains(&name))
        {
            continue;
        }
        result.push(candidate);
    }
    result
}

/// The single settlement named in the text; `None` for a multi-city route.
pub fn find_in_text(text: &str) -> Option<&'static Settlement> {
    let matches = find_all_in_text(text);
    if matches.len() == 1 {
        matches.first().copied()
    } else {
        None
    }
}

pub fn center(name: &str) -> Option<(f64, f64)> {
    let name = normalize(name);
    all()
        .iter()
        .find(|settlement| normalize(&settlement.name) == name)
        .map(|settlement| (settlement.lat, settlement.lon))
}

pub fn nearest(lat: f64, lon: f64) -> Option<&'static Settlement> {
    all()
        .iter()
        .map(|settlement| {
            (
                settlement,
                haversine_km((lat, lon), (settlement.lat, settlement.lon)),
            )
        })
        .filter(|(_, distance)| *distance <= 30.0)
        .min_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(settlement, _)| settlement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gazetteer_covers_primorye() {
        assert!(all().len() > 150, "settlements: {}", all().len());
        assert_eq!(
            find_all_in_text("хочу во Владивосток на выходные")
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Владивосток"]
        );
        assert_eq!(
            find_in_text("в Сергеевке погулять").map(|item| item.name.as_str()),
            Some("Сергеевка")
        );
        assert!(find_in_text("хорошая погода в приморье").is_none());
    }
}
