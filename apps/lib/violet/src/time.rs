//! Local time for the cities Settings offers, from UTC and a daylight-saving
//! rule per city, with no network and no time-zone database on the phone.
//!
//! Each city has a POSIX TZ string ("PST8PDT,M3.2.0,M11.1.0": Pacific time,
//! eight hours west, daylight time from the second Sunday of March to the
//! first Sunday of November). The rules are the ones in force when this was
//! written: every city's offset matched tzdata 2026f every three hours from
//! 2026 to 2031 (the `dump` test writes the table that was compared). A
//! country that changes its rule needs a new image.
//! Only the "Mm.w.d" form of a rule is understood, which is all these need.
use crate::CITIES;

/// The rule for each of `CITIES`, in the same order.
pub const ZONES: [&str; 20] = [
    "PST8PDT,M3.2.0,M11.1.0",        // San Francisco
    "PST8PDT,M3.2.0,M11.1.0",        // Oakland
    "PST8PDT,M3.2.0,M11.1.0",        // Los Angeles
    "PST8PDT,M3.2.0,M11.1.0",        // Seattle
    "EST5EDT,M3.2.0,M11.1.0",        // New York
    "CST6CDT,M3.2.0,M11.1.0",        // Chicago
    "CST6CDT,M3.2.0,M11.1.0",        // Austin
    "EST5EDT,M3.2.0,M11.1.0",        // Miami
    "MST7MDT,M3.2.0,M11.1.0",        // Denver
    "EST5EDT,M3.2.0,M11.1.0",        // Boston
    "HST10",                         // Honolulu
    "AKST9AKDT,M3.2.0,M11.1.0",      // Anchorage
    "CST6",                          // Mexico City
    "GMT0BST,M3.5.0/1,M10.5.0",      // London
    "CET-1CEST,M3.5.0,M10.5.0/3",    // Paris
    "CET-1CEST,M3.5.0,M10.5.0/3",    // Berlin
    "JST-9",                         // Tokyo
    "AEST-10AEDT,M10.1.0,M4.1.0/3",  // Sydney
    "IST-5:30",                      // Mumbai
    "EET-2EEST,M4.5.5/0,M10.5.4/24", // Cairo
];

/// The rule for the city with this index in `CITIES` (the setting time.city).
pub fn zone(city: usize) -> &'static str {
    ZONES[city.min(CITIES.len() - 1)]
}

/// Days since 1970-01-01 to (year, month, day) (Howard Hinnant's algorithm).
pub fn civil(days: i64) -> (i64, u32, u32) {
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

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "Mm.w.d[/time]": day d (0 Sunday) of week w (5: the last) of month m, at
/// `time` seconds after local midnight.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Change {
    month: u32,
    week: u32,
    day: u32,
    time: i64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Zone<'a> {
    name: &'a str,
    /// Seconds east of UTC.
    offset: i64,
    /// Daylight time: its name, offset, and when it starts and ends.
    dst: Option<(&'a str, i64, Change, Change)>,
}

struct Parser<'a>(&'a str);
impl<'a> Parser<'a> {
    fn eat(&mut self, c: char) -> bool {
        match self.0.strip_prefix(c) {
            Some(rest) => {
                self.0 = rest;
                true
            }
            None => false,
        }
    }
    /// A zone name: three or more letters, or anything in angle brackets.
    fn name(&mut self) -> Option<&'a str> {
        if self.eat('<') {
            let end = self.0.find('>')?;
            let name = &self.0[..end];
            self.0 = &self.0[end + 1..];
            return (!name.is_empty()).then_some(name);
        }
        let end = self.0.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(self.0.len());
        let name = &self.0[..end];
        self.0 = &self.0[end..];
        (end >= 3).then_some(name)
    }
    fn number(&mut self, max: i64) -> Option<i64> {
        let end = self.0.find(|c: char| !c.is_ascii_digit()).unwrap_or(self.0.len());
        if end == 0 || end > 3 {
            return None;
        }
        let n: i64 = self.0[..end].parse().ok()?;
        self.0 = &self.0[end..];
        (n <= max).then_some(n)
    }
    /// "[+-]hh[:mm[:ss]]" as seconds.
    fn hms(&mut self, max_hours: i64) -> Option<i64> {
        let negative = if self.eat('-') { true } else { self.eat('+') && false };
        let mut t = self.number(max_hours)? * 3600;
        if self.eat(':') {
            t += self.number(59)? * 60;
            if self.eat(':') {
                t += self.number(59)?;
            }
        }
        Some(if negative { -t } else { t })
    }
    fn change(&mut self) -> Option<Change> {
        if !self.eat('M') {
            return None;
        }
        let month = self.number(12).filter(|m| *m >= 1)? as u32;
        if !self.eat('.') {
            return None;
        }
        let week = self.number(5).filter(|w| *w >= 1)? as u32;
        if !self.eat('.') {
            return None;
        }
        let day = self.number(6)? as u32;
        let time = if self.eat('/') { self.hms(167)? } else { 7200 };
        Some(Change { month, week, day, time })
    }
}

