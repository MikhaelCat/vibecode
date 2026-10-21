use crate::domain::intent::TransportMode;

/// Primorsky Krai bounding box: south, west, north, east.
pub const PRIMORYE_BBOX: (f64, f64, f64, f64) = (42.2, 130.0, 48.5, 139.1);

pub fn in_primorye(lat: f64, lon: f64) -> bool {
    let (south, west, north, east) = PRIMORYE_BBOX;
    lat >= south && lat <= north && lon >= west && lon <= east
}

pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    const R: f64 = 6371.0;
    let d_lat = (b.0 - a.0).to_radians();
    let d_lon = (b.1 - a.1).to_radians();
    let lat1 = a.0.to_radians();
    let lat2 = b.0.to_radians();
    let h = (d_lat / 2.0).sin().powi(2)
        + lat1.cos() * lat2.cos() * (d_lon / 2.0).sin().powi(2);
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
}
