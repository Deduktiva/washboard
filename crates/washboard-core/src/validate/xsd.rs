//! XSD compilation and validation with libxml2 (WP-LIBXML2).
//!
//! [`CompiledSchema::compile`] turns a [`SchemaBundle`] into a libxml2 schema;
//! [`CompiledSchema::validate`] and [`CompiledSchema::validate_text`] check one document; the
//! request pipeline (`validate::request`) passes the whole SOAP envelope.
//!
//! # Resource loading
//!
//! Only documents in the bundle are ever loaded; a request for anything else fails, and
//! compilation fails with a [`DiagSource::Import`] diagnostic naming the location.
//!
//! The schema parser gets a per-context resource loader (`xmlSchemaSetResourceLoader`,
//! libxml2 >= 2.14). In 2.15.4 that loader is *not* passed on to the temporary contexts libxml2
//! creates for imported/included documents (`xmlSchemaParseNewDoc`), so from the second import
//! level on libxml2 falls back to the process-global external entity loader. We therefore also
//! install a global loader, once, that never touches the file system: it serves the bundle of
//! the compile running on the *current thread* (registered in a thread-local for the duration
//! of [`CompiledSchema::compile`]) and refuses everything otherwise. No locking is needed, and
//! compiles on different threads do not interfere.
//!
//! Instance documents are parsed with `XML_PARSE_NONET | XML_PARSE_NO_XXE` and a loader that
//! refuses everything; external DTDs are never loaded. The vendored libxml2 has no network
//! code at all (see `libxml2-sys`).
//!
//! Bundle URIs are matched literally. libxml2 resolves every `schemaLocation` against the
//! including document's base URI, so each document is handed to libxml2 with the synthetic
//! base `washboard-bundle://bundle/?doc=<index>`: a relative location such as
//! `xsd/common/party.xsd` then resolves to `washboard-bundle://bundle/xsd/common/party.xsd`
//! no matter which document references it, and absolute ones (`washboard:/inline/0.xsd`) stay
//! as they are. Relative bundle URIs must therefore be normalized paths without `..`.
//!
//! # Positions
//!
//! libxml2 records an element's line where its start tag *ends*. Diagnostics instead point at
//! the start tag's `<`: the element a libxml2 error refers to is located by its document-order
//! index among all elements, and the source text is scanned for the start tag with that index.
//! If that fails (undecodable text, entity-expanded content), the start tag ending on the
//! reported line is used, and failing that the raw libxml2 line.
//!
//! # Threads
//!
//! [`CompiledSchema`] is `Send` but not `Sync`: compile on any thread, move it to the thread
//! that validates, and wrap it in a `Mutex` if several threads must share it. libxml2 treats a
//! compiled schema as read-only during validation, but we do not rely on undocumented
//! guarantees. Separate `CompiledSchema`s can be compiled and used on different threads at the
//! same time: per-call state lives in the libxml2 contexts or in thread-locals.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::{self, NonNull};
use std::sync::Once;

use libxml2_sys as ffi;

use crate::diag::{DiagSource, Diagnostic, LineIndex, TextPos, has_errors};
use crate::model::SchemaBundle;

/// Base URI given to every bundle document; see the module docs.
const BASE: &str = "washboard-bundle://bundle/";
/// Base URI of instance documents. Nothing is ever resolved against it.
const INSTANCE_URL: &CStr = c"washboard-instance:/document.xml";
/// Stop collecting after this many diagnostics; libxml2 keeps going after errors and a
/// broken request can produce thousands of follow-up errors.
const MAX_DIAGNOSTICS: usize = 500;
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";
/// `XML_WAR_ENCODING_MISMATCH`: the declaration names another encoding than the BOM we add.
const WAR_ENCODING_MISMATCH: c_int = 113;

/// A libxml2 schema compiled from a [`SchemaBundle`]. See the module docs for thread rules.
pub struct CompiledSchema {
    schema: NonNull<ffi::xmlSchema>,
    warnings: Vec<Diagnostic>,
}

// SAFETY: the xmlSchema is owned exclusively by this value and only used through `&self`
// (validation creates its own validation context per call) or freed in `Drop`. libxml2 is
// built with thread support, and a schema holds no thread-affine state (its dictionary is
// reference-counted under a mutex), so using and freeing it from another thread is fine.
// Not `Sync`: concurrent validations against one schema are not something we rely on.
unsafe impl Send for CompiledSchema {}

impl fmt::Debug for CompiledSchema {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledSchema")
            .field("warnings", &self.warnings)
            .finish_non_exhaustive()
    }
}

impl Drop for CompiledSchema {
    fn drop(&mut self) {
        // SAFETY: `schema` came from xmlSchemaParse and is freed exactly once, here.
        unsafe { ffi::xmlSchemaFree(self.schema.as_ptr()) };
    }
}

impl CompiledSchema {
    /// Compiles the bundle, starting from `bundle.root`.
    ///
    /// Errors (including any attempt to load a document outside the bundle) are returned
    /// together with all warnings, as [`DiagSource::Import`] diagnostics. Their message starts
    /// with the bundle URI of the schema document concerned, and `pos` is a position in *that*
    /// document, not in a request.
    pub fn compile(bundle: &SchemaBundle) -> Result<CompiledSchema, Vec<Diagnostic>> {
        init();
        let Some(root_index) = bundle.docs.iter().position(|d| d.uri == bundle.root) else {
            return Err(vec![Diagnostic::error(
                DiagSource::Import,
                None,
                format!("root schema `{}` is not in the schema bundle", bundle.root),
            )]);
        };
        let Ok(root_url) = CString::new(libxml_url(&bundle.docs[root_index].uri)) else {
            return Err(vec![Diagnostic::error(
                DiagSource::Import,
                None,
                format!("root schema URI `{}` contains a NUL byte", bundle.root),
            )]);
        };

        let loader = BundleLoader {
            bundle,
            refused: RefCell::new(Vec::new()),
        };
        let collector = Collector::new(true);
        let active = ActiveLoader::set(&loader);

        // SAFETY: all pointers passed to libxml2 are valid for the calls; `loader` and
        // `collector` outlive the parser context, which is freed before this block ends.
        let schema = unsafe {
            let pctxt = ffi::xmlSchemaNewParserCtxt(root_url.as_ptr());
            if pctxt.is_null() {
                return Err(vec![oom()]);
            }
            ffi::xmlSchemaSetParserStructuredErrors(
                pctxt,
                Some(on_error),
                ptr::from_ref(&collector).cast_mut().cast(),
            );
            ffi::xmlSchemaSetResourceLoader(
                pctxt,
                Some(load_from_bundle),
                ptr::from_ref(&loader).cast_mut().cast(),
            );
            let schema = ffi::xmlSchemaParse(pctxt);
            ffi::xmlSchemaFreeParserCtxt(pctxt);
            schema
        };

        drop(active);
        let mut diags: Vec<Diagnostic> = loader
            .refused
            .into_inner()
            .into_iter()
            .map(|url| {
                Diagnostic::error(
                    DiagSource::Import,
                    None,
                    format!(
                        "`{}` is not in the schema bundle; only bundle documents are loaded",
                        humanize(&url, bundle)
                    ),
                )
            })
            .collect();
        diags.extend(compile_diagnostics(collector.finish(), bundle));

        let failed = has_errors(&diags);
        match NonNull::new(schema) {
            Some(schema) if !failed => Ok(CompiledSchema {
                schema,
                warnings: diags,
            }),
            other => {
                if let Some(schema) = other {
                    // SAFETY: freshly returned by xmlSchemaParse and not stored anywhere.
                    unsafe { ffi::xmlSchemaFree(schema.as_ptr()) };
                }
                if !failed {
                    diags.push(Diagnostic::error(
                        DiagSource::Import,
                        None,
                        "the schema could not be compiled",
                    ));
                }
                Err(diags)
            }
        }
    }

