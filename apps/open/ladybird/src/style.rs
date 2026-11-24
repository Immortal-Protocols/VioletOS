//! Computed style: the HTML user-agent defaults, then author style sheets
//! (`<style>` elements, with simple selectors and `@media` width queries),
//! then `style` attributes. Lengths are converted to device pixels here.
use std::collections::HashMap;

use crate::dom::{Document, Kind};
use violet::{rgb, Color};

/// Device pixels per CSS pixel: the 720-pixel-wide screen is a 360 CSS px viewport.
pub const SCALE: f32 = 2.0;
pub const VIEWPORT_CSS_WIDTH: f32 = 360.0;
/// The height below the browser's toolbar, in CSS pixels.
pub const VIEWPORT_CSS_HEIGHT: f32 = 738.0;
const BASE: f32 = 16.0 * SCALE;
const MAX_RULES: usize = 20000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Display {
    None,
    Block,
    Inline,
    ListItem,
    TableRow,
    TableCell,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub font_size: f32,
    pub bold: bool,
    pub color: Color,
    pub background: Option<Color>,
    pub align: Align,
    pub pre: bool,
    pub underline: bool,
    pub bullets: bool,
    pub margin_top: f32,
    pub margin_bottom: f32,
    pub margin_left: f32,
    pub margin_right: f32,
    pub padding_top: f32,
    pub padding_bottom: f32,
    pub padding_left: f32,
    pub padding_right: f32,
    pub border: Option<Color>,
}

impl Style {
    pub fn root() -> Style {
        Style {
            display: Display::Block,
            font_size: BASE,
            bold: false,
            color: rgb(0, 0, 0),
            background: None,
            align: Align::Left,
            pre: false,
            underline: false,
            bullets: true,
            margin_top: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            margin_right: 0.0,
            padding_top: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            padding_right: 0.0,
            border: None,
        }
    }

    /// What a child starts from: the inherited properties only.
    fn inherit(&self) -> Style {
        Style {
            display: Display::Inline,
            background: None,
            margin_top: 0.0,
            margin_bottom: 0.0,
            margin_left: 0.0,
            margin_right: 0.0,
            padding_top: 0.0,
            padding_bottom: 0.0,
            padding_left: 0.0,
            padding_right: 0.0,
            border: None,
            ..self.clone()
        }
    }

    pub fn is_block(&self) -> bool {
        !matches!(self.display, Display::Inline | Display::None)
    }
}

const HIDDEN: &[&str] = &[
    "head", "script", "style", "title", "meta", "link", "base", "template", "svg", "math", "iframe", "object", "embed",
    "canvas", "video", "audio", "source", "track", "map", "area", "datalist", "param", "noembed", "noframes", "dialog",
];
const BLOCKS: &[&str] = &[
    "html", "body", "div", "p", "h1", "h2", "h3", "h4", "h5", "h6", "ul", "ol", "dl", "dt", "dd", "blockquote", "pre",
    "hr", "table", "thead", "tbody", "tfoot", "caption", "form", "header", "footer", "nav", "section", "article",
    "aside", "main", "figure", "figcaption", "address", "center", "details", "summary", "fieldset", "legend", "hgroup",
    "menu", "dir", "listing", "xmp", "plaintext", "search", "picture", "noscript", "frameset", "frame",
];

