//! Layout: block boxes stacked down the page, inline content broken into
//! lines, list markers, and table rows as equal-width columns. The result is
//! a display list in page coordinates (device pixels), plus the link areas
//! and fragment anchors that the browser hit-tests and scrolls to.
use std::collections::HashMap;

use crate::dom::{Document, Kind, DOCUMENT};
use crate::style::{Align, Display, Style, SCALE};
use violet::{rgb, Color, Fonts, Rect};

#[derive(Debug)]
pub enum Item {
    Fill { r: Rect, color: Color },
    Text { x: i32, baseline: i32, text: String, size: f32, bold: bool, color: Color },
    Dot { cx: i32, cy: i32, radius: f32, color: Color },
}

pub struct Layout {
    pub items: Vec<Item>,
    pub links: Vec<(Rect, String)>,
    pub anchors: HashMap<String, i32>,
    pub height: i32,
    pub background: Color,
}

enum Frag {
    Word { text: String, node: usize, link: Option<usize> },
    /// An image's box, from its width and height attributes.
    Image { width: i32, height: i32, node: usize, link: Option<usize> },
    Space { node: usize, count: usize },
    Break,
}

struct Builder<'a> {
    d: &'a Document,
    styles: &'a [Style],
    fonts: &'a Fonts,
    has_block: Vec<bool>,
    /// The bottom margin of the block just above, which the next block's top margin collapses with.
    margin: i32,
    /// Column widths of each table being laid out, by table node.
    columns: HashMap<usize, Vec<i32>>,
    out: Layout,
}

fn face(fonts: &Fonts, s: &Style) -> violet::Face {
    let size = s.font_size.round();
    if s.bold { fonts.bold(size) } else { fonts.regular(size) }
}