    /// Warnings libxml2 reported while compiling (e.g. a namespace imported twice from
    /// different locations, whose second location is then skipped).
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }

    /// Validates a standalone XML document against the schema.
    ///
    /// Returns an empty list if the document is valid. A document that is not well-formed
    /// yields [`DiagSource::WellFormedness`] diagnostics and is not validated; schema
    /// violations are [`DiagSource::Schema`]. Element positions point at the start tag's `<`
    /// (module docs). `xml` is passed to libxml2 as is, so BOMs and declared encodings work.
    pub fn validate(&self, xml: &[u8]) -> Vec<Diagnostic> {
        self.validate_with(xml, None, None, || {
            crate::xml::decode(xml)
                .map(|d| scan_start_tags(&d.text))
                .unwrap_or_default()
        })
    }

    /// Like [`Self::validate`] for already decoded text, which is parsed as UTF-8 whatever its
    /// XML declaration says, the same way [`crate::xml::check_well_formed`] parses it. This is
    /// what the editor and the request pipeline hold.
    pub fn validate_text(&self, text: &str) -> Vec<Diagnostic> {
        self.validate_with(text.as_bytes(), Some(c"UTF-8"), None, || {
            scan_start_tags(text)
        })
    }

    /// Like [`Self::validate_text`], but removes the attributes `strip` names from the parsed
    /// document before validating it, so the schema never sees them. Positions are unaffected:
    /// elements keep their line numbers and document order.
    pub fn validate_text_stripping(
        &self,
        text: &str,
        strip: &StripAttributes<'_>,
    ) -> Vec<Diagnostic> {
        self.validate_with(text.as_bytes(), Some(c"UTF-8"), Some(strip), || {
            scan_start_tags(text)
        })
    }

    fn validate_with(
        &self,
        xml: &[u8],
        encoding: Option<&CStr>,
        strip: Option<&StripAttributes<'_>>,
        start_tags: impl FnOnce() -> Vec<StartTag>,
    ) -> Vec<Diagnostic> {
        let (doc, parse_errors) = match parse_instance(xml, encoding) {
            Ok(parsed) => parsed,
            Err(d) => return vec![d],
        };
        let not_well_formed = doc.is_null() || parse_errors.iter().any(RawError::is_error);
        if not_well_formed {
            if !doc.is_null() {
                // SAFETY: returned by xmlCtxtReadMemory, not used afterwards.
                unsafe { ffi::xmlFreeDoc(doc) };
            }
            let mut diags: Vec<Diagnostic> = parse_errors
                .into_iter()
                .map(|e| e.into_diagnostic(DiagSource::WellFormedness, None))
                .collect();
            if !has_errors(&diags) {
                diags.push(Diagnostic::error(
                    DiagSource::WellFormedness,
                    None,
                    "document is not well-formed",
                ));
            }
            return diags;
        }

        if let Some(strip) = strip {
            // SAFETY: `doc` is a valid, well-formed document owned here and not shared.
            unsafe { strip_attributes(doc, strip) };
        }

        let collector = Collector::new(false);
        // SAFETY: `doc` is a valid document owned here; the validation context is freed
        // before the document, and the document is freed exactly once below, after the
        // node pointers collected during validation have been resolved.
        let (ret, raw, ordinals) = unsafe {
            let vctxt = ffi::xmlSchemaNewValidCtxt(self.schema.as_ptr());
            if vctxt.is_null() {
                ffi::xmlFreeDoc(doc);
                return vec![oom()];
            }
            ffi::xmlSchemaSetValidStructuredErrors(
                vctxt,
                Some(on_error),
                ptr::from_ref(&collector).cast_mut().cast(),
            );
            let ret = ffi::xmlSchemaValidateDoc(vctxt, doc);
            ffi::xmlSchemaFreeValidCtxt(vctxt);
            let raw = collector.finish();
            let ordinals = if raw.iter().any(|e| e.node.is_some()) {
                element_ordinals(doc)
            } else {
                HashMap::new()
            };
            ffi::xmlFreeDoc(doc);
            (ret, raw, ordinals)
        };

        let tags = if raw.is_empty() {
            Vec::new()
        } else {
            start_tags()
        };
        let mut diags: Vec<Diagnostic> = raw
            .into_iter()
            .map(|mut e| {
                if let Some(node) = &mut e.node {
                    node.ordinal = ordinals.get(&node.addr).copied();
                }
                let pos = map_pos(&tags, &e);
                e.into_diagnostic(DiagSource::Schema, pos)
            })
            .collect();
        if ret != 0 && !has_errors(&diags) {
            diags.push(Diagnostic::error(
                DiagSource::Schema,
                None,
                if ret < 0 {
                    "internal error in the schema validator"
                } else {
                    "document is not valid"
                },
            ));
        }
        diags
    }
}

/// Attributes to remove before validation: those in `namespace` named in `names`, on every
/// element child of an element named `parent` (namespace URI, local name; `""` for no
/// namespace).
///
/// For attributes a protocol allows on elements whose schema types do not, such as SOAP's
/// `mustUnderstand` on header blocks.
#[derive(Debug, Clone, Copy)]
pub struct StripAttributes<'a> {
    pub parent: (&'a str, &'a str),
    pub namespace: &'a str,
    pub names: &'a [&'a str],
}

/// Removes the attributes `strip` names (see [`StripAttributes`]).
///
/// # Safety
/// `doc` must be a live document that nothing else uses during the call.
unsafe fn strip_attributes(doc: *mut ffi::xmlDoc, strip: &StripAttributes<'_>) {
    let (Ok(ns), Ok(names)) = (
        CString::new(strip.namespace),
        strip
            .names
            .iter()
            .map(|n| CString::new(*n))
            .collect::<Result<Vec<_>, _>>(),
    ) else {
        return; // A NUL byte cannot match any name libxml2 parsed.
    };
    let mut targets = Vec::new();
    // SAFETY: guaranteed by the caller; collecting first keeps the walk off a tree that is
    // being changed.
    unsafe {
        walk_elements(doc, |n, _| {
            let parent = (*n).parent;
            if !parent.is_null()
                && (*parent).type_ == ffi::XML_ELEMENT_NODE
                && element_is(parent, strip.parent.0, strip.parent.1)
            {
                targets.push(n);
            }
            false
        });
        for node in targets {
            for name in &names {
                let attr = ffi::xmlHasNsProp(node, name.as_ptr().cast(), ns.as_ptr().cast());
                if !attr.is_null() {
                    ffi::xmlRemoveProp(attr);
                }
            }
        }
    }
}