fn parse(tz: &str) -> Option<Zone<'_>> {
    let mut p = Parser(tz);
    let name = p.name()?;
    // POSIX offsets count west of Greenwich as positive.
    let offset = -p.hms(24)?;
    if p.0.is_empty() {
        return Some(Zone { name, offset, dst: None });
    }
    let dst_name = p.name()?;
    let dst_offset = if p.0.starts_with(',') { offset + 3600 } else { -p.hms(24)? };
    if !p.eat(',') {
        return None; // daylight time without a rule
    }
    let start = p.change()?;
    if !p.eat(',') {
        return None;
    }
    let end = p.change()?;
    p.0.is_empty().then_some(Zone { name, offset, dst: Some((dst_name, dst_offset, start, end)) })
}

fn month_days(year: i64, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The local wall-clock second (as if UTC) at which a change happens in `year`.
fn change_at(c: Change, year: i64) -> i64 {
    let first = days_from_civil(year, c.month, 1);
    let weekday = (first + 4).rem_euclid(7) as u32; // 0 Sunday; 1970-01-01 was a Thursday
    let mut day = 1 + (c.day + 7 - weekday) % 7 + (c.week - 1) * 7;
    if day > month_days(year, c.month) {
        day -= 7;
    }
    (first + day as i64 - 1) * 86_400 + c.time
}

/// Where a zone stands at one instant.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Offset<'a> {
    /// Seconds east of UTC.
    pub seconds: i64,
    pub daylight: bool,
    /// "PST", "CEST".
    pub name: &'a str,
}

/// The offset of zone `tz` (a POSIX TZ string) at `utc` seconds since 1970.
/// None if the string is not one this understands.
pub fn offset(tz: &str, utc: i64) -> Option<Offset<'_>> {
    let z = parse(tz)?;
    let standard = Offset { seconds: z.offset, daylight: false, name: z.name };
    let Some((name, dst_offset, start, end)) = z.dst else { return Some(standard) };
    let (year, _, _) = civil((utc + z.offset).div_euclid(86_400));
    // Daylight time starts at a standard-time wall clock and ends at a daylight one.
    let start = change_at(start, year) - z.offset;
    let end = change_at(end, year) - dst_offset;
    let daylight = if start < end { start <= utc && utc < end } else { !(end <= utc && utc < start) };
    Some(if daylight { Offset { seconds: dst_offset, daylight: true, name } } else { standard })
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// A wall-clock time in some zone.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Local<'a> {
    pub year: i64,
    pub month: u32, // 1..=12
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: usize, // 0 = Monday
    pub offset: Offset<'a>,
}

impl Local<'_> {
    /// "Nov 23, Mon"
    pub fn date(&self) -> String {
        format!("{} {}, {}", MONTHS[self.month as usize - 1], self.day, DAYS[self.weekday])
    }
    /// "18:47"
    pub fn time(&self) -> String {
        format!("{}:{:02}", self.hour, self.minute)
    }
    /// "PST (UTC-8)", "IST (UTC+5:30)"
    pub fn zone(&self) -> String {
        let s = self.offset.seconds;
        let (h, m) = (s.abs() / 3600, s.abs() / 60 % 60);
        let sign = if s < 0 { '-' } else { '+' };
        if m == 0 {
            format!("{} (UTC{}{})", self.offset.name, sign, h)
        } else {
            format!("{} (UTC{}{}:{:02})", self.offset.name, sign, h, m)
        }
    }
}

/// `utc` (seconds since 1970) as local time in zone `tz`.
pub fn local(tz: &str, utc: i64) -> Option<Local<'_>> {
    let offset = offset(tz, utc)?;
    let t = utc + offset.seconds;
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let (year, month, day) = civil(days);
    Some(Local {
        year,
        month,
        day,
        hour: (secs / 3600) as u32,
        minute: (secs / 60 % 60) as u32,
        second: (secs % 60) as u32,
        weekday: (days + 3).rem_euclid(7) as usize,
        offset,
    })
}