/// The HTML user-agent style sheet, approximately.
fn defaults(name: &str, s: &mut Style, attribute: &dyn Fn(&str) -> Option<String>) {
    let em = s.font_size;
    if HIDDEN.contains(&name) || (name == "input" && attribute("type").is_some_and(|t| t.eq_ignore_ascii_case("hidden"))) {
        s.display = Display::None;
        return;
    }
    if BLOCKS.contains(&name) {
        s.display = Display::Block;
    }
    match name {
        "body" => {
            s.margin_top = 8.0 * SCALE;
            s.margin_bottom = 8.0 * SCALE;
            s.margin_left = 8.0 * SCALE;
            s.margin_right = 8.0 * SCALE;
        }
        "p" | "dl" | "pre" | "listing" | "xmp" | "plaintext" | "figure" => {
            s.margin_top = em;
            s.margin_bottom = em;
        }
        "blockquote" => {
            s.margin_top = em;
            s.margin_bottom = em;
            s.margin_left = 40.0 * SCALE;
            s.margin_right = 20.0 * SCALE;
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let (size, margin) = match name {
                "h1" => (2.0, 0.67),
                "h2" => (1.5, 0.83),
                "h3" => (1.17, 1.0),
                "h4" => (1.0, 1.33),
                "h5" => (0.83, 1.67),
                _ => (0.67, 2.33),
            };
            s.font_size = em * size;
            s.bold = true;
            s.margin_top = s.font_size * margin;
            s.margin_bottom = s.font_size * margin;
        }
        "ul" | "ol" | "menu" | "dir" => {
            s.margin_top = em;
            s.margin_bottom = em;
            s.padding_left = 40.0 * SCALE;
        }
        "li" => s.display = Display::ListItem,
        "dd" => s.margin_left = 40.0 * SCALE,
        "hr" => {
            s.margin_top = em * 0.5;
            s.margin_bottom = em * 0.5;
            s.border = Some(rgb(0x99, 0x99, 0x99));
        }
        "tr" => s.display = Display::TableRow,
        "td" | "th" => {
            s.display = Display::TableCell;
            s.padding_top = SCALE;
            s.padding_bottom = SCALE;
            s.padding_left = SCALE;
            s.padding_right = SCALE;
            if name == "th" {
                s.bold = true;
                s.align = Align::Center;
            }
        }
        "caption" | "center" => s.align = Align::Center,
        // As in browsers' quirks style sheet, tables do not inherit <center>.
        "table" => s.align = Align::Left,
        "b" | "strong" | "dt" | "legend" | "summary" => s.bold = true,
        "small" | "sub" | "sup" => s.font_size = em * 0.83,
        "big" => s.font_size = em * 1.2,
        "a" => {
            if attribute("href").is_some() {
                s.color = rgb(0x00, 0x00, 0xee);
                s.underline = true;
            }
        }
        "u" | "ins" => s.underline = true,
        "mark" => s.background = Some(rgb(0xff, 0xff, 0x00)),
        "code" | "kbd" | "samp" | "tt" => s.font_size = em * 0.9,
        "fieldset" => {
            s.margin_left = 2.0 * SCALE;
            s.border = Some(rgb(0xc0, 0xc0, 0xc0));
            s.padding_left = 6.0 * SCALE;
        }
        _ => {}
    }
    if matches!(name, "pre" | "listing" | "xmp" | "plaintext" | "textarea") {
        s.pre = true;
    }
    if let Some(align) = attribute("align") {
        s.align = parse_align(&align).unwrap_or(s.align);
    }
    if let Some(color) = attribute("bgcolor").and_then(|c| parse_color(&c)) {
        s.background = Some(color);
    }
    if name == "font"
        && let Some(color) = attribute("color").and_then(|c| parse_color(&c))
    {
        s.color = color;
    }
}

fn parse_align(v: &str) -> Option<Align> {
    match v.trim().to_ascii_lowercase().as_str() {
        "left" | "start" | "justify" => Some(Align::Left),
        "center" | "middle" | "-webkit-center" => Some(Align::Center),
        "right" | "end" => Some(Align::Right),
        _ => None,
    }
}

