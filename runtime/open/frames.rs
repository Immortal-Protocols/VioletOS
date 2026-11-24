//! Frame-pipeline measurements (docs/8ms-frame-pipeline-rfc.md): what a
//! present costs, how long the app in front takes from an input event to its
//! next frame, and how often Runtime is polled for touches. `frames` in
//! /dev/violet/sys reports them and starts a new window; `bench tap X Y N`
//! injects N synthetic presses so the handset can be measured without a finger.
use alloc::string::String;

/// Latency buckets, in milliseconds (the last one is everything slower).
const BUCKETS: [u64; 7] = [2, 4, 8, 16, 33, 66, u64::MAX];

#[derive(Default, Clone, Copy)]
pub struct Histogram {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    buckets: [u32; 7],
}

impl Histogram {
    pub fn add(&mut self, ns: u64) {
        self.count += 1;
        self.total_ns += ns;
        self.max_ns = self.max_ns.max(ns);
        let ms = ns / 1_000_000;
        let b = BUCKETS.iter().position(|&limit| ms < limit).unwrap_or(BUCKETS.len() - 1);
        self.buckets[b] += 1;
    }
    /// The bucket's upper bound containing the p-th percentile, in ms.
    fn percentile_ms(&self, p: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let want = (self.count * p).div_ceil(100);
        let mut seen = 0;
        for (i, &n) in self.buckets.iter().enumerate() {
            seen += n as u64;
            if seen >= want {
                return if BUCKETS[i] == u64::MAX { self.max_ns / 1_000_000 } else { BUCKETS[i] };
            }
        }
        self.max_ns / 1_000_000
    }
    fn report(&self, name: &str) -> String {
        if self.count == 0 {
            return alloc::format!("{name}=0");
        }
        alloc::format!(
            "{name}={} {name}_avg_us={} {name}_p50_ms<{} {name}_p95_ms<{} {name}_max_us={}",
            self.count,
            self.total_ns / self.count / 1000,
            self.percentile_ms(50),
            self.percentile_ms(95),
            self.max_ns / 1000
        )
    }
}

/// A synthetic press: down at `at`, then focus is handed back so the app
/// forgets the press instead of opening something.
#[derive(Clone, Copy)]
pub struct Bench {
    pub x: i32,
    pub y: i32,
    pub left: u32,
    pub next_ns: u64,
    pub pressed: bool,
}

#[derive(Default)]
pub struct Frames {
    pub since_ns: u64,
    /// Time spent in present (copy and cache clean), per call.
    pub present: Histogram,
    pub present_pixels: u64,
    /// Input delivered to the app in front until its next present ends.
    pub input: Histogram,
    /// When the oldest unanswered input reached the app in front.
    pub input_at: Option<u64>,
    /// Gap between Runtime polls (touch delivery) while focused.
    pub poll_gap: Histogram,
    pub last_poll_ns: u64,
    pub bench: Option<Bench>,
    /// Frames the app in front presented since it last gained focus.
    pub flips: u64,
    /// Of the present time, how much went to cleaning the data cache.
    pub clean_ns: u64,
    /// How long presents waited for the panel to scan past their rows.
    pub waited: Histogram,
    /// Focus handed to the app in front until its next present (a full redraw).
    pub redraw: Histogram,
    pub focus_at: Option<u64>,
}

impl Frames {
    pub fn presented(&mut self, start_ns: u64, end_ns: u64, pixels: u64) {
        self.present.add(end_ns.wrapping_sub(start_ns));
        self.present_pixels += pixels;
        self.flips += 1;
        // An input the app ignored is not answered by a frame a second later.
        if let Some(at) = self.input_at.take().filter(|&at| end_ns.wrapping_sub(at) < 1_000_000_000) {
            self.input.add(end_ns.wrapping_sub(at));
        }
        if let Some(at) = self.focus_at.take().filter(|&at| end_ns.wrapping_sub(at) < 1_000_000_000) {
            self.redraw.add(end_ns.wrapping_sub(at));
        }
    }
    pub fn input_delivered(&mut self, now_ns: u64) {
        if self.input_at.is_none() {
            self.input_at = Some(now_ns);
        }
    }
    pub fn polled(&mut self, now_ns: u64) {
        if self.last_poll_ns != 0 {
            self.poll_gap.add(now_ns.wrapping_sub(self.last_poll_ns));
        }
        self.last_poll_ns = now_ns;
    }
    pub fn report(&mut self, now_ns: u64) -> String {
        let secs_x10 = now_ns.wrapping_sub(self.since_ns) / 100_000_000;
        let s = alloc::format!(
            "window_ms={} {} present_mpix={} clean_ms={} {} {} {} {}\n",
            secs_x10 * 100,
            self.present.report("present"),
            self.present_pixels / 1_000_000,
            self.clean_ns / 1_000_000,
            self.waited.report("wait"),
            self.input.report("input"),
            self.redraw.report("redraw"),
            self.poll_gap.report("poll"),
        );
        let bench = self.bench;
        *self = Frames { since_ns: now_ns, bench, ..Frames::default() };
        s
    }
}