/// The system clock, in seconds since 1970.
pub fn now_utc() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// The system clock was set from the network ("time" in /dev/violet/sys) and
/// how many seconds ago. Until then it reads the image's build time plus the
/// time since boot, which is not the time of day.
pub fn network_time() -> Option<u64> {
    let f = crate::fields(&crate::sys("time"));
    if f.get("synced").is_some_and(|v| v == "1") {
        return Some(f.get("age").and_then(|v| v.parse().ok()).unwrap_or(0));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i64, m: u32, d: u32, h: i64, min: i64) -> i64 {
        days_from_civil(y, m, d) * 86_400 + h * 3600 + min * 60
    }
    fn hours(city: &str, t: i64) -> (f64, bool) {
        let i = CITIES.iter().position(|c| *c == city).unwrap();
        let o = offset(zone(i), t).unwrap();
        (o.seconds as f64 / 3600.0, o.daylight)
    }

    #[test]
    fn every_city_has_a_rule_that_parses() {
        assert_eq!(ZONES.len(), CITIES.len());
        for (i, tz) in ZONES.iter().enumerate() {
            assert!(parse(tz).is_some(), "{}: {tz}", CITIES[i]);
            assert!(local(tz, 1_795_000_000).is_some());
        }
        assert_eq!(zone(999), ZONES[19]);
    }

    #[test]
    fn calendar() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(days_from_civil(2028, 2, 29)), (2028, 2, 29));
        assert_eq!(days_from_civil(2026, 11, 24), 20_781);
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn los_angeles_2026() {
        // Spring forward: 02:00 PST on 8 March is 10:00 UTC.
        assert_eq!(hours("Los Angeles", utc(2026, 3, 8, 9, 59)), (-8.0, false));
        assert_eq!(hours("Los Angeles", utc(2026, 3, 8, 10, 0)), (-7.0, true));
        // Fall back: 02:00 PDT on 1 November is 09:00 UTC.
        assert_eq!(hours("Los Angeles", utc(2026, 11, 1, 8, 59)), (-7.0, true));
        assert_eq!(hours("Los Angeles", utc(2026, 11, 1, 9, 0)), (-8.0, false));
        // The wall clock: 01:59 PST, then 03:00 PDT.
        let tz = zone(2);
        let before = local(tz, utc(2026, 3, 8, 9, 59)).unwrap();
        let after = local(tz, utc(2026, 3, 8, 10, 0)).unwrap();
        assert_eq!((before.time(), before.zone()), ("1:59".into(), "PST (UTC-8)".into()));
        assert_eq!((after.time(), after.zone()), ("3:00".into(), "PDT (UTC-7)".into()));
        // What Home's test expects of the same instant.
        let l = local(tz, utc(2026, 11, 24, 2, 47)).unwrap();
        assert_eq!((l.hour, l.minute, l.date()), (18, 47, "Nov 23, Mon".into()));
    }

    #[test]
    fn europe_2026() {
        // 01:00 UTC on the last Sundays of March and October, for both.
        for city in ["London", "Paris", "Berlin"] {
            let base = if city == "London" { 0.0 } else { 1.0 };
            assert_eq!(hours(city, utc(2026, 3, 29, 0, 59)), (base, false));
            assert_eq!(hours(city, utc(2026, 3, 29, 1, 0)), (base + 1.0, true));
            assert_eq!(hours(city, utc(2026, 10, 25, 0, 59)), (base + 1.0, true));
            assert_eq!(hours(city, utc(2026, 10, 25, 1, 0)), (base, false));
        }
        assert_eq!(local(zone(13), utc(2026, 7, 1, 12, 0)).unwrap().zone(), "BST (UTC+1)");
        assert_eq!(local(zone(13), utc(2026, 1, 1, 12, 0)).unwrap().zone(), "GMT (UTC+0)");
    }

    #[test]
    fn southern_hemisphere() {
        // Sydney's summer spans the new year.
        assert_eq!(hours("Sydney", utc(2026, 1, 1, 0, 0)), (11.0, true));
        assert_eq!(hours("Sydney", utc(2025, 12, 31, 12, 59)), (11.0, true));
        assert_eq!(hours("Sydney", utc(2025, 12, 31, 13, 0)), (11.0, true)); // local midnight
        // Ends 03:00 AEDT on 5 April 2026 = 16:00 UTC on the 4th.
        assert_eq!(hours("Sydney", utc(2026, 4, 4, 15, 59)), (11.0, true));
        assert_eq!(hours("Sydney", utc(2026, 4, 4, 16, 0)), (10.0, false));
        assert_eq!(hours("Sydney", utc(2026, 7, 1, 0, 0)), (10.0, false));
        // Starts 02:00 AEST on 4 October 2026 = 16:00 UTC on the 3rd.
        assert_eq!(hours("Sydney", utc(2026, 10, 3, 15, 59)), (10.0, false));
        assert_eq!(hours("Sydney", utc(2026, 10, 3, 16, 0)), (11.0, true));
    }

    #[test]
    fn zones_without_daylight_time_and_odd_ones() {
        for t in [utc(2026, 1, 15, 0, 0), utc(2026, 7, 15, 0, 0)] {
            assert_eq!(hours("Tokyo", t), (9.0, false));
            assert_eq!(hours("Honolulu", t), (-10.0, false));
            assert_eq!(hours("Mumbai", t), (5.5, false));
            assert_eq!(hours("Mexico City", t), (-6.0, false));
        }
        assert_eq!(local(zone(18), utc(2026, 1, 1, 20, 0)).unwrap().zone(), "IST (UTC+5:30)");
        let l = local(zone(18), utc(2026, 12, 31, 20, 0)).unwrap();
        assert_eq!((l.year, l.month, l.day, l.hour, l.minute), (2027, 1, 1, 1, 30));
        // Cairo: from the last Friday of April at 00:00 to the last Thursday of October at 24:00.
        assert_eq!(hours("Cairo", utc(2026, 4, 23, 21, 59)), (2.0, false));
        assert_eq!(hours("Cairo", utc(2026, 4, 23, 22, 0)), (3.0, true));
        assert_eq!(hours("Cairo", utc(2026, 10, 29, 20, 59)), (3.0, true));
        assert_eq!(hours("Cairo", utc(2026, 10, 29, 21, 0)), (2.0, false));
        assert_eq!(hours("Anchorage", utc(2026, 7, 1, 0, 0)), (-8.0, true));
        assert_eq!(hours("Denver", utc(2026, 1, 1, 0, 0)), (-7.0, false));
    }

    #[test]
    fn rule_strings() {
        assert_eq!(parse("UTC0"), Some(Zone { name: "UTC", offset: 0, dst: None }));
        assert_eq!(parse("<+0330>-3:30").unwrap().offset, 12_600);
        assert_eq!(parse("<-03>3").unwrap().name, "-03");
        let z = parse("NZST-12NZDT,M9.5.0,M4.1.0/3").unwrap();
        let (name, off, start, end) = z.dst.unwrap();
        assert_eq!((name, off), ("NZDT", 13 * 3600));
        assert_eq!(start, Change { month: 9, week: 5, day: 0, time: 7200 });
        assert_eq!(end.time, 3 * 3600);
        // An explicit daylight offset, and a negative change time.
        assert_eq!(parse("AAA3BBB1:30,M1.1.0,M2.1.0").unwrap().dst.unwrap().1, -5400);
        assert_eq!(parse("<-02>2<-01>,M3.5.0/-1,M10.5.0/0").unwrap().dst.unwrap().2.time, -3600);
        for bad in [
            "", "PS8", "PST", "PST8PDT", "PST8PDT,M3.2.0", "PST8PDT,J60,J300", "PST8PDT,M13.1.0,M11.1.0",
            "PST8PDT,M3.6.0,M11.1.0", "PST8PDT,M3.2.7,M11.1.0", "PST8PDT,M3.2.0,M11.1.0,", "PST25", "PST8:60",
            "PST8PDT,M0.1.0,M11.1.0", "PST8PDT,M3.0.0,M11.1.0", "<>8", "<PST8", "PST8 ", "PST8PDT,M3.2.0/168,M11.1.0",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
            assert_eq!(offset(bad, 0), None);
        }
    }

    #[test]
    fn last_week_of_a_month() {
        // October 2026 has four Sundays after the 4th: the last is the 25th.
        assert_eq!(civil(change_at(Change { month: 10, week: 5, day: 0, time: 0 }, 2026) / 86_400), (2026, 10, 25));
        // March 2026: second Sunday the 8th; first Sunday the 1st.
        assert_eq!(civil(change_at(Change { month: 3, week: 2, day: 0, time: 0 }, 2026) / 86_400), (2026, 3, 8));
        assert_eq!(civil(change_at(Change { month: 3, week: 1, day: 0, time: 0 }, 2026) / 86_400), (2026, 3, 1));
        // February 2028 (leap): the last Tuesday is the 29th.
        assert_eq!(civil(change_at(Change { month: 2, week: 5, day: 2, time: 0 }, 2028) / 86_400), (2028, 2, 29));
        assert_eq!(civil(change_at(Change { month: 2, week: 5, day: 2, time: 0 }, 2027) / 86_400), (2027, 2, 23));
    }

    /// VIOLET_TZ_DUMP=file writes every city's offset every three hours for
    /// 2026-2031, to compare with a time-zone database on the build machine.
    #[test]
    fn dump() {
        let Ok(path) = std::env::var("VIOLET_TZ_DUMP") else { return };
        let mut out = String::new();
        let mut t = utc(2026, 1, 1, 0, 0);
        while t < utc(2032, 1, 1, 0, 0) {
            for (i, city) in CITIES.iter().enumerate() {
                out += &format!("{city}\t{t}\t{}\n", offset(zone(i), t).unwrap().seconds);
            }
            t += 3 * 3600;
        }
        std::fs::write(path, out).unwrap();
    }
}
