//! Local time without a timezone database: wttr.in gives the city's UTC
//! offset ("18:47:32-0800") and the HTTP Date header the UTC time.
use std::time::Instant;

#[derive(Clone)]
pub struct Glance {
    pub utc: Option<i64>, // seconds since 1970, when the answer was sent
    pub offset: i64,      // seconds east of UTC at the city
    pub at: Instant,
    pub temp: String,
    pub sky: String,
    pub city: String,
}

pub struct Local {
    pub hour: u32,
    pub minute: u32,
    pub weekday: usize, // 0 = Monday
    pub month: usize,   // 1..=12
    pub day: u32,
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

impl Local {
    pub fn date(&self) -> String {
        format!("{}, {} {}", DAYS[self.weekday], MONTHS[self.month - 1], self.day)
    }
}

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

impl Glance {
    pub fn now(&self) -> Option<Local> {
        let t = self.utc? + self.at.elapsed().as_secs() as i64 + self.offset;
        let days = t.div_euclid(86_400);
        let secs = t.rem_euclid(86_400);
        let (_, month, day) = civil(days);
        Some(Local {
            hour: (secs / 3600) as u32,
            minute: (secs / 60 % 60) as u32,
            weekday: (days + 3).rem_euclid(7) as usize, // 1970-01-01 was a Thursday
            month: month as usize,
            day,
        })
    }
}

/// "Tue, 24 Nov 2026 02:47:33 GMT" to seconds since 1970.
pub fn parse_http_date(s: &str) -> Option<i64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() < 5 {
        return None;
    }
    let day: u32 = p[1].parse().ok()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == p[2])? as u32 + 1;
    let year: i64 = p[3].parse().ok()?;
    let hms: Vec<i64> = p[4].split(':').filter_map(|v| v.parse().ok()).collect();
    if hms.len() != 3 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hms[0] * 3600 + hms[1] * 60 + hms[2])
}

/// The offset at the end of "18:47:32-0800", in seconds.
pub fn parse_offset(t: &str) -> i64 {
    let t = t.trim();
    if t.len() < 5 {
        return 0;
    }
    let z = &t[t.len() - 5..];
    let (h, m) = (z[1..3].parse::<i64>().unwrap_or(0), z[3..5].parse::<i64>().unwrap_or(0));
    let v = h * 3600 + m * 60;
    if z.starts_with('-') {
        -v
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dates() {
        let t = parse_http_date("Tue, 24 Nov 2026 02:47:33 GMT").unwrap();
        let g = Glance { utc: Some(t), offset: parse_offset("18:47:32-0800"), at: Instant::now(), temp: String::new(), sky: String::new(), city: String::new() };
        let l = g.now().unwrap();
        assert_eq!((l.hour, l.minute), (18, 47));
        assert_eq!(l.date(), "Monday, November 23");
    }
}