/// Whether `node` is the element `{ns}local`.
///
/// # Safety
/// `node` must point to a live element.
unsafe fn element_is(node: *const ffi::xmlNode, ns: &str, local: &str) -> bool {
    // SAFETY: a live element's name is a NUL-terminated string, and its `ns` is null or a
    // live `xmlNs` with a NUL-terminated `href`.
    unsafe {
        let name = (*node).name;
        if name.is_null() || CStr::from_ptr(name.cast()).to_bytes() != local.as_bytes() {
            return false;
        }
        let node_ns = (*node).ns.cast::<ffi::xmlNs>();
        let href = if node_ns.is_null() || (*node_ns).href.is_null() {
            &b""[..]
        } else {
            CStr::from_ptr((*node_ns).href.cast()).to_bytes()
        };
        href == ns.as_bytes()
    }
}

fn init() {
    static INIT: Once = Once::new();
    // SAFETY: xmlInitParser has no preconditions. The global loader is written exactly once,
    // before any of our parsing (`Once` orders it before every later `init()` return), and
    // never changed again.
    INIT.call_once(|| unsafe {
        ffi::xmlInitParser();
        ffi::xmlSetExternalEntityLoader(Some(global_loader));
    });
}

fn oom() -> Diagnostic {
    Diagnostic::error(DiagSource::Schema, None, "libxml2 is out of memory")
}

/// The URL libxml2 sees for a bundle URI: absolute URIs as they are, relative ones under
/// [`BASE`], i.e. in the form libxml2 itself produces when resolving them.
fn libxml_url(uri: &str) -> String {
    if has_scheme(uri) {
        uri.to_owned()
    } else {
        format!("{BASE}{uri}")
    }
}

