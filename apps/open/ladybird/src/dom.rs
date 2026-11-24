//! The document that Ladybird's HTML tree builder builds. In Ladybird the
//! tree builder calls back into LibWeb's C++ DOM through the
//! `ladybird_html_parser_*` functions; this module answers the same calls
//! with a plain Rust node arena. Node handles are indices into the arena
//! plus one, so that 0 can mean "no node", as the tree builder expects.
//! The tree operations follow the fake DOM in Ladybird's own parser tests
//! (Libraries/LibWeb/HTML/Parser/Rust/src/parser/test_host.rs, BSD-2-Clause,
//! the Ladybird developers), extended to keep names, attributes and text.
use std::cell::RefCell;
use std::ffi::c_void;

use libweb_html_tokenizer::code_units::RustFfiHtmlUtf16View;
use libweb_html_tokenizer::parser::{
    rust_html_parser_create, rust_html_parser_destroy, rust_html_parser_pop_all_open_elements,
    rust_html_parser_run_document, rust_html_parser_take_pending_script, rust_html_parser_take_pending_svg_script,
    RustFfiHtmlNamespace, RustFfiHtmlParserAttribute, RustFfiHtmlParserRunResult, RustFfiHtmlQuirksMode,
    RustFfiHtmlShadowRootMode, RustFfiHtmlSlotAssignmentMode,
};
use libweb_html_tokenizer::{rust_html_tokenizer_create_from_utf16, rust_html_tokenizer_destroy, rust_html_tokenizer_insert_eof};

use crate::ak_runtime::string_of;

/// Where the tree builder inserts a child when it means "at the end".
const APPEND: usize = usize::MAX;
pub const DOCUMENT: usize = 1;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Document,
    DocumentType,
    Fragment,
    ShadowRoot,
    Element { html: bool, name: String, attributes: Vec<(String, String)> },
    Text(String),
    Comment,
}

#[derive(Debug)]
pub struct Node {
    pub kind: Kind,
    pub parent: usize,
    pub children: Vec<usize>,
    fragment_host: usize,
    template_content: usize,
    shadow_root: usize,
}

#[derive(Debug)]
pub struct Document {
    pub nodes: Vec<Node>,
}

impl Document {
    fn new() -> Document {
        let mut d = Document { nodes: Vec::new() };
        d.create(Kind::Document);
        d
    }

    fn create(&mut self, kind: Kind) -> usize {
        self.nodes.push(Node { kind, parent: 0, children: Vec::new(), fragment_host: 0, template_content: 0, shadow_root: 0 });
        self.nodes.len()
    }

    pub fn node(&self, handle: usize) -> &Node {
        &self.nodes[handle - 1]
    }

    fn node_mut(&mut self, handle: usize) -> &mut Node {
        &mut self.nodes[handle - 1]
    }

    /// The element's local name, or "" for other nodes.
    pub fn name(&self, handle: usize) -> &str {
        match &self.node(handle).kind {
            Kind::Element { name, .. } => name,
            _ => "",
        }
    }

    pub fn attribute(&self, handle: usize, attribute: &str) -> Option<&str> {
        match &self.node(handle).kind {
            Kind::Element { attributes, .. } => {
                attributes.iter().find(|(k, _)| k == attribute).map(|(_, v)| v.as_str())
            }
            _ => None,
        }
    }

    /// The first element with this local name, in tree order.
    pub fn find(&self, from: usize, name: &str) -> Option<usize> {
        if self.name(from) == name {
            return Some(from);
        }
        self.node(from).children.iter().find_map(|&c| self.find(c, name))
    }

    /// All the text under a node.
    pub fn text(&self, handle: usize) -> String {
        match &self.node(handle).kind {
            Kind::Text(t) => t.clone(),
            _ => self.node(handle).children.iter().map(|&c| self.text(c)).collect(),
        }
    }

