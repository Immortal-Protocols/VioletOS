use crate::{
    locomo,
    player::Catalog,
    style::{self, INK, LINE, MUTED, SURFACE},
};
use std::time::{SystemTime, UNIX_EPOCH};
use violet::{
    keyboard::{Key, Keyboard},
    Canvas, Fonts, Rect,
};

const TABS: [Rect; 3] = [
    Rect::new(26, 1482, 245, 1574),
    Rect::new(250, 1482, 470, 1574),
    Rect::new(475, 1482, 694, 1574),
];
const MODES: [Rect; 3] = [
    Rect::new(36, 282, 246, 374),
    Rect::new(255, 282, 465, 374),
    Rect::new(474, 282, 684, 374),
];
const STATION: Rect = Rect::new(36, 438, 684, 553);
const LENGTH: Rect = Rect::new(36, 612, 684, 727);
const COUNT: [Rect; 4] = [
    Rect::new(36, 790, 160, 876),
    Rect::new(171, 790, 273, 876),
    Rect::new(447, 790, 549, 876),
    Rect::new(560, 790, 684, 876),
];
const CANCEL: Rect = Rect::new(36, 914, 684, 1002);
const SUBMIT: Rect = Rect::new(36, 1080, 684, 1192);
const BACK_PICKER: Rect = Rect::new(36, 34, 210, 110);
const SEARCH: Rect = Rect::new(36, 280, 684, 374);
const DONE: Rect = Rect::new(523, 280, 684, 374);
const REFRESH: Rect = Rect::new(501, 47, 684, 130);
const PREV: Rect = Rect::new(36, 1336, 243, 1430);
const NEXT: Rect = Rect::new(477, 1336, 684, 1430);
const ROW_TOP: i32 = 410;
const ROW_H: i32 = 112;
const PAGE_SIZE: usize = 8;

const STATIONS: [&str; 18] = [
    "Future French House",
    "Latin House",
    "House",
    "Synth-Pop",
    "Indie Pop",
    "Classic Rock",
    "Punk Rock",
    "Boom Bap",
    "Lo-Fi Hip-Hop",
    "Country Pop",
    "Cool Jazz",
    "Jazz Fusion",
    "Techno",
    "Latin Techno",
    "Indie Folk",
    "Thrash Metal",
    "Progressive Metal",
    "Cinematic Hip-Hop",
];
// None keeps the desktop setting. Zero is Locomo's Auto length.
const LENGTHS: [Option<u32>; 14] = [
    None,
    Some(0),
    Some(60),
    Some(90),
    Some(120),
    Some(150),
    Some(180),
    Some(240),
    Some(300),
    Some(360),
    Some(420),
    Some(480),
    Some(540),
    Some(600),
];