/// RFC 3986: `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` followed by `:`.
fn has_scheme(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Finds the bundle document a libxml2 URL refers to.
fn lookup(bundle: &SchemaBundle, url: &str) -> Option<usize> {
    if let Some(i) = bundle.docs.iter().position(|d| d.uri == url) {
        return Some(i);
    }
    let rest = url.strip_prefix(BASE)?;
    if let Some(i) = rest.strip_prefix("?doc=").and_then(|n| n.parse().ok()) {
        return Some(i).filter(|&i: &usize| i < bundle.docs.len());
    }
    let rest = percent_encoding::percent_decode_str(rest).decode_utf8_lossy();
    bundle.docs.iter().position(|d| d.uri == rest)
}

/// Rewrites libxml2's internal URLs in a message back to bundle URIs.
fn humanize(msg: &str, bundle: &SchemaBundle) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    while let Some(at) = rest.find(BASE) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let end = tail
            .find(|c: char| c == '\'' || c == '"' || c == '`' || c.is_whitespace())
            .unwrap_or(tail.len());
        let url = &tail[..end];
        match lookup(bundle, url) {
            Some(i) => out.push_str(&bundle.docs[i].uri),
            None => out.push_str(
                &percent_encoding::percent_decode_str(&url[BASE.len()..]).decode_utf8_lossy(),
            ),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

// --- error collection -----------------------------------------------------------------------

/// The element an error refers to.
#[derive(Debug)]
struct NodeRef {
    /// Address of the `xmlNode`; only meaningful while its document is alive.
    addr: usize,
    /// Local name, to cross-check against the scanned start tag.
    local: String,
    /// Document-order index among all elements of its document.
    ordinal: Option<usize>,
    /// URL of the node's document (its base URI).
    doc_url: Option<String>,
}

#[derive(Debug)]
struct RawError {
    level: c_int,
    domain: c_int,
    code: c_int,
    message: String,
    file: Option<String>,
    line: u32,
    column: u32,
    node: Option<NodeRef>,
}

impl RawError {
    fn is_error(&self) -> bool {
        self.level >= ffi::XML_ERR_ERROR
    }

    fn into_diagnostic(self, source: DiagSource, pos: Option<TextPos>) -> Diagnostic {
        let pos = pos.or_else(|| {
            (self.line > 0).then_some(TextPos {
                line: self.line,
                column: self.column.max(1),
            })
        });
        if self.is_error() {
            Diagnostic::error(source, pos, self.message)
        } else {
            Diagnostic::warning(source, pos, self.message)
        }
    }
}

/// Parses an instance document with the safe options (no network, no external entities, a
/// loader that refuses everything). Returns the document, null if parsing failed outright,
/// and the parse errors in document order. The caller owns and frees the document.
///
/// `encoding` overrides the document's own declaration; `None` lets libxml2 detect it.
fn parse_instance(
    xml: &[u8],
    encoding: Option<&CStr>,
) -> Result<(*mut ffi::xmlDoc, Vec<RawError>), Diagnostic> {
    init();
    let Ok(len) = c_int::try_from(xml.len()) else {
        return Err(Diagnostic::error(
            DiagSource::WellFormedness,
            None,
            "document too large to parse",
        ));
    };
    let collector = Collector::new(false);
    let options = ffi::XML_PARSE_NONET | ffi::XML_PARSE_NO_XXE | ffi::XML_PARSE_BIG_LINES;

    // SAFETY: `xml` outlives the parse (libxml2 copies or reads it during the call);
    // `collector` outlives the context, which is freed before this block ends.
    let doc = unsafe {
        let ctxt = ffi::xmlNewParserCtxt();
        if ctxt.is_null() {
            return Err(oom());
        }
        ffi::xmlCtxtSetErrorHandler(
            ctxt,
            Some(on_error),
            ptr::from_ref(&collector).cast_mut().cast(),
        );
        ffi::xmlCtxtSetResourceLoader(ctxt, Some(refuse_all), ptr::null_mut());
        let doc = ffi::xmlCtxtReadMemory(
            ctxt,
            xml.as_ptr().cast::<c_char>(),
            len,
            INSTANCE_URL.as_ptr(),
            encoding.map_or(ptr::null(), CStr::as_ptr),
            options,
        );
        ffi::xmlFreeParserCtxt(ctxt);
        doc
    };
    Ok((doc, collector.finish()))
}

/// The first well-formedness error in already decoded `text`, as libxml2 reports it:
/// `(line, column, message)`, 1-based, 0 when unknown. The text is parsed as UTF-8 whatever
/// its XML declaration says. Used by [`crate::xml::check_well_formed`], so the editor and the
/// validation pipeline judge documents with the same parser.
pub(crate) fn first_well_formedness_error(text: &str) -> Option<(u32, u32, String)> {
    let (doc, errors) = match parse_instance(text.as_bytes(), Some(c"UTF-8")) {
        Ok(parsed) => parsed,
        Err(d) => return Some((0, 0, d.message)),
    };
    let failed = doc.is_null();
    if !failed {
        // SAFETY: returned by xmlCtxtReadMemory and not used afterwards.
        unsafe { ffi::xmlFreeDoc(doc) };
    }
    errors
        .into_iter()
        .find(RawError::is_error)
        .map(|e| (e.line, e.column, e.message))
        .or_else(|| failed.then(|| (0, 0, "document is not well-formed".to_owned())))
}

/// Receives libxml2's structured errors for one context.
struct Collector {
    /// Resolve element ordinals in the callback (compile: the schema documents are freed
    /// before `xmlSchemaParse` returns) instead of afterwards from a single tree walk.
    eager_ordinals: bool,
    errors: RefCell<Vec<RawError>>,
    dropped: RefCell<usize>,
}

impl Collector {
    fn new(eager_ordinals: bool) -> Self {
        Self {
            eager_ordinals,
            errors: RefCell::new(Vec::new()),
            dropped: RefCell::new(0),
        }
    }

    fn finish(self) -> Vec<RawError> {
        let mut errors = self.errors.into_inner();
        let dropped = self.dropped.into_inner();
        if dropped > 0 {
            errors.push(RawError {
                level: ffi::XML_ERR_WARNING,
                domain: 0,
                code: 0,
                message: format!("{dropped} more problems not shown"),
                file: None,
                line: 0,
                column: 0,
                node: None,
            });
        }
        errors
    }

    /// # Safety
    /// `err` must be a valid error as passed to a structured error handler, and any node
    /// it references must still be alive.
    unsafe fn push(&self, err: &ffi::xmlError) {
        let (Ok(mut errors), Ok(mut dropped)) =
            (self.errors.try_borrow_mut(), self.dropped.try_borrow_mut())
        else {
            return;
        };
        if errors.len() >= MAX_DIAGNOSTICS {
            *dropped += 1;
            return;
        }
        // SAFETY: libxml2 strings in the error are NUL-terminated or null.
        let message = unsafe { c_string(err.message) }.unwrap_or_default();
        let node = if matches!(err.domain, ffi::XML_FROM_SCHEMASP | ffi::XML_FROM_SCHEMASV) {
            // SAFETY: for schema errors `node` is null or an xmlNode/xmlAttr of a live doc.
            unsafe { node_ref(err.node.cast(), self.eager_ordinals) }
        } else {
            None
        };
        errors.push(RawError {
            level: err.level,
            domain: err.domain,
            code: err.code,
            message: message.trim_end().to_owned(),
            // SAFETY: as above.
            file: unsafe { c_string(err.file) },
            line: u32::try_from(err.line).unwrap_or(0),
            column: u32::try_from(err.int2).unwrap_or(0),
            node,
        });
    }
}

/// # Safety
/// `p` must be null or point to a NUL-terminated string.
unsafe fn c_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller.
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// # Safety
/// `node` must be null or point to a live `xmlNode` (or `xmlAttr`, which shares the prefix).
unsafe fn node_ref(node: *const ffi::xmlNode, eager: bool) -> Option<NodeRef> {
    if node.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller; attributes' parents are elements of the same doc.
    unsafe {
        let mut node = node;
        if (*node).type_ == ffi::XML_ATTRIBUTE_NODE {
            node = (*node).parent;
            if node.is_null() {
                return None;
            }
        }
        if (*node).type_ != ffi::XML_ELEMENT_NODE || (*node).name.is_null() {
            return None;
        }
        let doc = (*node).doc;
        let doc_url = if doc.is_null() {
            None
        } else {
            c_string((*doc).URL.cast())
        };
        Some(NodeRef {
            addr: node as usize,
            local: c_string((*node).name.cast()).unwrap_or_default(),
            ordinal: if eager { element_ordinal(node) } else { None },
            doc_url,
        })
    }
}

/// Calls `f` for every element of `node`'s document in document order, with its index,
/// until `f` returns `true`. Entity reference contents are not entered: they are not start
/// tags in the source text.
///
/// # Safety
/// `doc` must be a live document whose tree is not modified during the call.
unsafe fn walk_elements(
    doc: *const ffi::xmlDoc,
    mut f: impl FnMut(*const ffi::xmlNode, usize) -> bool,
) {
    // SAFETY: guaranteed by the caller; all links of a live tree are null or valid.
    unsafe {
        let root: *const ffi::xmlNode = ffi::xmlDocGetRootElement(doc);
        let mut cur = root;
        let mut index = 0;
        while !cur.is_null() {
            if (*cur).type_ == ffi::XML_ELEMENT_NODE {
                if f(cur, index) {
                    return;
                }
                index += 1;
                if !(*cur).children.is_null() {
                    cur = (*cur).children;
                    continue;
                }
            }
            loop {
                if cur == root {
                    return;
                }
                if !(*cur).next.is_null() {
                    cur = (*cur).next;
                    break;
                }
                cur = (*cur).parent;
                if cur.is_null() {
                    return;
                }
            }
        }
    }
}

/// # Safety
/// `node` must be an element of a live document.
unsafe fn element_ordinal(node: *const ffi::xmlNode) -> Option<usize> {
    let mut found = None;
    // SAFETY: guaranteed by the caller.
    unsafe {
        walk_elements((*node).doc, |n, i| {
            if n == node {
                found = Some(i);
            }
            found.is_some()
        });
    }
    found
}

/// # Safety
/// `doc` must be a live document.
unsafe fn element_ordinals(doc: *const ffi::xmlDoc) -> HashMap<usize, usize> {
    let mut map = HashMap::new();
    // SAFETY: guaranteed by the caller.
    unsafe {
        walk_elements(doc, |n, i| {
            map.insert(n as usize, i);
            false
        });
    }
    map
}

fn compile_diagnostics(raw: Vec<RawError>, bundle: &SchemaBundle) -> Vec<Diagnostic> {
    let mut tags: HashMap<usize, Vec<StartTag>> = HashMap::new();
    raw.into_iter()
        // Our loader's refusals are reported separately, once per location.
        .filter(|e| e.domain != ffi::XML_FROM_IO)
        .filter(|e| !(e.domain == ffi::XML_FROM_PARSER && e.code == WAR_ENCODING_MISMATCH))
        .map(|mut e| {
            let doc_url = e
                .node
                .as_ref()
                .and_then(|n| n.doc_url.clone())
                .or(e.file.take());
            let doc = doc_url.as_deref().and_then(|u| lookup(bundle, u));
            let pos = doc.and_then(|i| {
                let tags = tags
                    .entry(i)
                    .or_insert_with(|| scan_start_tags(&bundle.docs[i].text));
                map_pos(tags, &e)
            });
            let message = humanize(&e.message, bundle);
            e.message = match doc {
                Some(i) => format!("{}: {message}", bundle.docs[i].uri),
                None => message,
            };
            e.into_diagnostic(DiagSource::Import, pos)
        })
        .collect()
}

// --- resource loaders -----------------------------------------------------------------------

struct BundleLoader<'a> {
    bundle: &'a SchemaBundle,
    refused: RefCell<Vec<String>>,
}

impl BundleLoader<'_> {
    /// Creates a parser input for `url` if it names a bundle document, else records it.
    fn serve(&self, url: &str) -> Result<NonNull<ffi::xmlParserInput>, ffi::xmlParserErrors> {
        let Some(i) = lookup(self.bundle, url) else {
            if let Ok(mut refused) = self.refused.try_borrow_mut()
                && !refused.iter().any(|r| r == url)
            {
                refused.push(url.to_owned());
            }
            return Err(ffi::XML_IO_ENOENT);
        };
        // Texts are decoded already; a BOM makes libxml2 read them as UTF-8 whatever their
        // XML declaration says.
        let text = self.bundle.docs[i].text.trim_start_matches('\u{FEFF}');
        let mut bytes = Vec::with_capacity(UTF8_BOM.len() + text.len());
        bytes.extend_from_slice(UTF8_BOM);
        bytes.extend_from_slice(text.as_bytes());
        let base = CString::new(format!("{BASE}?doc={i}")).map_err(|_| ffi::XML_IO_UNKNOWN)?;
        // SAFETY: both buffers are valid for the call; without XML_INPUT_BUF_STATIC libxml2
        // copies them.
        let input = unsafe {
            ffi::xmlNewInputFromMemory(base.as_ptr(), bytes.as_ptr().cast(), bytes.len(), 0)
        };
        NonNull::new(input).ok_or(ffi::XML_ERR_NO_MEMORY)
    }
}

