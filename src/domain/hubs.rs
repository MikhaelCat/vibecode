//! Primorye hub (agglomeration) resolution.
//!
//! A query about a small place must send the lodging search to the town that
//! actually has hotels: "Южно-Морской" belongs to the Nakhodka coast, not to
//! Vladivostok. Hubs group the villages, bays, capes, mountains and districts
//! of one part of the krai around a reference centre with a priority list of
//! settlements worth searching. Embedded from `data/geo/primorye_hubs.json`,
//! built the way the settlements gazetteer in `settlements.rs` is.

use std::sync::OnceLock;

use serde::Deserialize;

use crate::domain::spatial::distance_km;

#[derive(Debug, Clone, Deserialize)]
pub struct Hub {
    pub id: String,
    pub title: String,
    pub lat: f64,
    pub lon: f64,
    /// Proper names — settlements, bays, capes, mountains, districts — that
    /// select this hub.
    pub triggers: Vec<String>,
    /// Settlements whose lodging should be searched, in priority order.
    pub lodging: Vec<String>,
}

/// Place-type words that only name the kind of place, so the bare name part of
/// a trigger may be used on its own: "бухта Триозёрье" also resolves from
/// "Триозёрье". Only leading words are dropped, so "район", "заповедник" or
/// "пруды" alone can never pick a hub.
const TYPE_WORDS: [&str; 12] = [
    "бухта",
    "гора",
    "залив",
    "коса",
    "маяк",
    "мыс",
    "озеро",
    "остров",
    "пещера",
    "полуостров",
    "скала",
    "ущелье",
];

static HUBS: OnceLock<Vec<Hub>> = OnceLock::new();

pub fn all() -> &'static Vec<Hub> {
    HUBS.get_or_init(|| {
        serde_json::from_str(include_str!("../../data/geo/primorye_hubs.json")).unwrap_or_default()
    })
}

/// Lower-case word form: `ё` folds into `е` and every separator becomes a
/// single space, so "Южно-Морском" and "южно морской" are comparable.
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

/// Consonants of a word, so case endings do not break the comparison.
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

/// Whole-word equality; longer distinctive words also connect by their
/// consonant skeleton, so "в Южно-Морском" finds "Южно-Морской". Short words
/// must match exactly, and matching never happens inside a longer word:
/// "ванна" can not resolve "Анна".
fn word_matches(text_word: &str, trigger_word: &str) -> bool {
    if text_word == trigger_word {
        return true;
    }
    let length = trigger_word.chars().count();
    if length < 5 {
        return false;
    }
    let limit = if length >= 8 { 4 } else { 3 };
    let key = consonant_key(trigger_word, limit);
    !key.is_empty() && consonant_key(text_word, limit).starts_with(&key)
}

/// Best match of one trigger inside the text: how many trigger words matched
/// and whether they matched as written. Only whole word sequences count, and
/// a leading type word may be omitted.
fn match_trigger(text_words: &[&str], trigger: &str) -> Option<(usize, bool)> {
    let words: Vec<&str> = trigger.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let mut starts = vec![0];
    let mut begin = 0;
    while begin + 1 < words.len() && TYPE_WORDS.contains(&words[begin]) {
        begin += 1;
    }
    if begin > 0 {
        starts.push(begin);
    }
    let mut best: Option<(usize, bool)> = None;
    for start in starts {
        let variant = &words[start..];
        let mut matched = false;
        let mut exact = false;
        for window in text_words.windows(variant.len()) {
            let mut window_ok = true;
            let mut window_exact = true;
            for (text_word, trigger_word) in window.iter().zip(variant) {
                if text_word == trigger_word {
                    continue;
                }
                window_exact = false;
                if !word_matches(text_word, trigger_word) {
                    window_ok = false;
                    break;
                }
            }
            if window_ok {
                matched = true;
                exact = exact || window_exact;
            }
        }
        if matched {
            let candidate = (variant.len(), exact);
            if best.is_none_or(|current| candidate > current) {
                best = Some(candidate);
            }
        }
    }
    best
}

/// Hub whose trigger appears in the text. Multi-word triggers win over single
/// words, an exact name wins over an inflected one, and the longest trigger
/// wins the remaining ties. `None` when nothing matches.
pub fn find(text: &str) -> Option<&'static Hub> {
    matched_trigger(text).map(|(hub, _)| hub)
}

