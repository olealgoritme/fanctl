//! /etc/fanctl.toml: curves for `fanctl run`.
//!
//! ```toml
//! interval_ms = 1000
//! fail_pct = 100            # a sensor that cannot be read, and on exit
//!
//! [[curve]]
//! name = "radiator"
//! controls = ["lianli/*"]
//! sensors = ["x53/temp1"]   # the hottest of these counts
//! points = [[30, 30], [35, 45], [40, 70], [44, 100]]   # [temperature C, duty %]
//! min_pct = 25
//! down_per_s = 3            # ramp down at most this many % per second; up is immediate
//!
//! [[critical]]
//! sensor = "k10temp/Tctl"
//! above = 90                # every curve's controls to 100 % while this holds
//! ```

use serde::Deserialize;

fn d_interval() -> u64 {
    1000
}
fn d_fail() -> f64 {
    100.0
}
fn d_down() -> f64 {
    3.0
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "d_interval")]
    pub interval_ms: u64,
    #[serde(default = "d_fail")]
    pub fail_pct: f64,
    #[serde(default)]
    pub curve: Vec<Curve>,
    #[serde(default)]
    pub critical: Vec<Critical>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Curve {
    #[serde(default)]
    pub name: String,
    pub controls: Vec<String>,
    pub sensors: Vec<String>,
    pub points: Vec<[f64; 2]>,
    #[serde(default)]
    pub min_pct: f64,
    #[serde(default = "d_down")]
    pub down_per_s: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Critical {
    pub sensor: String,
    pub above: f64,
}

impl Config {
    pub fn load(path: &str) -> Result<Config, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let mut c: Config = toml::from_str(&s).map_err(|e| format!("{path}: {e}"))?;
        for cv in &mut c.curve {
            if cv.points.is_empty() {
                return Err(format!("{path}: curve '{}' has no points", cv.name));
            }
            cv.points.sort_by(|a, b| a[0].total_cmp(&b[0]));
        }
        Ok(c)
    }
}

impl Curve {
    /// Linear between the points, flat beyond the first and the last.
    pub fn duty(&self, t: f64) -> f64 {
        let p = &self.points;
        let d = if t <= p[0][0] {
            p[0][1]
        } else if t >= p[p.len() - 1][0] {
            p[p.len() - 1][1]
        } else {
            let i = p.windows(2).position(|w| t >= w[0][0] && t <= w[1][0]).unwrap();
            let (a, b) = (p[i], p[i + 1]);
            if b[0] == a[0] { b[1] } else { a[1] + (b[1] - a[1]) * (t - a[0]) / (b[0] - a[0]) }
        };
        d.max(self.min_pct).clamp(0.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> Curve {
        Curve {
            name: "t".into(),
            controls: vec![],
            sensors: vec![],
            points: vec![[30.0, 30.0], [40.0, 70.0], [44.0, 100.0]],
            min_pct: 35.0,
            down_per_s: 3.0,
        }
    }

    #[test]
    fn interpolates_and_clamps() {
        let c = curve();
        assert_eq!(c.duty(20.0), 35.0);
        assert_eq!(c.duty(35.0), 50.0);
        assert_eq!(c.duty(42.0), 85.0);
        assert_eq!(c.duty(60.0), 100.0);
    }

    #[test]
    fn parses_the_documented_example() {
        let s = "interval_ms = 500\n[[curve]]\ncontrols = [\"lianli/*\"]\nsensors = [\"x53/temp1\"]\npoints = [[40, 70], [30, 30]]\n[[critical]]\nsensor = \"k10temp/Tctl\"\nabove = 90\n";
        let c: Config = toml::from_str(s).unwrap();
        assert_eq!(c.interval_ms, 500);
        assert_eq!(c.fail_pct, 100.0);
        assert_eq!(c.critical[0].above, 90.0);
        assert!(toml::from_str::<Config>("bogus = 1").is_err());
    }
}