thread_local! {
    /// The [`BundleLoader`] of the compile running on this thread, for [`global_loader`].
    static ACTIVE_LOADER: Cell<*const c_void> = const { Cell::new(ptr::null()) };
}

/// Registers a loader in [`ACTIVE_LOADER`] until dropped.
struct ActiveLoader {
    previous: *const c_void,
}

impl ActiveLoader {
    /// The guard must be dropped before `loader`; `compile` keeps both on its stack.
    fn set(loader: &BundleLoader<'_>) -> Self {
        let previous = ACTIVE_LOADER.replace(ptr::from_ref(loader).cast());
        Self { previous }
    }
}

impl Drop for ActiveLoader {
    fn drop(&mut self) {
        ACTIVE_LOADER.set(self.previous);
    }
}

/// `xmlResourceLoader` serving bundle documents only.
unsafe extern "C" fn load_from_bundle(
    ctxt: *mut c_void,
    url: *const c_char,
    _public_id: *const c_char,
    _type: ffi::xmlResourceType,
    _flags: ffi::xmlParserInputFlags,
    out: *mut *mut ffi::xmlParserInput,
) -> ffi::xmlParserErrors {
    catch_unwind(AssertUnwindSafe(|| {
        if ctxt.is_null() || out.is_null() {
            return ffi::XML_IO_UNKNOWN;
        }
        // SAFETY: `ctxt` is the BundleLoader registered in `compile`, alive for the parse;
        // `out` is a valid out-pointer; `url` is null or NUL-terminated.
        unsafe {
            *out = ptr::null_mut();
            let loader = &*ctxt.cast_const().cast::<BundleLoader<'_>>();
            match loader.serve(&c_string(url).unwrap_or_default()) {
                Ok(input) => {
                    *out = input.as_ptr();
                    ffi::XML_ERR_OK
                }
                Err(code) => code,
            }
        }
    }))
    .unwrap_or(ffi::XML_IO_UNKNOWN)
}

/// Process-global `xmlExternalEntityLoader`: serves the bundle of the compile running on this
/// thread (see the module docs) and refuses everything else. Never reads files.
unsafe extern "C" fn global_loader(
    url: *const c_char,
    _public_id: *const c_char,
    _context: *mut ffi::xmlParserCtxt,
) -> *mut ffi::xmlParserInput {
    catch_unwind(AssertUnwindSafe(|| {
        let active = ACTIVE_LOADER.get();
        if active.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: a non-null ACTIVE_LOADER points to the BundleLoader on the stack of the
        // `compile` call running on this thread (its guard resets the pointer before the
        // loader goes away); `url` is null or NUL-terminated.
        unsafe {
            let loader = &*active.cast::<BundleLoader<'_>>();
            loader
                .serve(&c_string(url).unwrap_or_default())
                .map_or(ptr::null_mut(), NonNull::as_ptr)
        }
    }))
    .unwrap_or(ptr::null_mut())
}

/// `xmlResourceLoader` for instance documents: they have no business loading anything.
unsafe extern "C" fn refuse_all(
    _ctxt: *mut c_void,
    _url: *const c_char,
    _public_id: *const c_char,
    _type: ffi::xmlResourceType,
    _flags: ffi::xmlParserInputFlags,
    out: *mut *mut ffi::xmlParserInput,
) -> ffi::xmlParserErrors {
    if !out.is_null() {
        // SAFETY: libxml2 passes a valid out-pointer.
        unsafe { *out = ptr::null_mut() };
    }
    ffi::XML_IO_NETWORK_ATTEMPT
}

/// Structured error handler; `user` is a [`Collector`].
unsafe extern "C" fn on_error(user: *mut c_void, err: *const ffi::xmlError) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if user.is_null() || err.is_null() {
            return;
        }
        // SAFETY: `user` is the Collector registered with this context and outlives it;
        // `err` and the nodes it references are valid for the duration of the callback.
        unsafe {
            let collector = &*user.cast_const().cast::<Collector>();
            collector.push(&*err);
        }
    }));
}

// --- start tag positions --------------------------------------------------------------------

/// A start tag in source text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StartTag {
    /// Position of the `<`.
    pos: TextPos,
    /// Line of the closing `>`, which is what libxml2 reports.
    end_line: u32,
    local: String,
}

/// Lists all start tags (including empty-element tags) in document order, with the positions
/// `map_pos` needs. Uses the editor's tokenizer, so comments, CDATA, PIs and the DOCTYPE are
/// skipped the same way everywhere.
fn scan_start_tags(text: &str) -> Vec<StartTag> {
    let lines = LineIndex::new(text);
    crate::xml::start_tags(text)
        .into_iter()
        .map(|t| StartTag {
            pos: lines.pos(t.start),
            end_line: lines.pos(t.end.saturating_sub(1)).line,
            local: t.local(text).to_owned(),
        })
        .collect()
}