pub fn parse_color(v: &str) -> Option<Color> {
    let v = v.trim().to_ascii_lowercase();
    if let Some(inner) = v.strip_prefix("light-dark(") {
        return parse_color(inner.split(',').next().unwrap_or(""));
    }
    if let Some(hex) = v.strip_prefix('#') {
        let digits: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        return match digits.len() {
            3 | 4 => {
                if digits.len() == 4 && digits[3] < 2 {
                    return None;
                }
                Some(rgb(digits[0] * 17, digits[1] * 17, digits[2] * 17))
            }
            6 | 8 => {
                if digits.len() == 8 && digits[6] * 16 + digits[7] < 26 {
                    return None;
                }
                Some(rgb(digits[0] * 16 + digits[1], digits[2] * 16 + digits[3], digits[4] * 16 + digits[5]))
            }
            _ => None,
        };
    }
    if let Some(inner) = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb(")) {
        let parts: Vec<&str> = inner.trim_end_matches(')').split([',', ' ', '/']).filter(|p| !p.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let channel = |p: &str| -> Option<u8> {
            if let Some(pc) = p.strip_suffix('%') {
                Some((pc.parse::<f32>().ok()? * 2.55).clamp(0.0, 255.0) as u8)
            } else {
                Some(p.parse::<f32>().ok()?.clamp(0.0, 255.0) as u8)
            }
        };
        if let Some(alpha) = parts.get(3) {
            let a = alpha.strip_suffix('%').map_or_else(|| alpha.parse::<f32>().ok(), |p| p.parse::<f32>().ok().map(|x| x / 100.0))?;
            if a < 0.1 {
                return None;
            }
        }
        return Some(rgb(channel(parts[0])?, channel(parts[1])?, channel(parts[2])?));
    }
    let named = match v.as_str() {
        "black" => 0x000000,
        "white" => 0xffffff,
        "red" => 0xff0000,
        "green" => 0x008000,
        "blue" => 0x0000ff,
        "yellow" => 0xffff00,
        "orange" => 0xffa500,
        "purple" => 0x800080,
        "gray" | "grey" => 0x808080,
        "silver" => 0xc0c0c0,
        "maroon" => 0x800000,
        "navy" => 0x000080,
        "teal" => 0x008080,
        "olive" => 0x808000,
        "lime" => 0x00ff00,
        "aqua" | "cyan" => 0x00ffff,
        "fuchsia" | "magenta" => 0xff00ff,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "dimgray" | "dimgrey" => 0x696969,
        "whitesmoke" => 0xf5f5f5,
        "gainsboro" => 0xdcdcdc,
        "darkblue" => 0x00008b,
        "darkred" => 0x8b0000,
        "darkgreen" => 0x006400,
        "brown" => 0xa52a2a,
        "pink" => 0xffc0cb,
        "gold" => 0xffd700,
        "beige" => 0xf5f5dc,
        "ivory" => 0xfffff0,
        "linen" => 0xfaf0e6,
        "lavender" => 0xe6e6fa,
        "lightblue" => 0xadd8e6,
        "lightyellow" => 0xffffe0,
        "steelblue" => 0x4682b4,
        "slategray" | "slategrey" => 0x708090,
        "crimson" => 0xdc143c,
        "indigo" => 0x4b0082,
        "violet" => 0xee82ee,
        "orangered" => 0xff4500,
        _ => return None,
    };
    Some(rgb((named >> 16) as u8, (named >> 8) as u8, named as u8))
}

/// A length in device pixels, relative to the element's font size `em`.
fn length(v: &str, em: f32, percent_of: f32) -> Option<f32> {
    let v = v.trim();
    if v == "0" {
        return Some(0.0);
    }
    let number = |suffix: &str| v.strip_suffix(suffix).and_then(|n| n.trim().parse::<f32>().ok());
    if let Some(n) = number("px") {
        return Some(n * SCALE);
    }
    if let Some(n) = number("rem") {
        return Some(n * BASE);
    }
    if let Some(n) = number("em") {
        return Some(n * em);
    }
    if let Some(n) = number("%") {
        return Some(n / 100.0 * percent_of);
    }
    if let Some(n) = number("pt") {
        return Some(n * 4.0 / 3.0 * SCALE);
    }
    if let Some(n) = number("vw") {
        return Some(n / 100.0 * VIEWPORT_CSS_WIDTH * SCALE);
    }
    if let Some(n) = number("vh") {
        return Some(n / 100.0 * VIEWPORT_CSS_HEIGHT * SCALE);
    }
    None
}

fn font_size(v: &str, parent: f32) -> Option<f32> {
    let keyword = match v.trim() {
        "xx-small" => Some(9.0),
        "x-small" => Some(10.0),
        "small" => Some(13.0),
        "medium" => Some(16.0),
        "large" => Some(18.0),
        "x-large" => Some(24.0),
        "xx-large" => Some(32.0),
        "smaller" => return Some(parent * 0.83),
        "larger" => return Some(parent * 1.2),
        _ => None,
    };
    if let Some(k) = keyword {
        return Some(k * SCALE);
    }
    length(v, parent, parent).map(|px| px.clamp(6.0 * SCALE, 64.0 * SCALE))
}

/// Apply one declaration. `parent_size` is the inherited font size, which
/// `em` in font-size refers to.
fn apply(s: &mut Style, property: &str, value: &str, parent_size: f32) {
    let value = value.trim().trim_end_matches("!important").trim();
    let lower = value.to_ascii_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    let em = s.font_size;
    match property {
        "display" => {
            s.display = match first {
                "none" => Display::None,
                "inline" | "inline-block" | "inline-flex" | "contents" | "inline-table" => Display::Inline,
                "list-item" => Display::ListItem,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "block" | "flex" | "grid" | "table" | "flow-root" | "table-row-group" | "table-header-group"
                | "table-footer-group" | "table-caption" => Display::Block,
                _ => s.display,
            }
        }
        "visibility" if first == "hidden" || first == "collapse" => s.display = Display::None,
        "color" => s.color = parse_color(&lower).unwrap_or(s.color),
        "background-color" => s.background = parse_color(&lower).or(if first == "transparent" { None } else { s.background }),
        "background" => {
            if let Some(c) = parse_color(&lower).or_else(|| lower.split_whitespace().find_map(parse_color)) {
                s.background = Some(c);
            } else if first == "none" || first == "transparent" {
                s.background = None;
            }
        }
        "font-weight" => {
            s.bold = match first {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => n.parse::<u32>().map_or(s.bold, |w| w >= 600),
            }
        }
        "font-size" => {
            if let Some(px) = font_size(&lower, parent_size) {
                s.font_size = px;
            }
        }
        "font" => {
            for word in lower.split_whitespace() {
                if word == "bold" || word.parse::<u32>().is_ok_and(|w| w >= 600) {
                    s.bold = true;
                }
                if let Some(px) = font_size(word.split('/').next().unwrap_or(word), parent_size)
                    && word.chars().next().is_some_and(|c| c.is_ascii_digit())
                {
                    s.font_size = px;
                }
            }
        }
        "text-align" => s.align = parse_align(first).unwrap_or(s.align),
        "text-decoration" | "text-decoration-line" => {
            if lower.contains("underline") {
                s.underline = true;
            } else if first == "none" {
                s.underline = false;
            }
        }
        "white-space" => s.pre = matches!(first, "pre" | "pre-wrap" | "pre-line" | "break-spaces"),
        "list-style" | "list-style-type" => s.bullets = !lower.contains("none"),
        "margin" | "padding" => {
            let parts: Vec<f32> = lower.split_whitespace().map(|p| length(p, em, VIEWPORT_CSS_WIDTH * SCALE).unwrap_or(0.0)).collect();
            let (top, right, bottom, left) = match parts.as_slice() {
                [a] => (*a, *a, *a, *a),
                [a, b] => (*a, *b, *a, *b),
                [a, b, c] => (*a, *b, *c, *b),
                [a, b, c, d, ..] => (*a, *b, *c, *d),
                [] => return,
            };
            let clamp = |x: f32| x.clamp(0.0, 160.0 * SCALE);
            if property == "margin" {
                (s.margin_top, s.margin_right, s.margin_bottom, s.margin_left) = (clamp(top), clamp(right), clamp(bottom), clamp(left));
            } else {
                (s.padding_top, s.padding_right, s.padding_bottom, s.padding_left) = (clamp(top), clamp(right), clamp(bottom), clamp(left));
            }
        }
        "margin-top" | "margin-bottom" | "margin-left" | "margin-right" | "padding-top" | "padding-bottom" | "padding-left"
        | "padding-right" => {
            let Some(px) = length(first, em, VIEWPORT_CSS_WIDTH * SCALE) else {
                return;
            };
            let px = px.clamp(0.0, 160.0 * SCALE);
            match property {
                "margin-top" => s.margin_top = px,
                "margin-bottom" => s.margin_bottom = px,
                "margin-left" => s.margin_left = px,
                "margin-right" => s.margin_right = px,
                "padding-top" => s.padding_top = px,
                "padding-bottom" => s.padding_bottom = px,
                "padding-left" => s.padding_left = px,
                _ => s.padding_right = px,
            }
        }
        "border" | "border-top" | "border-bottom" => {
            if first == "none" || first == "0" {
                s.border = None;
            } else if let Some(c) = lower.split_whitespace().find_map(parse_color) {
                s.border = Some(c);
            }
        }
        _ => {}
    }
}

fn declarations(block: &str) -> Vec<(String, String)> {
    block
        .split(';')
        .filter_map(|d| d.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

// Selectors: compound selectors of a type, #id, .class and [attribute], joined
// by descendant or child combinators. Anything else makes the rule not apply.

#[derive(Debug, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<String>,
    link: bool,
    parent_direct: bool,
}

#[derive(Debug)]
struct Selector {
    /// Rightmost first.
    parts: Vec<Compound>,
    specificity: u32,
}

#[derive(Debug)]
struct Rule {
    selector: Selector,
    declarations: Vec<(String, String)>,
    order: usize,
}

fn parse_compound(text: &str) -> Option<Compound> {
    let mut c = Compound::default();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let ident = |i: &mut usize| {
        let start = *i;
        while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '-' || chars[*i] == '_' || chars[*i] as u32 > 0x7f) {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    while i < chars.len() {
        match chars[i] {
            '*' => i += 1,
            '#' => {
                i += 1;
                c.id = Some(ident(&mut i));
            }
            '.' => {
                i += 1;
                c.classes.push(ident(&mut i));
            }
            '[' => {
                let end = chars[i..].iter().position(|&ch| ch == ']')? + i;
                let inner: String = chars[i + 1..end].iter().collect();
                if inner.contains('=') {
                    return None;
                }
                c.attributes.push(inner.trim().to_ascii_lowercase());
                i = end + 1;
            }
            ':' => {
                i += 1;
                if i < chars.len() && chars[i] == ':' {
                    return None;
                }
                match ident(&mut i).to_ascii_lowercase().as_str() {
                    "link" | "any-link" | "visited" => c.link = true,
                    "root" => c.tag = Some("html".into()),
                    _ => return None,
                }
            }
            ch if ch.is_alphabetic() => c.tag = Some(ident(&mut i).to_ascii_lowercase()),
            _ => return None,
        }
    }
    Some(c)
}

fn parse_selector(text: &str) -> Option<Selector> {
    let spaced = text.replace('>', " > ");
    let mut parts: Vec<Compound> = Vec::new();
    let mut child = false;
    for token in spaced.split_whitespace() {
        if token == ">" {
            child = true;
            continue;
        }
        if token.contains(['+', '~']) {
            return None;
        }
        let mut compound = parse_compound(token)?;
        // The compound to the left of this one must be its parent, not just an ancestor.
        compound.parent_direct = child && !parts.is_empty();
        parts.push(compound);
        child = false;
    }
    if parts.is_empty() {
        return None;
    }
    let specificity = parts
        .iter()
        .map(|p| {
            p.id.is_some() as u32 * 10000
                + (p.classes.len() + p.attributes.len() + p.link as usize) as u32 * 100
                + p.tag.is_some() as u32
        })
        .sum();
    parts.reverse();
    Some(Selector { parts, specificity })
}

pub fn media_applies(query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    if q.contains("print") && !q.contains("screen") {
        return false;
    }
    if q.contains("prefers-color-scheme: dark") || q.contains("prefers-color-scheme:dark") {
        return false;
    }
    let width = VIEWPORT_CSS_WIDTH;
    for (feature, at_least) in [("min-width", true), ("max-width", false)] {
        let mut rest = q.as_str();
        while let Some(i) = rest.find(feature) {
            rest = &rest[i + feature.len()..];
            let value: String = rest.trim_start_matches([' ', ':']).chars().take_while(|c| c.is_ascii_digit() || *c == '.' || c.is_alphabetic()).collect();
            let px = length(&value, 16.0, 0.0).map(|d| d / SCALE).unwrap_or(0.0);
            if (at_least && width < px) || (!at_least && width > px) {
                return false;
            }
        }
    }
    true
}

fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        rest = rest[start + 2..].find("*/").map_or("", |end| &rest[start + 2 + end + 2..]);
    }
    out.push_str(rest);
    out
}

fn parse_sheet(css: &str, rules: &mut Vec<Rule>) {
    let css = strip_comments(css);
    let bytes = css.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let Some(open) = css[i..].find('{').map(|o| o + i) else {
            break;
        };
        let prelude = css[i..open].trim();
        // Find the matching close brace.
        let mut depth = 0;
        let mut close = open;
        for (j, &b) in bytes.iter().enumerate().skip(open) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = j;
                        break;
                    }
                }
                _ => {}
            }
        }
        if close == open {
            break;
        }
        let body = &css[open + 1..close];
        if let Some(at) = prelude.strip_prefix('@') {
            if let Some(query) = at.strip_prefix("media")
                && media_applies(query)
            {
                parse_sheet(body, rules);
            }
        } else {
            let declarations = declarations(body);
            for selector in prelude.split(',').filter_map(parse_selector) {
                let order = rules.len();
                rules.push(Rule { selector, declarations: declarations.clone(), order });
            }
        }
        i = close + 1;
    }
}

