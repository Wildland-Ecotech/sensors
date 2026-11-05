//! Faulty readings: values a sensor reports when its hardware misbehaves,
//! such as RF interference or condensation on the circuit, rather than what
//! it measured. They are found while the readings are built, kept in
//! `reading_faults`, and left out of every line, mean, lowest and highest.
//!
//! A reading is faulty when it is:
//!
//! - **Out of range:** outside what the measure can plausibly be in the
//!   field. A temperature sensor that fails often reports the end of its
//!   scale, such as -50 °C or 150 °C, which is -58 °F or 302 °F.
//! - **A spike:** further than the measure's spike size from the median of
//!   itself and its neighbors: up to two readings each side within three
//!   minutes, as many on each side, and at least one. A reading that jumps away and
//!   comes straight back is a spike; a change that lasts, such as a heater
//!   turning on, is not, since the readings after it agree with it.
//!
//! Only measures with a rule are checked. Changing a rule takes effect after
//! `sensors analyze --rebuild`.

/// What a measure can plausibly be, and how far a reading may jump away from
/// its neighbors and back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub low: f64,
    pub high: f64,
    pub spike: f64,
}

/// The rule for a measure, if it has one.
pub fn rule(measure: &str) -> Option<Rule> {
    match measure {
        // °F: beyond the coldest and hottest a greenhouse sees.
        "temp" => Some(Rule {
            low: -40.0,
            high: 140.0,
            spike: 10.0,
        }),
        // % relative humidity. 100 is real: saturated air.
        "humid" => Some(Rule {
            low: 0.0,
            high: 100.0,
            spike: 15.0,
        }),
        _ => None,
    }
}

/// Why a reading is faulty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Range,
    Spike,
}

impl Reason {
    /// As stored in `reading_faults`.
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Range => "range",
            Reason::Spike => "spike",
        }
    }

    /// For people, such as `out of range`.
    pub fn describe(reason: &str) -> &'static str {
        match reason {
            "range" => "out of range",
            "spike" => "spike",
            _ => "faulty",
        }
    }
}

/// How far apart, in milliseconds, a reading's neighbors may be.
const NEIGHBOR_SPAN: i64 = 3 * 60_000;

/// Neighbors on each side counted towards the median.
const NEIGHBORS: usize = 2;

/// The faulty readings among `readings`, which are one series' readings
/// oldest first, checking only those at or after `from`. Readings before
/// `from` count as neighbors. Out of range readings never count as
/// neighbors, so they cannot hide a spike.
pub fn detect(readings: &[(i64, f64)], from: i64, rule: &Rule) -> Vec<(i64, f64, Reason)> {
    let in_range: Vec<bool> = readings
        .iter()
        .map(|&(_, value)| value >= rule.low && value <= rule.high)
        .collect();
    let mut faults = Vec::new();
    for (i, &(at, value)) in readings.iter().enumerate() {
        if at < from {
            continue;
        }
        if !in_range[i] {
            faults.push((at, value, Reason::Range));
            continue;
        }
        let mut before: Vec<f64> = (0..i)
            .rev()
            .filter(|&j| in_range[j])
            .take_while(|&j| at - readings[j].0 <= NEIGHBOR_SPAN)
            .take(NEIGHBORS)
            .map(|j| readings[j].1)
            .collect();
        let mut after: Vec<f64> = (i + 1..readings.len())
            .filter(|&j| in_range[j])
            .take_while(|&j| readings[j].0 - at <= NEIGHBOR_SPAN)
            .take(NEIGHBORS)
            .map(|j| readings[j].1)
            .collect();
        // As many on each side, so the window is centered on the reading.
        let each = before.len().min(after.len());
        if each == 0 {
            continue;
        }
        before.truncate(each);
        after.truncate(each);
        let mut window: Vec<f64> = before.into_iter().chain(after).collect();
        window.push(value);
        if (value - median(&mut window)).abs() > rule.spike {
            faults.push((at, value, Reason::Spike));
        }
    }
    faults
}

/// The middle of an odd number of values.
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMP: Rule = Rule {
        low: -40.0,
        high: 140.0,
        spike: 10.0,
    };

    /// Readings 30 seconds apart.
    fn every_30s(values: &[f64]) -> Vec<(i64, f64)> {
        values
            .iter()
            .enumerate()
            .map(|(i, &value)| (i as i64 * 30_000, value))
            .collect()
    }

    fn found(readings: &[(i64, f64)]) -> Vec<(f64, Reason)> {
        detect(readings, 0, &TEMP)
            .into_iter()
            .map(|(_, value, reason)| (value, reason))
            .collect()
    }

    #[test]
    fn flags_readings_out_of_range() {
        let readings = every_30s(&[67.4, 302.0, 67.5, -58.0, 67.6]);
        assert_eq!(
            found(&readings),
            [(302.0, Reason::Range), (-58.0, Reason::Range)]
        );
        assert_eq!(rule("humid").unwrap().high, 100.0);
        assert_eq!(rule("moisture"), None);
    }

    #[test]
    fn flags_spikes_that_come_straight_back() {
        let one = every_30s(&[67.4, 67.5, 95.0, 67.6, 67.5]);
        assert_eq!(found(&one), [(95.0, Reason::Spike)]);
        let two = every_30s(&[67.4, 67.5, 95.0, 96.0, 67.6, 67.5]);
        assert_eq!(found(&two), [(95.0, Reason::Spike), (96.0, Reason::Spike)]);
        let down = every_30s(&[67.4, 67.5, 30.0, 67.6]);
        assert_eq!(found(&down), [(30.0, Reason::Spike)]);
    }

    #[test]
    fn leaves_changes_that_last_alone() {
        // A heater turning on: the readings after the jump agree with it.
        let step = every_30s(&[60.0, 60.1, 75.0, 75.2, 75.1, 75.3]);
        assert_eq!(found(&step), []);
        // Small wobbles are not spikes.
        let wobble = every_30s(&[67.0, 70.0, 66.0, 71.0, 67.0]);
        assert_eq!(found(&wobble), []);
        // The newest reading waits for a neighbor after it.
        let newest = every_30s(&[67.4, 67.5, 95.0]);
        assert_eq!(found(&newest), []);
    }

    #[test]
    fn needs_neighbors_close_in_time() {
        // Ten minutes on each side: no neighbors, so no spike.
        let apart = [(0, 67.0), (600_000, 95.0), (1_200_000, 67.0)];
        assert_eq!(found(&apart), []);
    }

    #[test]
    fn checks_only_from_the_given_time_but_uses_earlier_neighbors() {
        let readings = every_30s(&[67.4, 95.0, 67.5, 67.6, 99.0, 67.4]);
        let later: Vec<f64> = detect(&readings, 3 * 30_000, &TEMP)
            .into_iter()
            .map(|(_, value, _)| value)
            .collect();
        assert_eq!(later, [99.0]);
    }

    #[test]
    fn out_of_range_readings_do_not_hide_a_spike() {
        let readings = every_30s(&[67.4, 302.0, 95.0, 302.0, 67.5]);
        assert_eq!(
            found(&readings),
            [
                (302.0, Reason::Range),
                (95.0, Reason::Spike),
                (302.0, Reason::Range)
            ]
        );
    }
}