/// Start-tag position for an error, if it can be determined.
fn map_pos(tags: &[StartTag], e: &RawError) -> Option<TextPos> {
    if let Some(node) = &e.node {
        if let Some(tag) = node.ordinal.and_then(|i| tags.get(i))
            && tag.local == node.local
        {
            return Some(tag.pos);
        }
        if let Some(tag) = tags
            .iter()
            .find(|t| t.end_line == e.line && t.local == node.local)
        {
            return Some(tag.pos);
        }
    } else if e.domain == ffi::XML_FROM_SCHEMASV
        && let Some(tag) = tags.iter().find(|t| t.end_line == e.line)
    {
        return Some(tag.pos);
    }
    None
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::model::{SchemaDoc, SchemaOrigin};
    use crate::test_support::{fixtures, read_fixture};

    const XS: &str = "http://www.w3.org/2001/XMLSchema";

    fn doc(uri: &str, target_ns: &str, origin: SchemaOrigin, text: String) -> SchemaDoc {
        SchemaDoc {
            uri: uri.into(),
            target_ns: target_ns.into(),
            origin,
            text,
        }
    }

    /// A fixture XSD as WP-WSDL would put it into the bundle: decoded, URI relative to the
    /// project's `wsdl/` folder, `schemaLocation`s rewritten to bundle URIs.
    fn file_doc(uri: &str, target_ns: &str, rewrites: &[(&str, &str)]) -> SchemaDoc {
        let mut text = read_fixture(&format!("customer/{uri}"));
        for (from, to) in rewrites {
            let from = format!("schemaLocation=\"{from}\"");
            assert!(text.contains(&from), "{uri} references {from}");
            text = text.replace(&from, &format!("schemaLocation=\"{to}\""));
        }
        doc(
            uri,
            target_ns,
            SchemaOrigin::File { path: uri.into() },
            text,
        )
    }

    /// Copies `el`'s subtree out of `text` with all in-scope namespace declarations put on
    /// its start tag.
    fn cut_with_namespaces(text: &str, el: roxmltree::Node<'_, '_>) -> String {
        let mut s = text[el.range()].to_owned();
        let tag_end = s.find('>').expect("start tag");
        let name_end = s
            .find(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .expect("tag name");
        let mut decls = String::new();
        for ns in el.namespaces() {
            let attr = match ns.name() {
                Some(p) => format!("xmlns:{p}="),
                None => "xmlns=".to_owned(),
            };
            if ns.name() != Some("xml") && !s[..tag_end].contains(&attr) {
                decls.push_str(&format!(" {attr}\"{}\"", ns.uri()));
            }
        }
        s.insert_str(name_end, &decls);
        s
    }

    /// The inline schema of `CustomerBinding.wsdl` with namespaces carried over.
    fn inline_messages_schema() -> SchemaDoc {
        let wsdl = read_fixture("customer/CustomerBinding.wsdl");
        let parsed = roxmltree::Document::parse(&wsdl).expect("wsdl parses");
        let schema = parsed
            .descendants()
            .find(|n| n.has_tag_name((XS, "schema")))
            .expect("inline schema");
        doc(
            "washboard:/inline/0.xsd",
            "urn:example:customer:messages",
            SchemaOrigin::InlineWsdl {
                wsdl_path: "CustomerBinding.wsdl".into(),
                index: 0,
            },
            cut_with_namespaces(&wsdl, schema),
        )
    }

    fn root_doc(imports: &[(&str, &str)]) -> SchemaDoc {
        let mut text =
            format!("<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:washboard:root\">\n");
        for (ns, loc) in imports {
            text.push_str(&format!(
                "  <xs:import namespace=\"{ns}\" schemaLocation=\"{loc}\"/>\n"
            ));
        }
        text.push_str("</xs:schema>\n");
        doc(
            "washboard:/root.xsd",
            "urn:washboard:root",
            SchemaOrigin::Generated,
            text,
        )
    }

    fn customer_bundle() -> SchemaBundle {
        SchemaBundle {
            docs: vec![
                root_doc(&[
                    ("urn:example:customer:messages", "washboard:/inline/0.xsd"),
                    // Also imported by the inline schema: same location, loaded once.
                    ("urn:example:customer", "xsd/customer.xsd"),
                ]),
                inline_messages_schema(),
                file_doc(
                    "xsd/customer.xsd",
                    "urn:example:customer",
                    &[
                        ("common/party.xsd", "xsd/common/party.xsd"),
                        ("ext/audit.xsd", "xsd/ext/audit.xsd"),
                    ],
                ),
                // Starts with a UTF-8 BOM on disk.
                file_doc(
                    "xsd/common/party.xsd",
                    "urn:example:common",
                    &[("party-ids.xsd", "xsd/common/party-ids.xsd")],
                ),
                file_doc("xsd/common/party-ids.xsd", "urn:example:common", &[]),
                file_doc("xsd/ext/audit.xsd", "urn:example:audit", &[]),
            ],
            root: "washboard:/root.xsd".into(),
        }
    }

    fn single_doc_bundle(text: &str) -> SchemaBundle {
        SchemaBundle {
            docs: vec![doc(
                "washboard:/root.xsd",
                "urn:t",
                SchemaOrigin::Generated,
                text.to_owned(),
            )],
            root: "washboard:/root.xsd".into(),
        }
    }

    const SMALL_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"
           targetNamespace="urn:t" elementFormDefault="qualified">
  <xs:element name="r">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="a" type="xs:string" minOccurs="0"/>
        <xs:element name="n" type="xs:int" maxOccurs="unbounded"/>
      </xs:sequence>
      <xs:attribute name="count" type="xs:positiveInteger"/>
    </xs:complexType>
  </xs:element>
</xs:schema>
"#;

    fn small() -> CompiledSchema {
        CompiledSchema::compile(&single_doc_bundle(SMALL_XSD)).expect("compiles")
    }

    fn pos(line: u32, column: u32) -> Option<TextPos> {
        Some(TextPos { line, column })
    }

    #[test]
    fn not_well_formed_is_reported_with_position() {
        let schema = CompiledSchema::compile(&customer_bundle()).expect("customer compiles");
        let bytes = std::fs::read(fixtures().join("customer/requests/invalid-not-well-formed.xml"))
            .expect("fixture");
        let diags = schema.validate(&bytes);
        assert!(
            diags.iter().any(|d| d.source == DiagSource::WellFormedness
                && d.is_error()
                && d.pos.map(|p| p.line) == Some(9)),
            "{diags:#?}"
        );
    }

    #[test]
    fn multi_line_start_tag_maps_to_its_first_line() {
        let xml = "<r xmlns=\"urn:t\">\n  <n\n     >abc</n>\n</r>\n";
        let diags = small().validate(xml.as_bytes());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].pos, pos(2, 3), "{diags:#?}");
        assert!(diags[0].message.contains("'abc'"), "{diags:#?}");
    }

    #[test]
    fn attribute_errors_point_at_the_element() {
        let xml = "<r xmlns=\"urn:t\"\n   count=\"0\">\n  <n>1</n>\n</r>\n";
        let diags = small().validate(xml.as_bytes());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].pos, pos(1, 1));
        assert!(diags[0].message.contains("count"), "{diags:#?}");
    }

    /// Only the named attributes in the named namespace on children of the named parent go;
    /// everything else is still validated, and positions do not move.
    #[test]
    fn stripped_attributes_are_not_validated() {
        let strip = StripAttributes {
            parent: ("urn:t", "r"),
            namespace: "urn:x",
            names: &["flag"],
        };
        let schema = small();
        let ok = "<r xmlns=\"urn:t\" xmlns:x=\"urn:x\">\n  <n x:flag=\"1\">1</n>\n</r>";
        assert_eq!(schema.validate_text(ok).len(), 1);
        assert_eq!(schema.validate_text_stripping(ok, &strip), Vec::new());

        // Another name, another namespace, or on the parent itself: still an error.
        for bad in [
            "<r xmlns=\"urn:t\" xmlns:x=\"urn:x\">\n  <n x:other=\"1\">1</n>\n</r>",
            "<r xmlns=\"urn:t\" xmlns:y=\"urn:y\">\n  <n y:flag=\"1\">1</n>\n</r>",
            "<r xmlns=\"urn:t\" xmlns:x=\"urn:x\" x:flag=\"1\">\n  <n>1</n>\n</r>",
        ] {
            let diags = schema.validate_text_stripping(bad, &strip);
            assert_eq!(diags.len(), 1, "{bad}: {diags:#?}");
        }

        let diags = schema.validate_text_stripping(
            "<r xmlns=\"urn:t\" xmlns:x=\"urn:x\">\n  <n x:flag=\"1\">no</n>\n</r>",
            &strip,
        );
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].pos, pos(2, 3));
    }

    #[test]
    fn columns_count_chars_and_ignore_the_bom() {
        let xml = "\u{FEFF}<r xmlns=\"urn:t\">\n  <a>üü</a><n\n>x</n>\n</r>";
        let diags = small().validate(xml.as_bytes());
        assert_eq!(diags.len(), 1, "{diags:#?}");
        // `  <a>üü</a>` is 11 chars (13 bytes), so `<n` starts in column 12.
        assert_eq!(diags[0].pos, pos(2, 12));
    }

    #[test]
    fn missing_content_and_unknown_root() {
        let schema = small();
        let diags = schema.validate(b"<r xmlns=\"urn:t\"/>");
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].pos, pos(1, 1));
        let diags = schema.validate(b"<x:other xmlns:x=\"urn:t\"/>");
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert!(
            diags[0].message.contains("No matching global"),
            "{diags:#?}"
        );
        assert!(
            schema
                .validate(b"<r xmlns=\"urn:t\"><n>1</n><n>2</n></r>")
                .is_empty()
        );
    }

    #[test]
    fn utf16_instance_is_read_by_libxml2() {
        let mut bytes = vec![0xFF, 0xFE];
        for u in "<r xmlns=\"urn:t\">\n<n>x</n></r>".encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let diags = small().validate(&bytes);
        assert_eq!(diags.len(), 1, "{diags:#?}");
        assert_eq!(diags[0].pos, pos(2, 1));
    }

    #[test]
    fn instance_documents_load_nothing() {
        let mut secret = tempfile::NamedTempFile::new().expect("temp file");
        std::io::Write::write_all(&mut secret, b"TOP-SECRET").expect("temp file");
        let url = format!("file://{}", secret.path().display());
        let xml = format!(
            "<!DOCTYPE r [<!ENTITY e SYSTEM \"{url}\">]>\n\
             <r xmlns=\"urn:t\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n   \
             xsi:schemaLocation=\"urn:t {url}\"><a>&e;</a><n>1</n></r>"
        );
        let diags = small().validate(xml.as_bytes());
        assert!(
            diags.iter().all(|d| !d.message.contains("TOP-SECRET")),
            "{diags:#?}"
        );
    }

    #[test]
    fn locations_outside_the_bundle_are_refused() {
        let text = format!(
            "<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:t\">\n\
             <xs:import namespace=\"urn:a\" schemaLocation=\"http://example.com/a.xsd\"/>\n\
             <xs:import namespace=\"urn:b\" schemaLocation=\"../../etc/b.xsd\"/>\n\
             <xs:import namespace=\"urn:c\" schemaLocation=\"file:///etc/passwd\"/>\n\
             </xs:schema>"
        );
        let diags = CompiledSchema::compile(&single_doc_bundle(&text)).expect_err("refused");
        for loc in [
            "http://example.com/a.xsd",
            "etc/b.xsd",
            "file:///etc/passwd",
        ] {
            assert!(
                diags.iter().any(|d| d.is_error()
                    && d.source == DiagSource::Import
                    && d.message.contains(loc)
                    && d.message.contains("not in the schema bundle")),
                "{loc}: {diags:#?}"
            );
        }
    }

    /// libxml2 2.15.4 does not hand the per-context loader to the contexts of imported
    /// documents, so this exercises the global fallback loader: an existing file must still
    /// not be loaded from a nested import.
    #[test]
    fn nested_imports_cannot_reach_the_file_system() {
        let mut outside = tempfile::Builder::new()
            .suffix(".xsd")
            .tempfile()
            .expect("temp file");
        std::io::Write::write_all(
            &mut outside,
            format!("<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:o\"/>").as_bytes(),
        )
        .expect("temp file");
        let path = outside.path().display().to_string();
        let mid = |loc: &str| {
            format!(
                "<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:m\">\
                 <xs:import namespace=\"urn:o\" schemaLocation=\"{loc}\"/></xs:schema>"
            )
        };
        let file = |uri: &str| SchemaOrigin::File { path: uri.into() };
        for loc in [path.clone(), format!("file://{path}")] {
            let bundle = SchemaBundle {
                docs: vec![
                    root_doc(&[("urn:m", "a/mid.xsd")]),
                    doc("a/mid.xsd", "urn:m", file("a/mid.xsd"), mid(&loc)),
                ],
                root: "washboard:/root.xsd".into(),
            };
            let diags = CompiledSchema::compile(&bundle).expect_err("refused");
            assert!(
                diags.iter().any(|d| d.is_error()
                    && d.message.contains(path.trim_start_matches('/'))
                    && d.message.contains("not in the schema bundle")),
                "{loc}: {diags:#?}"
            );
        }
    }

    #[test]
    fn root_must_be_in_the_bundle() {
        let mut bundle = single_doc_bundle(SMALL_XSD);
        bundle.root = "washboard:/missing.xsd".into();
        let diags = CompiledSchema::compile(&bundle).expect_err("no root");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn compile_errors_name_the_document_and_start_tag() {
        let bad = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"\n\
                   targetNamespace=\"urn:b\">\n  <xs:element name=\"x\"\n      type=\"xs:nope\"/>\n\
                   </xs:schema>\n";
        let bundle = SchemaBundle {
            docs: vec![
                root_doc(&[("urn:b", "dir/bad.xsd")]),
                doc(
                    "dir/bad.xsd",
                    "urn:b",
                    SchemaOrigin::File {
                        path: "dir/bad.xsd".into(),
                    },
                    bad.into(),
                ),
            ],
            root: "washboard:/root.xsd".into(),
        };
        let diags = CompiledSchema::compile(&bundle).expect_err("bad type");
        let d = diags
            .iter()
            .find(|d| d.message.contains("nope"))
            .unwrap_or_else(|| panic!("{diags:#?}"));
        assert!(d.message.starts_with("dir/bad.xsd: "), "{d:#?}");
        assert_eq!(d.pos, pos(3, 3), "{d:#?}");
        assert!(!d.message.contains(BASE), "{d:#?}");
    }

    #[test]
    fn not_well_formed_schema_document() {
        let bundle = SchemaBundle {
            docs: vec![
                root_doc(&[("urn:b", "bad.xsd")]),
                doc(
                    "bad.xsd",
                    "urn:b",
                    SchemaOrigin::File {
                        path: "bad.xsd".into(),
                    },
                    "<xs:schema\n<".into(),
                ),
            ],
            root: "washboard:/root.xsd".into(),
        };
        let diags = CompiledSchema::compile(&bundle).expect_err("not well-formed");
        assert!(
            diags
                .iter()
                .any(|d| d.message.starts_with("bad.xsd: ") && d.pos.is_some_and(|p| p.line == 2)),
            "{diags:#?}"
        );
    }

    #[test]
    fn declared_encoding_of_decoded_text_is_ignored() {
        let text = format!(
            "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n\
             <xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:t\">\n\
             <xs:element name=\"größe\" type=\"xs:string\"/>\n</xs:schema>"
        );
        let schema = CompiledSchema::compile(&single_doc_bundle(&text)).expect("compiles");
        assert_eq!(schema.warnings(), &[] as &[Diagnostic]);
        assert!(
            schema
                .validate("<größe xmlns=\"urn:t\">x</größe>".as_bytes())
                .is_empty()
        );
    }

    #[test]
    fn second_location_for_a_namespace_is_a_warning() {
        let part = |name: &str| {
            format!(
                "<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:split\">\
                 <xs:element name=\"{name}\"/></xs:schema>"
            )
        };
        let file = |uri: &str| SchemaOrigin::File { path: uri.into() };
        let bundle = SchemaBundle {
            docs: vec![
                root_doc(&[("urn:split", "a.xsd"), ("urn:split", "b.xsd")]),
                doc("a.xsd", "urn:split", file("a.xsd"), part("A")),
                doc("b.xsd", "urn:split", file("b.xsd"), part("B")),
            ],
            root: "washboard:/root.xsd".into(),
        };
        let schema = CompiledSchema::compile(&bundle).expect("compiles");
        assert!(
            schema.warnings().iter().any(|d| !d.is_error()
                && d.message.contains("Skipping import")
                && d.message.contains("b.xsd")),
            "{:#?}",
            schema.warnings()
        );
    }

    #[test]
    fn schemas_compile_and_validate_on_other_threads() {
        let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(small)).collect();
        for h in handles {
            let schema = h.join().expect("thread");
            let moved =
                std::thread::spawn(move || schema.validate(b"<r xmlns=\"urn:t\"><n>1</n></r>"));
            assert!(moved.join().expect("thread").is_empty());
        }
    }

    /// Concurrent compiles must each see only their own bundle, also for nested imports, which
    /// go through the process-global loader (module docs). The bundles below use the same URIs
    /// with different contents, so a loader serving another thread's bundle shows up as the
    /// wrong element being accepted.
    #[test]
    fn concurrent_compiles_resolve_nested_imports_from_their_own_bundle() {
        fn bundle(leaf_element: &str) -> SchemaBundle {
            let generated = || SchemaOrigin::Generated;
            let mid = format!(
                "<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:m\">\
                 <xs:import namespace=\"urn:l\" schemaLocation=\"a/leaf.xsd\"/></xs:schema>"
            );
            let leaf = format!(
                "<xs:schema xmlns:xs=\"{XS}\" targetNamespace=\"urn:l\">\
                 <xs:element name=\"{leaf_element}\" type=\"xs:string\"/></xs:schema>"
            );
            SchemaBundle {
                docs: vec![
                    root_doc(&[("urn:m", "a/mid.xsd")]),
                    doc("a/mid.xsd", "urn:m", generated(), mid),
                    doc("a/leaf.xsd", "urn:l", generated(), leaf),
                ],
                root: "washboard:/root.xsd".into(),
            }
        }
        let instance = |name: &str| format!("<l:{name} xmlns:l=\"urn:l\">x</l:{name}>");

        const THREADS: usize = 8;
        for _ in 0..10 {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
            let handles: Vec<_> = (0..THREADS)
                .map(|i| {
                    let barrier = barrier.clone();
                    let (own, other) = if i % 2 == 0 { ("a", "b") } else { ("b", "a") };
                    std::thread::spawn(move || {
                        let bundle = bundle(own);
                        barrier.wait();
                        let schema = CompiledSchema::compile(&bundle).expect("compiles");
                        (own, other, schema)
                    })
                })
                .collect();
            for h in handles {
                let (own, other, schema) = h.join().expect("thread");
                assert!(
                    schema.validate(instance(own).as_bytes()).is_empty(),
                    "{own}"
                );
                assert!(
                    !schema.validate(instance(other).as_bytes()).is_empty(),
                    "{other}"
                );
            }
        }
    }

    #[test]
    fn start_tag_scanner_skips_markup_that_is_not_a_start_tag() {
        let text = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [\n<!ENTITY x \"<y>\">\n<!-- ] > -->\n]>\n\
                    <r a='>'><!-- <c> --><![CDATA[<d>]]><?pi <e>?>\n<p:f\n/></r>";
        let tags = scan_start_tags(text);
        let got: Vec<_> = tags
            .iter()
            .map(|t| (t.local.as_str(), t.pos.line, t.pos.column, t.end_line))
            .collect();
        assert_eq!(got, [("r", 6, 1, 6), ("f", 7, 1, 8)]);
    }

    #[test]
    fn bundle_urls_round_trip() {
        let bundle = customer_bundle();
        assert_eq!(lookup(&bundle, "washboard:/inline/0.xsd"), Some(1));
        assert_eq!(lookup(&bundle, &libxml_url("xsd/customer.xsd")), Some(2));
        assert_eq!(lookup(&bundle, &format!("{BASE}?doc=3")), Some(3));
        assert_eq!(lookup(&bundle, &format!("{BASE}?doc=99")), None);
        assert_eq!(lookup(&bundle, "customer.xsd"), None);
        assert!(has_scheme("washboard:/root.xsd"));
        assert!(!has_scheme("xsd/a:b.xsd"));
    }
}