/// Like [`find`], but also returns the trigger that matched, so callers can
/// keep the user's own wording ("бухта Триозёрье") as the area name.
pub fn matched_trigger(text: &str) -> Option<(&'static Hub, &'static str)> {
    let haystack = normalize(text);
    if haystack.is_empty() {
        return None;
    }
    let text_words: Vec<&str> = haystack.split_whitespace().collect();
    let mut best: Option<((usize, bool, usize), usize, &'static str)> = None;
    for (index, hub) in all().iter().enumerate() {
        for trigger in &hub.triggers {
            let normalized = normalize(trigger);
            if let Some((words, exact)) = match_trigger(&text_words, &normalized) {
                let score = (words, exact, normalized.chars().count());
                if best.is_none_or(|(current, _, _)| score > current) {
                    best = Some((score, index, trigger.as_str()));
                }
            }
        }
    }
    best.and_then(|(_, index, trigger)| all().get(index).map(|hub| (hub, trigger)))
}

/// Lodging settlements to search, in priority order: the hub list when the
/// text names a hub, otherwise the settlement itself (when it is non-empty).
pub fn lodging_candidates(name: &str) -> Vec<String> {
    if let Some(hub) = find(name) {
        return hub.lodging.clone();
    }
    let settlement = name.trim();
    if settlement.is_empty() {
        Vec::new()
    } else {
        vec![settlement.to_string()]
    }
}

/// Nearest hub centre, used to suggest where to look when nothing is found.
pub fn nearest(lat: f64, lon: f64) -> Option<&'static Hub> {
    all().iter().min_by(|left, right| {
        distance_km(lat, lon, left.lat, left.lon)
            .partial_cmp(&distance_km(lat, lon, right.lat, right.lon))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

/// Primary city of a hub (its first lodging entry), for user-facing hints.
pub fn primary_city(hub: &Hub) -> Option<&'static str> {
    let stored = all().iter().find(|item| item.id == hub.id)?;
    stored.lodging.first().map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub_id(text: &str) -> Option<&'static str> {
        find(text).map(|hub| hub.id.as_str())
    }

    #[test]
    fn resolves_the_reported_bug_query() {
        assert_eq!(
            hub_id("Хочу погулять в Южно-Морском"),
            Some("nakhodka-coast")
        );
    }

    #[test]
    fn matches_a_bay_of_the_nakhodka_coast() {
        assert_eq!(hub_id("отдых в бухте Триозёрье"), Some("nakhodka-coast"));
    }

    #[test]
    fn matches_a_mountain_from_its_bare_name() {
        assert_eq!(hub_id("поездка на Пидан"), Some("partizansk-taiga"));
        assert_eq!(hub_id("гора Фалаза на выходные"), Some("partizansk-taiga"));
    }

    #[test]
    fn matches_inflected_settlements() {
        assert_eq!(hub_id("выходные в Анисимовке"), Some("partizansk-taiga"));
        assert_eq!(hub_id("в Раздольном"), Some("ussuriysk-west"));
    }

    #[test]
    fn exact_place_beats_a_longer_approximate_trigger() {
        assert_eq!(hub_id("Ливадия на два дня"), Some("nakhodka-coast"));
        assert_eq!(hub_id("на Ливадийскую"), Some("partizansk-taiga"));
    }

    #[test]
    fn lodging_starts_at_the_named_settlement() {
        let candidates = lodging_candidates("Южно-Морской");
        assert_eq!(candidates.first().map(String::as_str), Some("Южно-Морской"));
        assert!(candidates.contains(&"Находка".to_string()));
        assert!(!candidates.contains(&"Владивосток".to_string()));
        assert_eq!(hub_id("Южно-Морской"), Some("nakhodka-coast"));
    }

    #[test]
    fn lodging_for_vladivostok_is_its_own_list() {
        let hub = find("Владивосток").expect("vladivostok hub");
        assert_eq!(hub.id, "vladivostok");
        assert_eq!(lodging_candidates("Владивосток"), hub.lodging);
        assert_eq!(
            hub.lodging,
            ["Владивосток".to_string(), "Артём".to_string()]
        );
    }

    #[test]
    fn unknown_text_falls_back_to_the_name_itself() {
        assert!(find("Абракадабра").is_none());
        assert_eq!(
            lodging_candidates("Абракадабра"),
            ["Абракадабра".to_string()]
        );
        assert!(lodging_candidates("   ").is_empty());
    }

    #[test]
    fn short_words_do_not_match_inside_other_words() {
        assert!(find("ванна").is_none(), "ванна must not resolve Анна");
        assert_eq!(hub_id("поедем в Хасан в мае"), Some("khasan-sea"));
    }

    #[test]
    fn every_hub_is_well_formed() {
        assert_eq!(all().len(), 11);
        for hub in all() {
            assert!(!hub.id.is_empty(), "id");
            assert!(!hub.title.is_empty(), "{}: title", hub.id);
            assert!(hub.lat != 0.0 && hub.lon != 0.0, "{}: centre", hub.id);
            assert!(!hub.triggers.is_empty(), "{}: triggers", hub.id);
            assert!(!hub.lodging.is_empty(), "{}: lodging", hub.id);
            assert!(hub
                .triggers
                .iter()
                .all(|trigger| !trigger.trim().is_empty()));
            assert!(hub.lodging.iter().all(|place| !place.trim().is_empty()));
        }
    }

    #[test]
    fn nearest_hub_suggests_the_right_centre() {
        assert_eq!(
            nearest(42.85, 132.80).map(|hub| hub.id.as_str()),
            Some("nakhodka-coast")
        );
        assert_eq!(
            nearest(43.11, 131.88).map(|hub| hub.id.as_str()),
            Some("vladivostok")
        );
        assert!(nearest(0.0, 0.0).is_some());
    }

    #[test]
    fn primary_city_is_the_first_lodging_entry() {
        let hub = find("Хочу погулять в Южно-Морском").expect("hub");
        assert_eq!(primary_city(hub), Some("Южно-Морской"));
    }
}