    pub fn title(&self) -> String {
        self.find(DOCUMENT, "title").map(|t| self.text(t).split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()
    }

    pub fn elements(&self) -> usize {
        self.nodes.iter().filter(|n| matches!(n.kind, Kind::Element { .. })).count()
    }

    fn index_in_parent(&self, node: usize) -> usize {
        let parent = self.node(node).parent;
        self.node(parent).children.iter().position(|&c| c == node).unwrap()
    }

    fn has_element_child(&self, node: usize) -> bool {
        self.node(node).children.iter().any(|&c| matches!(self.node(c).kind, Kind::Element { .. }))
    }

    fn is_host_including_inclusive_ancestor(&self, ancestor: usize, node: usize) -> bool {
        let mut current = node;
        while current != 0 {
            if current == ancestor {
                return true;
            }
            let n = self.node(current);
            current = if n.parent != 0 { n.parent } else { n.fragment_host };
        }
        false
    }

    fn child_at(&self, parent: usize, offset: usize) -> Option<usize> {
        if offset == APPEND { None } else { self.node(parent).children.get(offset).copied() }
    }

    fn child_before(&self, parent: usize, offset: usize) -> Option<usize> {
        let children = &self.node(parent).children;
        match offset {
            APPEND => children.last().copied(),
            0 => None,
            _ => children.get(offset - 1).copied(),
        }
    }

    fn remove(&mut self, node: usize) {
        let parent = self.node(node).parent;
        if parent == 0 {
            return;
        }
        let index = self.index_in_parent(node);
        self.node_mut(parent).children.remove(index);
        self.node_mut(node).parent = 0;
    }

    fn insert_before(&mut self, parent: usize, node: usize, reference: Option<usize>) {
        self.remove(node);
        let index = match reference {
            Some(r) => self.index_in_parent(r),
            None => self.node(parent).children.len(),
        };
        self.node_mut(parent).children.insert(index, node);
        self.node_mut(node).parent = parent;
    }

    fn insert_text(&mut self, parent: usize, offset: usize, data: &str) {
        if self.node(parent).kind == Kind::Document {
            return;
        }
        if let Some(previous) = self.child_before(parent, offset)
            && let Kind::Text(text) = &mut self.node_mut(previous).kind
        {
            text.push_str(data);
            return;
        }
        let reference = self.child_at(parent, offset);
        let text = self.create(Kind::Text(data.to_string()));
        self.insert_before(parent, text, reference);
    }
}

thread_local! {
    static BUILDING: RefCell<Document> = RefCell::new(Document::new());
}

fn with<R>(f: impl FnOnce(&mut Document) -> R) -> R {
    BUILDING.with_borrow_mut(f)
}

fn view(v: &RustFfiHtmlUtf16View) -> String {
    if v.length == 0 {
        return String::new();
    }
    if !v.latin1.is_null() {
        let bytes = unsafe { std::slice::from_raw_parts(v.latin1, v.length) };
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(v.utf16, v.length) })
    }
}

/// Parse a whole document with Ladybird's tokenizer and tree builder.
/// Scripts are not run: the tree builder stops at each one, and the
/// browser simply carries on, as a browser with scripting off would.
pub fn parse(html: &str) -> Document {
    crate::register_names();
    with(|d| *d = Document::new());
    let units: Vec<u16> = html.encode_utf16().collect();
    let tokenizer = rust_html_tokenizer_create_from_utf16(ak::Span::new(&units));
    rust_html_tokenizer_insert_eof(unsafe { tokenizer.as_mut() });
    let parser = rust_html_parser_create();
    let mut host = 0u8;
    let host = (&raw mut host).cast::<c_void>();
    loop {
        let result = unsafe { rust_html_parser_run_document(parser, tokenizer, host, false, true, false) };
        match result {
            RustFfiHtmlParserRunResult::ExecuteScript | RustFfiHtmlParserRunResult::ExecuteSvgScript => {
                rust_html_parser_take_pending_script(unsafe { parser.as_mut() });
                rust_html_parser_take_pending_svg_script(unsafe { parser.as_mut() });
            }
            RustFfiHtmlParserRunResult::Ok | RustFfiHtmlParserRunResult::Unsupported => break,
        }
    }
    unsafe {
        rust_html_parser_pop_all_open_elements(parser);
        rust_html_parser_destroy(parser);
        rust_html_tokenizer_destroy(tokenizer);
    }
    with(|d| std::mem::replace(d, Document::new()))
}