pub fn layout(d: &Document, styles: &[Style], fonts: &Fonts, width: i32) -> Layout {
    let mut has_block = vec![false; d.nodes.len() + 1];
    // Children have larger handles than their parents in practice, but not
    // always (the tree builder moves nodes), so iterate to a fixed point.
    loop {
        let mut changed = false;
        for n in 1..=d.nodes.len() {
            let v = d.node(n).children.iter().any(|&c| {
                styles[c].display != Display::None && (styles[c].is_block() || has_block[c])
            });
            if v != has_block[n] {
                has_block[n] = v;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let background = ["body", "html"]
        .iter()
        .filter_map(|n| d.find(DOCUMENT, n))
        .find_map(|n| styles[n].background)
        .unwrap_or(rgb(0xff, 0xff, 0xff));
    let mut b = Builder {
        d,
        styles,
        fonts,
        has_block,
        margin: 0,
        columns: HashMap::new(),
        out: Layout { items: Vec::new(), links: Vec::new(), anchors: HashMap::new(), height: 0, background },
    };
    let mut y = 0;
    for &c in &d.node(DOCUMENT).children {
        if matches!(d.node(c).kind, Kind::Element { .. }) {
            y = b.block(c, 0, width, y, None);
        }
    }
    b.out.height = y;
    b.out
}

impl Builder<'_> {
    fn link_of(&self, node: usize, inherited: Option<usize>) -> Option<usize> {
        if self.d.name(node) == "a" && self.d.attribute(node, "href").is_some() { Some(node) } else { inherited }
    }

    fn block(&mut self, node: usize, x: i32, width: i32, y: i32, link: Option<usize>) -> i32 {
        let s = &self.styles[node];
        if s.display == Display::None {
            return y;
        }
        let s = s.clone();
        let link = self.link_of(node, link);
        let top_margin = s.margin_top.round() as i32;
        let mut y = y - self.margin + self.margin.max(top_margin);
        self.margin = 0;
        let bx = x + s.margin_left.round() as i32;
        let bw = (width - (s.margin_left + s.margin_right).round() as i32).max(60.min(width));
        for anchor in ["id", "name"] {
            if let Some(a) = self.d.attribute(node, anchor) {
                self.out.anchors.entry(a.to_string()).or_insert(y);
            }
        }
        let background_at = self.out.items.len();
        let top = y;
        let inner_x = bx + s.padding_left.round() as i32;
        let inner_w = (bw - (s.padding_left + s.padding_right).round() as i32).max(40.min(bw));
        y += s.padding_top.round() as i32;
        if s.display == Display::ListItem && s.bullets {
            self.marker(node, &s, inner_x, y);
        }
        if self.d.name(node) == "table" {
            self.size_columns(node, inner_w);
        }
        if self.d.name(node) == "hr" {
            self.out.items.push(Item::Fill { r: Rect::new(inner_x, y, inner_x + inner_w, y + 2), color: s.border.unwrap_or(rgb(0x99, 0x99, 0x99)) });
            y += 2;
        } else if self.d.name(node) == "img" {
            if let Some((w, h)) = self.image_size(node) {
                let r = Rect::new(inner_x, y, inner_x + w.min(inner_w), y + h);
                self.out.items.push(Item::Fill { r, color: rgb(0xe4, 0xe0, 0xea) });
                if let Some(href) = link.and_then(|a| self.d.attribute(a, "href")) {
                    self.out.links.push((r, href.to_string()));
                }
                y += h;
            }
        } else if s.display == Display::TableRow {
            y = self.row(node, inner_x, inner_w, y, link);
        } else {
            y = self.content(node, inner_x, inner_w, y, link);
        }
        y += s.padding_bottom.round() as i32;
        if let Some(color) = s.background {
            self.out.items.insert(background_at, Item::Fill { r: Rect::new(bx, top, bx + bw, y), color });
        }
        if let Some(color) = s.border
            && self.d.name(node) != "hr"
        {
            for r in [
                Rect::new(bx, top, bx + bw, top + 2),
                Rect::new(bx, y - 2, bx + bw, y),
                Rect::new(bx, top, bx + 2, y),
                Rect::new(bx + bw - 2, top, bx + bw, y),
            ] {
                self.out.items.push(Item::Fill { r, color });
            }
        }
        self.margin = s.margin_bottom.round() as i32;
        y + self.margin
    }

    fn marker(&mut self, node: usize, s: &Style, x: i32, y: i32) {
        let f = face(self.fonts, s);
        let parent = self.d.node(node).parent;
        let baseline = y + f.ascent();
        if self.d.name(parent) == "ol" {
            let start = self.d.attribute(parent, "start").and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(1);
            let index = self.d.node(parent).children.iter().filter(|&&c| self.d.name(c) == "li").position(|&c| c == node).unwrap_or(0);
            let text = format!("{}.", start + index as i32);
            let w = f.width(&text);
            self.out.items.push(Item::Text { x: x - w - (6.0 * SCALE) as i32, baseline, text, size: f.size(), bold: s.bold, color: s.color });
        } else {
            let radius = (s.font_size / 7.0).max(3.0);
            self.out.items.push(Item::Dot { cx: x - (10.0 * SCALE) as i32, cy: baseline - (s.font_size * 0.33) as i32, radius, color: s.color });
        }
    }

    fn row(&mut self, node: usize, x: i32, width: i32, y: i32, link: Option<usize>) -> i32 {
        let cells: Vec<usize> = self
            .d
            .node(node)
            .children
            .iter()
            .copied()
            .filter(|&c| self.styles[c].display != Display::None && matches!(self.d.node(c).kind, Kind::Element { .. }))
            .collect();
        if cells.is_empty() {
            return y;
        }
        let spans: Vec<i32> = cells
            .iter()
            .map(|&c| self.d.attribute(c, "colspan").and_then(|v| v.trim().parse::<i32>().ok()).unwrap_or(1).clamp(1, 20))
            .collect();
        let columns: i32 = spans.iter().sum();
        let mut table = self.d.node(node).parent;
        while table != 0 && self.d.name(table) != "table" {
            table = self.d.node(table).parent;
        }
        let widths = self.columns.get(&table).filter(|w| w.len() as i32 >= columns && w.iter().sum::<i32>() > 0).cloned();
        let total: i32 = widths.as_ref().map_or(1, |w| w.iter().sum());
        let mut bottom = y;
        let mut column = 0usize;
        let mut cx = x;
        for (&cell, span) in cells.iter().zip(spans) {
            let cw = match &widths {
                Some(w) => w[column..column + span as usize].iter().sum::<i32>() * width / total,
                None => width * span / columns,
            };
            self.margin = 0;
            bottom = bottom.max(self.block(cell, cx, cw, y, link));
            cx += cw;
            column += span as usize;
        }
        self.margin = 0;
        bottom
    }

    /// The narrowest and widest a node's content can be laid out: its
    /// longest word, and all of each line's content side by side.
    fn measure(&self, node: usize) -> (i32, i32) {
        let s = &self.styles[node];
        if s.display == Display::None {
            return (0, 0);
        }
        match &self.d.node(node).kind {
            Kind::Text(text) => {
                let f = face(self.fonts, s);
                let space = f.width(" ");
                let words: Vec<i32> = text.split_whitespace().map(|w| f.width(w)).collect();
                let min = words.iter().copied().max().unwrap_or(0);
                let max = words.iter().sum::<i32>() + space * words.len().saturating_sub(1) as i32;
                if s.pre { (max, max) } else { (min, max) }
            }
            Kind::Element { name, .. } => {
                let extra = (s.padding_left + s.padding_right + s.margin_left + s.margin_right).round() as i32;
                if name == "img" {
                    let w = self.image_size(node).map_or(0, |(w, _)| w);
                    return (w + extra, w + extra);
                }
                let (mut min, mut max, mut line) = (0, 0, 0);
                if name == "table" {
                    // A nested table: its rows side by side are its width.
                    for &c in &self.d.node(node).children {
                        let (cmin, cmax) = self.measure(c);
                        min = min.max(cmin);
                        max = max.max(cmax);
                    }
                    return (min + extra, max + extra);
                }
                for &c in &self.d.node(node).children {
                    let cs = &self.styles[c];
                    if cs.display == Display::None {
                        continue;
                    }
                    let (cmin, cmax) = self.measure(c);
                    min = min.max(cmin);
                    let element = matches!(self.d.node(c).kind, Kind::Element { .. });
                    if self.d.name(c) == "br" {
                        max = max.max(line);
                        line = 0;
                    } else if element && cs.display == Display::TableRow {
                        max = max.max(cmax);
                    } else if element && (cs.is_block() || self.has_block[c]) && cs.display != Display::TableCell {
                        max = max.max(line).max(cmax);
                        line = 0;
                    } else {
                        line += cmax;
                    }
                }
                max = max.max(line);
                (min + extra, max + extra)
            }
            _ => (0, 0),
        }
    }

    /// Size a table's columns from its cells' content, as browsers do for
    /// tables without fixed widths: each column gets at least its longest word,
    /// and the rest of the width goes where the content is widest.
    fn size_columns(&mut self, table: usize, width: i32) {
        let mut rows = Vec::new();
        let mut stack = vec![table];
        while let Some(n) = stack.pop() {
            for &c in self.d.node(n).children.iter().rev() {
                if self.styles[c].display == Display::None {
                    continue;
                }
                match self.d.name(c) {
                    "tr" => rows.push(c),
                    "thead" | "tbody" | "tfoot" => stack.push(c),
                    _ => {}
                }
            }
        }
        rows.reverse();
        let mut min: Vec<i32> = Vec::new();
        let mut max: Vec<i32> = Vec::new();
        for &row in &rows {
            let mut column = 0usize;
            for &cell in &self.d.node(row).children {
                if !matches!(self.d.name(cell), "td" | "th") || self.styles[cell].display == Display::None {
                    continue;
                }
                let span = self.d.attribute(cell, "colspan").and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(1).clamp(1, 20);
                if min.len() < column + span {
                    min.resize(column + span, 0);
                    max.resize(column + span, 0);
                }
                if span == 1 {
                    let (cmin, cmax) = self.measure(cell);
                    min[column] = min[column].max(cmin);
                    max[column] = max[column].max(cmax);
                }
                column += span;
            }
        }
        if min.is_empty() {
            return;
        }
        let (total_min, total_max): (i32, i32) = (min.iter().sum(), max.iter().sum());
        let widths: Vec<i32> = if total_max <= width {
            // Everything fits on one line: share the spare width by content.
            let spare = width - total_max;
            max.iter().map(|&m| m + if total_max > 0 { spare * m / total_max } else { spare / max.len() as i32 }).collect()
        } else if total_min >= width {
            min.clone()
        } else {
            let room = width - total_min;
            let flexible: i32 = max.iter().zip(&min).map(|(a, b)| a - b).sum::<i32>().max(1);
            min.iter().zip(&max).map(|(&lo, &hi)| lo + room * (hi - lo) / flexible).collect()
        };
        self.columns.insert(table, widths);
    }

    fn image_size(&self, node: usize) -> Option<(i32, i32)> {
        let dimension = |a: &str| {
            let v = self.d.attribute(node, a)?.trim().trim_end_matches("px").to_string();
            v.parse::<f32>().ok().map(|n| (n * SCALE).round() as i32)
        };
        let (w, h) = (dimension("width"), dimension("height"));
        let (w, h) = match (w, h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, w * 3 / 4),
            (None, Some(h)) => (h * 4 / 3, h),
            (None, None) => return None,
        };
        // Wider than the screen: scale down, keeping the shape.
        let most = violet::W as i32 - 32;
        Some(if w > most { (most, h * most / w.max(1)) } else { (w, h) })
    }

    /// The children of a block: runs of inline content, and block children.
    fn content(&mut self, node: usize, x: i32, width: i32, y: i32, link: Option<usize>) -> i32 {
        let align = self.styles[node].align;
        let mut y = y;
        let mut run: Vec<Frag> = Vec::new();
        let children = self.d.node(node).children.clone();
        for c in children {
            let st = &self.styles[c];
            if st.display == Display::None {
                continue;
            }
            let element = matches!(self.d.node(c).kind, Kind::Element { .. });
            if element && (st.is_block() || self.has_block[c]) {
                y = self.lines(std::mem::take(&mut run), x, width, y, align);
                y = self.block(c, x, width, y, link);
            } else {
                self.collect(c, link, &mut run);
            }
        }
        self.lines(run, x, width, y, align)
    }

    fn push_space(run: &mut Vec<Frag>, node: usize) {
        if matches!(run.last(), Some(Frag::Word { .. })) {
            run.push(Frag::Space { node, count: 1 });
        }
    }

    fn collect(&self, node: usize, link: Option<usize>, run: &mut Vec<Frag>) {
        let s = &self.styles[node];
        match &self.d.node(node).kind {
            Kind::Text(text) => {
                if s.pre {
                    for (i, line) in text.replace('\t', "    ").split('\n').enumerate() {
                        if i > 0 {
                            run.push(Frag::Break);
                        }
                        let mut word = String::new();
                        let mut spaces = 0;
                        for ch in line.chars() {
                            if ch == ' ' {
                                if !word.is_empty() {
                                    run.push(Frag::Word { text: std::mem::take(&mut word), node, link });
                                }
                                spaces += 1;
                            } else {
                                if spaces > 0 {
                                    run.push(Frag::Space { node, count: spaces });
                                    spaces = 0;
                                }
                                word.push(ch);
                            }
                        }
                        if !word.is_empty() {
                            run.push(Frag::Word { text: word, node, link });
                        }
                        if spaces > 0 {
                            run.push(Frag::Space { node, count: spaces });
                        }
                    }
                    return;
                }
                if text.starts_with(char::is_whitespace) {
                    Self::push_space(run, node);
                }
                let mut words = text.split_whitespace().peekable();
                while let Some(w) = words.next() {
                    run.push(Frag::Word { text: w.to_string(), node, link });
                    if words.peek().is_some() {
                        run.push(Frag::Space { node, count: 1 });
                    }
                }
                if text.ends_with(char::is_whitespace) && !text.trim().is_empty() {
                    Self::push_space(run, node);
                }
            }
            Kind::Element { name, .. } => {
                let link = self.link_of(node, link);
                match name.as_str() {
                    "br" => run.push(Frag::Break),
                    "img" => {
                        let alt = self.d.attribute(node, "alt").unwrap_or("").trim();
                        if let Some((width, height)) = self.image_size(node) {
                            run.push(Frag::Image { width, height, node, link });
                        } else if !alt.is_empty() {
                            run.push(Frag::Word { text: format!("[{alt}]"), node, link });
                        }
                    }
                    "input" => {
                        let kind = self.d.attribute(node, "type").unwrap_or("text").to_ascii_lowercase();
                        let label = match kind.as_str() {
                            "submit" | "button" | "reset" => self.d.attribute(node, "value").unwrap_or("Submit").to_string(),
                            "checkbox" | "radio" => "[ ]".to_string(),
                            _ => format!("[{}]", self.d.attribute(node, "value").or(self.d.attribute(node, "placeholder")).unwrap_or("          ")),
                        };
                        run.push(Frag::Word { text: label, node, link });
                    }
                    _ => {
                        for &c in &self.d.node(node).children {
                            if self.styles[c].display != Display::None {
                                self.collect(c, link, run);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn lines(&mut self, run: Vec<Frag>, x: i32, width: i32, y: i32, align: Align) -> i32 {
        if !run.iter().any(|f| matches!(f, Frag::Word { .. } | Frag::Image { .. })) {
            // Only whitespace and breaks: each break after the first word still makes a line,
            // but a run without words takes no room unless it holds explicit breaks.
            let breaks = run.iter().filter(|f| matches!(f, Frag::Break)).count() as i32;
            if breaks > 0 {
                self.margin = 0;
            }
            return y + breaks * (crate::style::Style::root().font_size * 1.2) as i32;
        }
        self.margin = 0;
        // Measure and place.
        struct Placed {
            x: i32,
            width: i32,
            frag: usize,
            text: Option<String>,
        }
        let mut y = y;
        let mut line: Vec<Placed> = Vec::new();
        let mut cursor = 0;
        let flush = |b: &mut Self, line: &mut Vec<Placed>, run: &[Frag], y: &mut i32| {
            while line.last().is_some_and(|p| p.text.is_none()) {
                line.pop();
            }
            let mut ascent = 0;
            let mut descent = 0;
            for p in line.iter() {
                let node = match &run[p.frag] {
                    Frag::Image { height, .. } => {
                        ascent = ascent.max(*height);
                        continue;
                    }
                    Frag::Word { node, .. } | Frag::Space { node, .. } => *node,
                    Frag::Break => continue,
                };
                let f = face(b.fonts, &b.styles[node]);
                let leading = (b.styles[node].font_size * 0.15) as i32;
                ascent = ascent.max(f.ascent() + leading);
                descent = descent.max(-f.descent() + leading);
            }
            if line.is_empty() {
                let base = crate::style::Style::root().font_size;
                *y += (base * 1.2) as i32;
                return;
            }
            let used = line.last().map_or(0, |p| p.x + p.width);
            let shift = match align {
                Align::Left => 0,
                Align::Center => (width - used).max(0) / 2,
                Align::Right => (width - used).max(0),
            };
            let baseline = *y + ascent;
            let placed: Vec<Placed> = line.drain(..).collect();
            for (i, p) in placed.iter().enumerate() {
                if let Frag::Image { width, height, node, link } = &run[p.frag] {
                    let wx = x + shift + p.x;
                    let r = Rect::new(wx, baseline - height, wx + width, baseline);
                    b.out.items.push(Item::Fill { r, color: rgb(0xe4, 0xe0, 0xea) });
                    let _ = node;
                    if let Some(a) = link
                        && let Some(href) = b.d.attribute(*a, "href")
                    {
                        b.out.links.push((r, href.to_string()));
                    }
                    continue;
                }
                let Frag::Word { node, link, .. } = &run[p.frag] else {
                    continue;
                };
                // Underline through the space before the next word of the same link.
                let joined = match (placed.get(i + 1), placed.get(i + 2)) {
                    (Some(space), Some(next)) if space.text.is_none() => match &run[next.frag] {
                        Frag::Word { node: n2, link: l2, .. } => *l2 == *link && b.styles[*n2].underline,
                        _ => false,
                    },
                    _ => false,
                };
                let underline_end = if joined { x + shift + placed[i + 2].x } else { x + shift + p.x + p.width };
                let s = &b.styles[*node];
                let text = p.text.clone().unwrap_or_default();
                let wx = x + shift + p.x;
                let f = face(b.fonts, s);
                if let Some(bg) = s.background {
                    b.out.items.push(Item::Fill { r: Rect::new(wx, baseline - f.ascent(), wx + p.width, baseline - f.descent()), color: bg });
                }
                b.out.items.push(Item::Text { x: wx, baseline, text, size: f.size(), bold: s.bold, color: s.color });
                if s.underline {
                    let t = (s.font_size / 14.0).max(2.0) as i32;
                    let uy = baseline + (s.font_size * 0.12) as i32;
                    b.out.items.push(Item::Fill { r: Rect::new(wx, uy, underline_end, uy + t), color: s.color });
                }
                if let Some(a) = link
                    && let Some(href) = b.d.attribute(*a, "href")
                {
                    let r = Rect::new(wx - 6, baseline - ascent, wx + p.width + 6, baseline + descent);
                    b.out.links.push((r, href.to_string()));
                }
            }
            *y = baseline + descent;
        };
        for (i, frag) in run.iter().enumerate() {
            match frag {
                Frag::Break => {
                    flush(self, &mut line, &run, &mut y);
                    cursor = 0;
                }
                Frag::Space { node, count } => {
                    if cursor == 0 && !self.styles[*node].pre {
                        continue;
                    }
                    let w = face(self.fonts, &self.styles[*node]).width(" ") * *count as i32;
                    line.push(Placed { x: cursor, width: w, frag: i, text: None });
                    cursor += w;
                }
                Frag::Image { width: w, .. } => {
                    let w = (*w).min(width);
                    if cursor + w > width && cursor > 0 {
                        flush(self, &mut line, &run, &mut y);
                        cursor = 0;
                    }
                    line.push(Placed { x: cursor, width: w, frag: i, text: Some(String::new()) });
                    cursor += w;
                }
                Frag::Word { text, node, .. } => {
                    let f = face(self.fonts, &self.styles[*node]);
                    let mut w = f.width(text);
                    if cursor + w > width && cursor > 0 {
                        flush(self, &mut line, &run, &mut y);
                        cursor = 0;
                    }
                    if w <= width {
                        line.push(Placed { x: cursor, width: w, frag: i, text: Some(text.clone()) });
                        cursor += w;
                        continue;
                    }
                    // A word wider than the line: break it between characters.
                    let mut piece = String::new();
                    for ch in text.chars() {
                        piece.push(ch);
                        w = f.width(&piece);
                        if w > width && piece.chars().count() > 1 {
                            piece.pop();
                            let pw = f.width(&piece);
                            line.push(Placed { x: 0, width: pw, frag: i, text: Some(std::mem::take(&mut piece)) });
                            flush(self, &mut line, &run, &mut y);
                            piece.push(ch);
                        }
                    }
                    let pw = f.width(&piece);
                    line.push(Placed { x: 0, width: pw, frag: i, text: Some(piece) });
                    cursor = pw;
                }
            }
        }
        flush(self, &mut line, &run, &mut y);
        y
    }
}