fn matches_compound(d: &Document, node: usize, c: &Compound) -> bool {
    let Kind::Element { name, .. } = &d.node(node).kind else {
        return false;
    };
    if c.tag.as_ref().is_some_and(|t| t != name) {
        return false;
    }
    if let Some(id) = &c.id
        && d.attribute(node, "id") != Some(id.as_str())
    {
        return false;
    }
    if !c.classes.is_empty() {
        let classes = d.attribute(node, "class").unwrap_or("");
        if !c.classes.iter().all(|wanted| classes.split_whitespace().any(|have| have == wanted)) {
            return false;
        }
    }
    if c.attributes.iter().any(|a| d.attribute(node, a).is_none()) {
        return false;
    }
    !(c.link && (name != "a" || d.attribute(node, "href").is_none()))
}

fn matches(d: &Document, node: usize, s: &Selector) -> bool {
    if !matches_compound(d, node, &s.parts[0]) {
        return false;
    }
    let mut current = node;
    for (i, part) in s.parts.iter().enumerate().skip(1) {
        let direct = s.parts[i - 1].parent_direct;
        loop {
            current = d.node(current).parent;
            if current == 0 || !matches!(d.node(current).kind, Kind::Element { .. }) {
                return false;
            }
            if matches_compound(d, current, part) {
                break;
            }
            if direct {
                return false;
            }
        }
    }
    true
}

