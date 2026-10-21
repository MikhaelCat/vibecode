use crate::domain::intent::TransportMode;

/// Primorsky Krai bounding box: south, west, north, east.
pub const PRIMORYE_BBOX: (f64, f64, f64, f64) = (42.2, 130.0, 48.5, 139.1);

pub fn in_primorye(lat: f64, lon: f64) -> bool {
    let (south, west, north, east) = PRIMORYE_BBOX;
    lat >= south && lat <= north && lon >= west && lon <= east
}

/// True when a title names a water area rather than a point on land.
/// Such objects are useful as search areas, but a centroid marker in the sea
/// or in a lake looks wrong, so providers never return them as itinerary
/// stops. Shore objects (beaches, capes, viewpoints, settlements) stay.
pub fn waterbody_title(title: &str) -> bool {
    let lower = title.to_lowercase();
    // Wikipedia disambiguates water articles in parentheses:
    // "Находка (бухта, Японское море)", "Андреевка (река, ...)".
    if let Some(open) = lower.find('(') {
        let descriptor = lower[open..]
            .split(|c: char| !c.is_alphabetic())
            .filter(|word| !word.is_empty());
        if descriptor.into_iter().any(is_water_word) {
            return true;
        }
    }
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    // The head of a water name is either first ("Бухта Новик", "Озеро Ханка")
    // or last ("Японское море"). A land object that merely mentions water
    // ("Смотровая площадка «Бухта Находка»", "Морской вокзал") stays.
    words.first().is_some_and(|word| is_water_word(word))
        || words.last().is_some_and(|word| is_water_word(word))
}

fn is_water_word(word: &str) -> bool {
    word.starts_with("бухт")
        || matches!(
            word,
            "залив"
                | "залива"
                | "заливе"
                | "заливом"
                | "заливы"
                | "заливов"
                | "заливам"
                | "заливами"
                | "заливах"
        )
        || word.starts_with("озер")
        || word.starts_with("озёр")
        || matches!(
            word,
            "река"
                | "реки"
                | "реке"
                | "реку"
                | "рекой"
                | "рекою"
                | "рек"
                | "рекам"
                | "реками"
                | "реках"
        )
        || matches!(
            word,
            "море" | "моря" | "морю" | "морем" | "морям" | "морями" | "морях"
        )
        || word.starts_with("водохранилищ")
        || word.starts_with("пруд")
        || word.starts_with("пролив")
        || word.starts_with("ковш")
        || word.starts_with("лиман")
        || word.starts_with("затон")
        || word.starts_with("стариц")
        || word.starts_with("проток")
        || word.starts_with("канал")
}

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    const R: f64 = 6371.0;
    let d_lat = (b.0 - a.0).to_radians();
    let d_lon = (b.1 - a.1).to_radians();
    let lat1 = a.0.to_radians();
    let lat2 = b.0.to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (d_lon / 2.0).sin().powi(2);
    2.0 * R * h.sqrt().asin()
}

pub fn travel_minutes(distance_km: f64, mode: TransportMode) -> u32 {
    let speed = match mode {
        TransportMode::Car => 55.0,
        TransportMode::Foot => 4.5,
        TransportMode::Bike => 15.0,
        TransportMode::Transit => 35.0,
    };
    ((distance_km / speed) * 60.0).ceil().max(1.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vladivostok_is_inside() {
        assert!(in_primorye(43.1155, 131.8855));
        assert!(!in_primorye(43.5855, 39.7231)); // Sochi
    }

    #[test]
    fn haversine_positive() {
        let km = haversine_km((43.1155, 131.8855), (43.8, 131.95));
        assert!(km > 70.0 && km < 90.0, "km={km}");
    }

    #[test]
    fn waterbody_titles_are_not_stops() {
        assert!(waterbody_title("Находка (бухта, Японское море)"));
        assert!(waterbody_title("Солёное (озеро, Находка)"));
        assert!(waterbody_title("озеро Ханка"));
        assert!(waterbody_title("река Раздольная"));
        assert!(!waterbody_title("Пляж Андреевки"));
        assert!(!waterbody_title("Морской вокзал"));
        assert!(!waterbody_title("Мыс Тобизина"));
        assert!(!waterbody_title("Смотровая площадка «Бухта Находка»"));
    }
}