// The host interface of Ladybird's tree builder (Libraries/LibWeb/HTML/Parser/Rust/src/parser.rs).

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_document_node(_parser: *mut c_void) -> usize {
    DOCUMENT
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_document_html_element(_parser: *mut c_void) -> usize {
    with(|d| {
        let first = d.node(DOCUMENT).children.iter().copied().find(|&c| matches!(d.node(c).kind, Kind::Element { .. }));
        match first {
            Some(e) if matches!(&d.node(e).kind, Kind::Element { html: true, name, .. } if name == "html") => e,
            _ => 0,
        }
    })
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_set_document_quirks_mode(_parser: *mut c_void, _mode: RustFfiHtmlQuirksMode) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_document_is_iframe_srcdoc_document(_parser: *mut c_void) -> bool {
    false
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_log_parse_error(_parser: *mut c_void, _message: *const u8, _length: usize) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_stop_parsing(_parser: *mut c_void) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_parse_errors_enabled() -> bool {
    false
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_visit_node(_visitor: *mut c_void, _node: usize) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_create_document_type(
    _parser: *mut c_void,
    _name: RustFfiHtmlUtf16View,
    _public_id: RustFfiHtmlUtf16View,
    _system_id: RustFfiHtmlUtf16View,
) -> usize {
    with(|d| d.create(Kind::DocumentType))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_create_comment(_parser: *mut c_void, _data: RustFfiHtmlUtf16View) -> usize {
    with(|d| d.create(Kind::Comment))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_insert_text(parent: usize, offset: usize, text: RustFfiHtmlUtf16View, _is_ascii: bool) {
    let text = view(&text);
    with(|d| d.insert_text(parent, offset, &text));
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_add_missing_attribute(
    _parser: *mut c_void,
    element: usize,
    local_name: usize,
    value: RustFfiHtmlUtf16View,
) {
    let (name, value) = (string_of(local_name), view(&value));
    with(|d| {
        if let Kind::Element { attributes, .. } = &mut d.node_mut(element).kind
            && !attributes.iter().any(|(k, _)| *k == name)
        {
            attributes.push((name, value));
        }
    });
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_remove_node(node: usize) {
    with(|d| d.remove(node));
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_handle_element_popped(_element: usize) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_set_script_source_line(_parser: *mut c_void, _element: usize, _line: usize) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_mark_script_already_started(_parser: *mut c_void, _element: usize) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_process_meta_element(_parser: *mut c_void, _element: usize) {}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_parent_node(node: usize) -> usize {
    with(|d| d.node(node).parent)
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_element_cannot_be_inserted(target_parent: usize, element: usize) -> bool {
    with(|d| {
        d.node(element).parent != 0
            || d.is_host_including_inclusive_ancestor(element, target_parent)
            || (d.node(target_parent).kind == Kind::Document && d.has_element_child(target_parent))
    })
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_node_index(node: usize) -> usize {
    with(|d| d.index_in_parent(node))
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
extern "C" fn ladybird_html_parser_create_element(
    _parser: *mut c_void,
    _intended_parent: usize,
    namespace: RustFfiHtmlNamespace,
    _namespace_uri: *const u16,
    _namespace_uri_length: usize,
    local_name: usize,
    attributes: *const RustFfiHtmlParserAttribute,
    attribute_count: usize,
    _had_duplicate_attribute: bool,
    _form_element: usize,
    _has_template_element_on_stack: bool,
) -> usize {
    let name = string_of(local_name);
    let list = if attribute_count == 0 { &[][..] } else { unsafe { std::slice::from_raw_parts(attributes, attribute_count) } };
    let attributes = list.iter().map(|a| (string_of(a.local_name), view(&a.value))).collect();
    let html = namespace == RustFfiHtmlNamespace::Html;
    with(|d| d.create(Kind::Element { html, name, attributes }))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_append_child(parent: usize, child: usize) {
    with(|d| d.insert_before(parent, child, None));
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_insert_node(parent: usize, offset: usize, child: usize, _reactions: bool) {
    with(|d| {
        let reference = d.child_at(parent, offset);
        d.insert_before(parent, child, reference);
    });
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_reinsert_last_node(parent: usize, offset: usize, last_node: usize) {
    with(|d| {
        let reference = d.child_at(parent, offset);
        d.remove(last_node);
        let fits = !d.is_host_including_inclusive_ancestor(last_node, parent)
            && (d.node(parent).kind != Kind::Document || !d.has_element_child(parent))
            && reference.is_none_or(|r| d.node(r).parent == parent);
        if fits {
            d.insert_before(parent, last_node, reference);
        }
    });
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_move_all_children(from: usize, to: usize) {
    with(|d| {
        for child in d.node(from).children.clone() {
            d.insert_before(to, child, None);
        }
    });
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_template_content(element: usize) -> usize {
    with(|d| {
        if d.node(element).template_content == 0 {
            let content = d.create(Kind::Fragment);
            d.node_mut(content).fragment_host = element;
            d.node_mut(element).template_content = content;
        }
        d.node(element).template_content
    })
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_attach_declarative_shadow_root(
    host: usize,
    _mode: RustFfiHtmlShadowRootMode,
    _slot_assignment: RustFfiHtmlSlotAssignmentMode,
    _clonable: bool,
    _serializable: bool,
    _delegates_focus: bool,
    _keep_custom_element_registry_null: bool,
) -> usize {
    with(|d| {
        if d.node(host).shadow_root != 0 {
            return 0;
        }
        let root = d.create(Kind::ShadowRoot);
        d.node_mut(root).fragment_host = host;
        d.node_mut(host).shadow_root = root;
        root
    })
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_set_template_content(element: usize, content: usize) {
    with(|d| d.node_mut(element).template_content = content);
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_is_shadow_host(node: usize) -> bool {
    with(|d| d.node(node).shadow_root != 0)
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_sanitizer_action(_parser: *mut c_void, _element: usize) -> u8 {
    0
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_child_at_index(parent: usize, index: usize) -> usize {
    with(|d| d.node(parent).children.get(index).copied().unwrap_or(0))
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_declarative_shadow_root_attributes(_element: usize) -> u32 {
    0
}

#[unsafe(no_mangle)]
extern "C" fn ladybird_html_parser_set_script_text_from_parser(_element: usize) {}