pub struct Sheet {
    rules: Vec<Rule>,
    /// Rules by the id, first class or type of their rightmost compound
    /// selector, so that each element is tested only against rules that can match it.
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
}

impl Sheet {
    /// The linked style sheets, then every `<style>` in the document, in order.
    pub fn from_document(d: &Document, linked: &[String]) -> Sheet {
        let mut rules = Vec::new();
        for css in linked {
            parse_sheet(css, &mut rules);
        }
        for (i, n) in d.nodes.iter().enumerate() {
            if let Kind::Element { name, .. } = &n.kind
                && name == "style"
                && d.attribute(i + 1, "media").is_none_or(media_applies)
            {
                parse_sheet(&d.text(i + 1), &mut rules);
            }
        }
        rules.truncate(MAX_RULES);
        let mut sheet = Sheet { rules, by_id: HashMap::new(), by_class: HashMap::new(), by_tag: HashMap::new(), universal: Vec::new() };
        for (i, r) in sheet.rules.iter().enumerate() {
            let c = &r.selector.parts[0];
            if let Some(id) = &c.id {
                sheet.by_id.entry(id.clone()).or_default().push(i);
            } else if let Some(class) = c.classes.first() {
                sheet.by_class.entry(class.clone()).or_default().push(i);
            } else if let Some(tag) = &c.tag {
                sheet.by_tag.entry(tag.clone()).or_default().push(i);
            } else {
                sheet.universal.push(i);
            }
        }
        sheet
    }

