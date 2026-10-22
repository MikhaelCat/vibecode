//! Primorye settlements gazetteer.
//!
//! Generated once from OSM (`place=city|town|village|hamlet`) and embedded as
//! `data/geo/primorye_settlements.json`. Used to recognise the city or village
//! named in a free-form query without a hand-written short list.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::domain::spatial::KdTree;

#[derive(Debug, Clone, Deserialize)]
pub struct Settlement {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    /// OSM place type: `city`, `town`, `village` or `hamlet`.
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "village".to_string()
}

impl Settlement {
    /// Search radius around the settlement centre. A city point of interest can
    /// sit 20 km from the centre, while a village must not drag in the next
    /// town: a stop is only "in this place" when it is inside this circle.
    pub fn scope_km(&self) -> f64 {
        match self.kind.as_str() {
            "city" => 25.0,
            "town" => 15.0,
            "hamlet" => 6.0,
            _ => 9.0,
        }
    }
}

static SETTLEMENTS: OnceLock<Vec<Settlement>> = OnceLock::new();
/// Spatial index over the gazetteer, built once. Nearest lookups stay O(log n)
/// even though every plan point asks for its city after the plan is built.
static SETTLEMENTS_TREE: OnceLock<KdTree> = OnceLock::new();

fn all() -> &'static Vec<Settlement> {
    SETTLEMENTS.get_or_init(|| {
        serde_json::from_str(include_str!("../../data/geo/primorye_settlements.json"))
            .unwrap_or_default()
    })
}

fn tree() -> &'static KdTree {
    SETTLEMENTS_TREE.get_or_init(|| {
        let points: Vec<(f64, f64)> = all()
            .iter()
            .map(|settlement| (settlement.lat, settlement.lon))
            .collect();
        KdTree::new(&points)
    })
}

pub fn normalize(value: &str) -> String {
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

/// Positions of the consonants inside a word, counted in letters.
pub(crate) fn consonant_positions(word: &str) -> Vec<usize> {
    word.chars()
        .filter(|character| character.is_alphabetic())
        .enumerate()
        .filter(|(_, character)| {
            !matches!(
                character,
                'а' | 'е' | 'ё' | 'и' | 'о' | 'у' | 'ы' | 'э' | 'ю' | 'я' | 'ь' | 'ъ'
            )
        })
        .map(|(index, _)| index)
        .collect()
}

/// The consonant skeleton alone is not enough: "Посьет" and "поесть" share the
/// skeleton "пст", so a user asking for "поесть морепродукты" was sent to the
/// village Посьет. Case endings move only the *last* consonant, so every
/// earlier consonant must sit in the same place: "Сергеевка"/"Сергеевке" agree,
/// "Посьет"/"поесть" do not.
pub(crate) fn consonants_align(name_positions: &[usize], text_positions: &[usize]) -> bool {
    let shared = name_positions.len().min(text_positions.len());
    if shared < 2 {
        return true;
    }
    let compared = shared - 1;
    name_positions[..compared] == text_positions[..compared]
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
    if name_key.is_empty() || !text_key.starts_with(&name_key) {
        return false;
    }
    consonants_align(
        &consonant_positions(name_word),
        &consonant_positions(text_word),
    )
}

fn settlement_matches(haystack: &str, name: &str) -> bool {
    if mentions_region(haystack) && (name.contains("приморск") || name.contains("приморь"))
    {
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

/// True when the text talks about the region itself ("по Приморью",
/// "приморский край"), not about the village Приморский in Khasansky district.
/// Expects text already passed through [`normalize`].
pub fn mentions_region(normalized: &str) -> bool {
    let words: Vec<&str> = normalized.split_whitespace().collect();
    let has_word = |list: &[&str]| words.iter().any(|word| list.contains(word));
    if has_word(&["приморье", "приморья", "приморью", "приморьем"])
    {
        return true;
    }
    has_word(&["приморский", "приморском", "приморского", "приморскому"])
        && has_word(&["край", "крае", "краю", "края", "краем"])
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

/// Resolves an `area` argument ("Уссурийск", "село Сергеевка", "Приморский")
/// to a gazetteer entry: exact name first, then the fuzzy in-text match.
pub fn resolve(area: &str) -> Option<&'static Settlement> {
    let normalized = normalize(area);
    if normalized.is_empty() {
        return None;
    }
    if let Some(settlement) = all()
        .iter()
        .find(|settlement| normalize(&settlement.name) == normalized)
    {
        return Some(settlement);
    }
    find_in_text(area)
}

/// Search radius for a settlement name, so callers can scope results to the
/// place the user asked for. Unknown names fall back to a generic town radius.
pub fn scope_km(name: &str) -> f64 {
    let normalized = normalize(name);
    all()
        .iter()
        .find(|settlement| normalize(&settlement.name) == normalized)
        .map(Settlement::scope_km)
        .unwrap_or(15.0)
}

pub fn nearest(lat: f64, lon: f64) -> Option<&'static Settlement> {
    nearest_within(lat, lon, 30.0)
}

/// Nearest settlement inside `max_km`. Lakes and other large areas use a wider
/// reach to find the village on their shore.
pub fn nearest_within(lat: f64, lon: f64, max_km: f64) -> Option<&'static Settlement> {
    let (index, distance) = tree().nearest(lat, lon, 1).into_iter().next()?;
    if distance > max_km {
        return None;
    }
    all().get(index)
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

    #[test]
    fn village_primorsky_is_a_place_not_the_region() {
        assert_eq!(
            find_in_text("поехали в Приморский").map(|item| item.name.as_str()),
            Some("Приморский")
        );
        assert_eq!(
            find_in_text("на выходные в приморский край").map(|item| item.name.as_str()),
            None
        );
        assert_eq!(
            resolve("Приморский").map(|item| item.name.as_str()),
            Some("Приморский")
        );
    }

    #[test]
    fn scopes_are_tighter_for_small_places() {
        assert!(scope_km("Владивосток") > scope_km("Сергеевка"));
        assert!(scope_km("Владивосток") <= 25.0);
        assert!(scope_km("Сергеевка") >= 5.0);
    }

    #[test]
    fn resolve_accepts_cases_and_prefixes() {
        assert_eq!(
            resolve("в Кировский на выходные").map(|item| item.name.as_str()),
            Some("Кировский")
        );
        assert_eq!(
            resolve("Кавалерово").map(|item| item.name.as_str()),
            Some("Кавалерово")
        );
        assert!(resolve("Атлантида").is_none());
    }

    #[test]
    fn common_words_are_not_settlements() {
        // "поесть" shares the consonant skeleton "пст" with "Посьет".
        assert!(find_in_text("хочу на выходные к морю, погулять и поесть морепродукты").is_none());
        assert!(find_in_text("поесть и погулять").is_none());
        // Inflected settlement names still match.
        assert_eq!(
            find_in_text("в Сергеевке погулять").map(|item| item.name.as_str()),
            Some("Сергеевка")
        );
        assert_eq!(
            find_in_text("поездка во Владивосток").map(|item| item.name.as_str()),
            Some("Владивосток")
        );
        assert_eq!(
            find_in_text("вернуться из Владивостока").map(|item| item.name.as_str()),
            Some("Владивосток")
        );
    }
}