fn length_label(length: Option<u32>) -> String {
    match length {
        None => "Mac setting".into(),
        Some(0) => "Auto".into(),
        Some(seconds) if seconds % 60 == 0 => format!("{} min", seconds / 60),
        Some(seconds) => format!("{}.5 min", seconds / 60),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Player,
    Generate,
    Library,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Picker {
    Station,
    Length,
}

pub enum Action {
    Redraw,
    Play(usize),
    Submit,
    None,
}

pub struct Pages {
    pub page: Page,
    picker: Option<Picker>,
    option_page: usize,
    station: usize,
    length: usize,
    mode: usize,
    count: u32,
    source: usize,
    library_page: usize,
    query: String,
    request: u32,
    library: Option<locomo::Library>,
    keyboard: Option<Keyboard>,
}

impl Default for Pages {
    fn default() -> Self {
        Self {
            page: Page::Player,
            picker: None,
            option_page: 0,
            station: 10,
            length: 0,
            mode: 0,
            count: 5,
            source: 0,
            library_page: 0,
            query: String::new(),
            request: 0,
            library: None,
            keyboard: None,
        }
    }
}

pub fn navigation(c: &mut Canvas, f: &Fonts, page: Page) {
    c.fill(Rect::new(0, 1456, 720, 1600), SURFACE);
    c.fill(Rect::new(26, 1456, 694, 1458), LINE);
    for (i, title) in ["Playing", "Generate", "Library"].iter().enumerate() {
        button(c, f, TABS[i], title, page as usize == i);
    }
}

fn button(c: &mut Canvas, f: &Fonts, rect: Rect, title: &str, active: bool) {
    let font = f.medium(26.0);
    if active {
        c.round_rect(rect, 18.0, INK);
    } else {
        style::outline(c, rect, 18.0);
    }
    font.centered(
        c,
        &font.fit(title, rect.width() - 24),
        rect.x0,
        rect.x1,
        (rect.y0 + rect.y1 + font.ascent() + font.descent()) / 2,
        if active { SURFACE } else { INK },
    );
}

fn text(c: &mut Canvas, f: &Fonts, title: &str, y: i32, muted: bool) {
    let face = f.regular(27.0);
    face.draw(
        c,
        &face.fit(title, 648),
        36,
        y,
        if muted { MUTED } else { INK },
    );
}

fn row(index: usize) -> Rect {
    let y = ROW_TOP + ROW_H * index as i32;
    Rect::new(36, y, 684, y + ROW_H - 12)
}

impl Pages {
    pub fn sync_library(&mut self) -> bool {
        if self.library.is_some() || self.request == 0 {
            return false;
        }
        if let Some(list) = locomo::library().filter(|list| list.request == self.request) {
            println!("music: Mac library received ({} songs)", list.tracks.len());
            self.library = Some(list);
            return true;
        }
        false
    }

    pub fn refresh(&mut self) {
        if self.source == 2 {
            return;
        }
        self.request = if self.request == 0 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
                .max(1)
        } else {
            self.request.wrapping_add(1).max(1)
        };
        self.library = None;
        let _ = locomo::browse(
            self.request,
            &self.query,
            self.library_page,
            self.source == 1,
        );
    }

    fn album_rows(&self, catalog: &Catalog) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        catalog
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, track)| {
                (!track.audio.starts_with('/') && track.title.to_lowercase().contains(&query))
                    .then_some(i)
            })
            .collect()
    }

    fn more(&self, catalog: &Catalog) -> bool {
        if self.source == 2 {
            self.album_rows(catalog).len() > (self.library_page + 1) * PAGE_SIZE
        } else {
            self.library.as_ref().is_some_and(|list| list.more)
        }
    }

    pub fn tap(&mut self, x: i32, y: i32, catalog: &Catalog) -> Action {
        if let Some(keyboard) = &mut self.keyboard {
            if DONE.contains(x, y) || keyboard.key_at(x, y) == Some(Key::Go) {
                self.keyboard = None;
                self.library_page = 0;
                self.refresh();
            } else if let Some(key) = keyboard.key_at(x, y) {
                keyboard.press(key, &mut self.query, 70);
            } else {
                return Action::None;
            }
            return Action::Redraw;
        }

        if let Some(i) = TABS.iter().position(|r| r.contains(x, y)) {
            self.page = [Page::Player, Page::Generate, Page::Library][i];
            self.picker = None;
            if self.page == Page::Library {
                self.refresh();
            }
            return Action::Redraw;
        }
        if let Some(picker) = self.picker {
            let count = if picker == Picker::Station {
                STATIONS.len()
            } else {
                LENGTHS.len()
            };
            if BACK_PICKER.contains(x, y) {
                self.picker = None;
            } else if PREV.contains(x, y) {
                self.option_page = self.option_page.saturating_sub(1);
            } else if NEXT.contains(x, y) && (self.option_page + 1) * PAGE_SIZE < count {
                self.option_page += 1;
            } else if let Some(i) = (0..PAGE_SIZE).find(|i| row(*i).contains(x, y)) {
                let selection = self.option_page * PAGE_SIZE + i;
                if selection < count {
                    match picker {
                        Picker::Station => self.station = selection,
                        Picker::Length => self.length = selection,
                    }
                    self.picker = None;
                }
            }
            return Action::Redraw;
        }
        match self.page {
            Page::Player => return Action::None,
            Page::Generate => {
                if let Some(i) = MODES.iter().position(|r| r.contains(x, y)) {
                    self.mode = i;
                } else if self.mode != 2 && STATION.contains(x, y) {
                    self.picker = Some(Picker::Station);
                    self.option_page = self.station / PAGE_SIZE;
                } else if self.mode != 2 && LENGTH.contains(x, y) {
                    self.picker = Some(Picker::Length);
                    self.option_page = self.length / PAGE_SIZE;
                } else if self.mode == 1 {
                    if let Some(i) = COUNT.iter().position(|r| r.contains(x, y)) {
                        self.count = (self.count as i32 + [-10, -1, 1, 10][i]).clamp(1, 100) as u32;
                    }
                }
                if self.mode != 0 && CANCEL.contains(x, y) {
                    let action = if self.mode == 1 { "stop" } else { "disarm" };
                    if locomo::control(action).is_ok() {
                        return Action::Submit;
                    }
                }
                if SUBMIT.contains(x, y) {
                    let count = if self.mode == 1 { self.count } else { 1 };
                    if locomo::generate(
                        STATIONS[self.station],
                        LENGTHS[self.length],
                        count,
                        self.mode == 2,
                    )
                    .is_ok()
                    {
                        return Action::Submit;
                    }
                }
            }
            Page::Library => {
                // Library filters sit above the search field.
                if let Some(i) = MODES.iter().position(|r| r.offset(0, -112).contains(x, y)) {
                    self.source = i;
                    self.library_page = 0;
                    self.refresh();
                } else if REFRESH.contains(x, y) && self.source != 2 {
                    self.refresh();
                } else if SEARCH.contains(x, y) {
                    self.keyboard = Some(Keyboard::address());
                } else if PREV.contains(x, y) && self.library_page > 0 {
                    self.library_page -= 1;
                    self.refresh();
                } else if NEXT.contains(x, y) && self.more(catalog) {
                    self.library_page += 1;
                    self.refresh();
                } else if let Some(i) = (0..PAGE_SIZE).find(|i| row(*i).contains(x, y)) {
                    if self.source == 2 {
                        if let Some(&index) = self
                            .album_rows(catalog)
                            .get(self.library_page * PAGE_SIZE + i)
                        {
                            self.page = Page::Player;
                            return Action::Play(index);
                        }
                    } else if let Some(song) =
                        self.library.as_ref().and_then(|list| list.tracks.get(i))
                    {
                        if locomo::play(&song.id).is_ok() {
                            return Action::Submit;
                        }
                    }
                }
            }
        }
        Action::Redraw
    }

    pub fn draw(&self, c: &mut Canvas, f: &Fonts, catalog: &Catalog, status: &str) {
        c.fill(c.bounds(), SURFACE);
        if let Some(picker) = self.picker {
            button(c, f, BACK_PICKER, "‹ Back", false);
            f.bold(56.0).draw(
                c,
                if picker == Picker::Station {
                    "Choose a station"
                } else {
                    "Song length"
                },
                36,
                222,
                INK,
            );
            let titles: Vec<_> = if picker == Picker::Station {
                STATIONS.iter().map(|s| (*s).into()).collect()
            } else {
                LENGTHS.iter().copied().map(length_label).collect()
            };
            let first = self.option_page * PAGE_SIZE;
            for (i, title) in titles.iter().skip(first).take(PAGE_SIZE).enumerate() {
                let selected = if picker == Picker::Station {
                    self.station
                } else {
                    self.length
                };
                button(c, f, row(i), title, selected == first + i);
            }
            self.draw_pager(c, f, self.option_page, first + PAGE_SIZE < titles.len());
        } else if self.page == Page::Generate {
            self.draw_generate(c, f, status);
        } else {
            self.draw_library(c, f, catalog, status);
        }
        navigation(c, f, self.page);
        if let Some(keyboard) = &self.keyboard {
            keyboard.draw(c, f);
        }
    }

    fn draw_generate(&self, c: &mut Canvas, f: &Fonts, status: &str) {
        f.bold(72.0).draw(c, "Generate", 36, 143, INK);
        style::record(c, 614.0, 130.0, 58.0, INK, SURFACE);
        text(c, f, "Use Locomo Music on your paired Mac.", 212, true);
        for (i, title) in ["One song", "Automatic", "Overnight"].iter().enumerate() {
            button(c, f, MODES[i], title, self.mode == i);
        }
        if self.mode == 2 {
            text(c, f, "Five tracks per station.", 472, false);
            text(
                c,
                f,
                "Locomo runs once the Mac is idle and on power.",
                524,
                true,
            );
        } else {
            text(c, f, "Station", 425, true);
            button(
                c,
                f,
                STATION,
                &format!("{}  ›", STATIONS[self.station]),
                false,
            );
            text(c, f, "Song length", 600, true);
            button(
                c,
                f,
                LENGTH,
                &format!("{}  ›", length_label(LENGTHS[self.length])),
                false,
            );
            if self.mode == 1 {
                text(c, f, "Automatic songs", 776, true);
                for (i, label) in ["−10", "−1", "+1", "+10"].iter().enumerate() {
                    button(c, f, COUNT[i], label, false);
                }
                f.bold(42.0)
                    .centered(c, &self.count.to_string(), 273, 447, 849, INK);
            }
        }
        button(
            c,
            f,
            SUBMIT,
            [
                "Make a song",
                "Start automatic generation",
                "Arm overnight batch",
            ][self.mode],
            true,
        );
        if self.mode != 0 {
            button(
                c,
                f,
                CANCEL,
                if self.mode == 1 {
                    "Stop automatic generation"
                } else {
                    "Disarm overnight batch"
                },
                false,
            );
        }
        let face = f.regular(27.0);
        let mut line = String::new();
        let mut y = 1260;
        for word in status.split_whitespace() {
            if !line.is_empty() && face.width(&line) + face.width(word) + 12 > 648 {
                text(c, f, &line, y, true);
                line.clear();
                y += 42;
                if y >= 1430 {
                    break;
                }
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            text(c, f, &line, y, true);
        }
    }

    fn draw_library(&self, c: &mut Canvas, f: &Fonts, catalog: &Catalog, status: &str) {
        f.bold(65.0).draw(c, "Library", 36, 125, INK);
        if self.source != 2 {
            button(c, f, REFRESH, "Refresh", false);
        }
        for (i, title) in ["Mac Library", "Favorites", "Cloud Songs"]
            .iter()
            .enumerate()
        {
            button(c, f, MODES[i].offset(0, -112), title, self.source == i);
        }
        style::outline(c, SEARCH, 18.0);
        let query = if self.query.is_empty() {
            "Search songs"
        } else {
            &self.query
        };
        let font = f.medium(27.0);
        let width = if self.keyboard.is_some() { 430 } else { 585 };
        font.draw(
            c,
            &font.fit(query, width),
            60,
            338,
            if self.query.is_empty() { MUTED } else { INK },
        );
        if self.keyboard.is_some() {
            button(c, f, DONE, "Search", true);
        }

        if self.source == 2 {
            let rows = self.album_rows(catalog);
            if rows.is_empty() {
                text(c, f, "No songs to display.", 465, true);
            }
            for (i, &index) in rows
                .iter()
                .skip(self.library_page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .enumerate()
            {
                let t = &catalog.tracks[index];
                Self::draw_song(c, f, i, &t.title, &t.artist, t.duration_ms, false);
            }
        } else if let Some(list) = &self.library {
            if !list.error.is_empty() {
                text(c, f, &list.error, 465, true);
            } else if list.tracks.is_empty() {
                text(c, f, "No matching songs.", 465, true);
            }
            for (i, t) in list.tracks.iter().enumerate().take(PAGE_SIZE) {
                Self::draw_song(c, f, i, &t.title, &t.station, t.duration_ms, t.favorite);
            }
        } else {
            text(c, f, "Loading Mac library…", 465, true);
        }
        if self.keyboard.is_none() {
            text(c, f, status, 1320, true);
            self.draw_pager(c, f, self.library_page, self.more(catalog));
        }
    }

    fn draw_song(
        c: &mut Canvas,
        f: &Fonts,
        index: usize,
        title: &str,
        subtitle: &str,
        duration_ms: u64,
        favorite: bool,
    ) {
        let r = row(index);
        c.fill(Rect::new(r.x0, r.y1, r.x1, r.y1 + 2), LINE);
        c.ring((r.x0 + 32) as f32, (r.y0 + 52) as f32, 24.0, 2.0, LINE);
        let x = (r.x0 + 32) as f32;
        let y = (r.y0 + 52) as f32;
        c.triangle(
            [[x - 6.0, y - 10.0], [x - 6.0, y + 10.0], [x + 10.0, y]],
            INK,
        );
        let face = f.medium(29.0);
        face.draw(c, &face.fit(title, 430), r.x0 + 78, r.y0 + 42, INK);
        let small = f.regular(22.0);
        small.draw(c, &small.fit(subtitle, 430), r.x0 + 78, r.y0 + 77, MUTED);
        let secs = duration_ms / 1000;
        let timing = format!(
            "{}{secs_min}:{:02}",
            if favorite { "♥  " } else { "" },
            secs % 60,
            secs_min = secs / 60
        );
        small.draw(
            c,
            &timing,
            r.x1 - 24 - small.width(&timing),
            r.y0 + 57,
            MUTED,
        );
    }

    fn draw_pager(&self, c: &mut Canvas, f: &Fonts, page: usize, more: bool) {
        if page > 0 {
            button(c, f, PREV, "‹ Previous", false);
        }
        if more {
            button(c, f, NEXT, "Next ›", false);
        }
        if page > 0 || more {
            f.medium(24.0)
                .centered(c, &format!("Page {}", page + 1), 250, 470, 1390, MUTED);
        }
    }
}