    fn candidates(&self, d: &Document, node: usize) -> Vec<usize> {
        let mut out: Vec<usize> = self.universal.clone();
        if let Some(v) = self.by_tag.get(d.name(node)) {
            out.extend(v);
        }
        if let Some(id) = d.attribute(node, "id")
            && let Some(v) = self.by_id.get(id)
        {
            out.extend(v);
        }
        for class in d.attribute(node, "class").unwrap_or("").split_whitespace() {
            if let Some(v) = self.by_class.get(class) {
                out.extend(v);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

/// The computed style of every node, by handle (index 0 is unused).
pub fn compute(d: &Document, sheet: &Sheet) -> Vec<Style> {
    let mut styles = vec![Style::root(); d.nodes.len() + 1];
    fn visit(d: &Document, sheet: &Sheet, node: usize, parent: &Style, styles: &mut Vec<Style>) {
        let mut s = parent.inherit();
        if let Kind::Element { name, .. } = &d.node(node).kind {
            let attribute = |a: &str| d.attribute(node, a).map(str::to_string);
            defaults(name, &mut s, &attribute);
            if s.display != Display::None {
                let mut matched: Vec<&Rule> =
                    sheet.candidates(d, node).into_iter().map(|i| &sheet.rules[i]).filter(|r| matches(d, node, &r.selector)).collect();
                matched.sort_by_key(|r| (r.selector.specificity, r.order));
                let inline = d.attribute(node, "style").map(declarations).unwrap_or_default();
                let all = matched.iter().flat_map(|r| r.declarations.iter()).chain(inline.iter());
                let (important, normal): (Vec<_>, Vec<_>) = all.partition(|(_, v)| v.contains("!important"));
                for (k, v) in normal.into_iter().chain(important) {
                    apply(&mut s, k, v, parent.font_size);
                }
                // Keep text legible on the small screen.
                s.font_size = s.font_size.max(10.0 * SCALE);
            }
        } else {
            s.display = Display::Inline;
        }
        styles[node] = s.clone();
        if s.display == Display::None {
            return;
        }
        for &c in &d.node(node).children {
            visit(d, sheet, c, &s, styles);
        }
    }
    let root = Style::root();
    for &c in &d.node(crate::dom::DOCUMENT).children {
        visit(d, sheet, c, &root, &mut styles);
    }
    styles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors_and_media() {
        assert_eq!(parse_color("#fff"), Some(rgb(255, 255, 255)));
        assert_eq!(parse_color("rgb(10, 20, 30)"), Some(rgb(10, 20, 30)));
        assert_eq!(parse_color("rgba(0,0,0,0)"), None);
        assert!(media_applies("screen and (max-width: 600px)"));
        assert!(!media_applies("(min-width: 1024px)"));
        assert!(!media_applies("print"));
    }

    #[test]
    fn applies_author_styles() {
        let d = crate::dom::parse("<style>.big{font-size:2em} div > p{color:red} #x{display:none}</style><div><p class=big>a</p></div><p id=x>b</p>");
        let sheet = Sheet::from_document(&d, &[]);
        let styles = compute(&d, &sheet);
        let ps: Vec<usize> = (1..=d.nodes.len()).filter(|&n| d.name(n) == "p").collect();
        assert_eq!(styles[ps[0]].color, rgb(255, 0, 0));
        assert_eq!(styles[ps[0]].font_size, 64.0);
        assert_eq!(styles[ps[1]].display, Display::None);
    }
}
