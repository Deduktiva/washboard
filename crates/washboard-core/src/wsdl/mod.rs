//! WSDL 1.1 model: definitions merged across `wsdl:import`, services, ports, bindings,
//! operations, messages, SOAP 1.1 binding details (style, soapAction, body/header parts),
//! plus the import graph, the import check, and the [`SchemaBundle`].
//!
//! Owned by WP-WSDL (`docs/TASKS.md`). Details: `docs/PLAN.md` §4 (import check), §5, §5.3,
//! §5.4.
//!
//! Entry point: [`load`] on a [`Sources`] set. Loading never fails as a whole: malformed or
//! missing files are reported in [`ImportCheck::diagnostics`] and the rest is still
//! analyzed, so the New Project sheet can show everything at once.
//!
//! ```no_run
//! # use std::path::{Path, PathBuf};
//! # use washboard_core::wsdl;
//! let sources = wsdl::Sources::from_disk(Path::new("Svc.wsdl"), &[PathBuf::from("xsd")])?;
//! let w = wsdl::load(&sources);
//! if !w.check.has_errors() {
//!     // copy files according to `w.layout`, compile `w.bundle` …
//! }
//! # Ok::<(), wsdl::SourceError>(())
//! ```

mod bundle;
mod defs;
mod discover;
mod graph;
mod report;
mod source;
mod text;

#[cfg(test)]
mod tests;

use std::collections::HashMap;

use crate::diag::{DiagSource, Diagnostic};
use crate::model::{OperationRef, QName, SchemaBundle};
use crate::xml::Encoding;

pub use bundle::{FILE_URI_PREFIX, ROOT_NS, ROOT_URI, SplitNamespace, file_uri};
pub use defs::{
    AbstractOperation, Binding, Definitions, Direction, HeaderPart, Message, MessageRef, Operation,
    OperationFault, OperationMessage, Part, PartContent, Port, PortType, Protocol, Service, Style,
    Support, UnsupportedReason, Use,
};
pub use discover::{Loaded, load_from_disk};
pub use graph::{FileKind, RefKind, Reference, Resolution, Unresolved, Xsd11Construct};
pub use report::StructuralReport;
pub(crate) use source::split_scheme;
pub use source::{LayoutEntry, MAX_DIR_FILES, MatchedBy, SourceError, SourceFile, Sources};

/// Everything known about a WSDL and its supporting files.
#[derive(Debug, Clone)]
pub struct Wsdl {
    pub definitions: Definitions,
    pub check: ImportCheck,
    /// Closed schema set for libxml2 and the Rust schema model. Only meaningful when
    /// `check.has_errors()` is false; with unresolved references it is incomplete.
    pub bundle: SchemaBundle,
    /// Project-relative destination of every supplied file, parallel to
    /// [`Sources::files`]. Destinations are relative to the deepest directory containing all
    /// supplied files, so relative references keep working after the copy. Files with
    /// [`FileInfo::used`] false may be skipped by the caller.
    pub layout: Vec<LayoutEntry>,
    pub report: StructuralReport,
    dispatch: HashMap<QName, Vec<Dispatch>>,
}

/// The import check shown in the New Project / Replace WSDL sheet (PLAN §4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportCheck {
    /// Per supplied file, parallel to [`Sources::files`].
    pub files: Vec<FileInfo>,
    /// Every reference found, in walk order.
    pub references: Vec<Reference>,
    pub split_namespaces: Vec<SplitNamespace>,
    pub xsd11: Vec<Xsd11Construct>,
    /// All findings, with [`DiagSource::Import`]. Positions refer to the file named at the
    /// start of the message.
    pub diagnostics: Vec<Diagnostic>,
}

impl ImportCheck {
    /// Errors block project creation: unreadable files, unresolved or ambiguous references.
    pub fn has_errors(&self) -> bool {
        crate::diag::has_errors(&self.diagnostics)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Project-relative destination; also the name used in messages.
    pub path: String,
    pub kind: FileKind,
    /// Reached from the entry WSDL.
    pub used: bool,
    /// Shortest reference chain from the entry WSDL (entry = 0).
    pub depth: u32,
    pub encoding: Option<Encoding>,
    pub had_bom: bool,
    pub target_namespace: Option<String>,
}

/// Where a request body element is dispatched to (PLAN §4 "Validation semantics" step 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    pub operation: OperationRef,
    pub style: Style,
    /// `None` means send `SOAPAction: ""`.
    pub soap_action: Option<String>,
    /// Elements of the input's `soap:header` parts; header parts declared with `type=`
    /// cannot be validated and are left out.
    pub header_elements: Vec<QName>,
}

/// Analyzes the entry WSDL and everything it references within `sources`.
pub fn load(sources: &Sources) -> Wsdl {
    let files = sources.files();
    let layout = source::layout(files);
    let names: Vec<String> = layout.iter().map(|l| l.dest.clone()).collect();

    let walk = graph::walk(files, &names);
    let mut diags = walk.diags.clone();

    let wsdl_files: Vec<(usize, &str, &str)> = walk
        .wsdl_order
        .iter()
        .filter_map(|&f| {
            walk.files[f]
                .text
                .as_deref()
                .map(|t| (f, names[f].as_str(), t))
        })
        .collect();
    let definitions = defs::build(&wsdl_files, &mut diags);
    let built = bundle::build(&walk, &names, &definitions, &mut diags);

    for x in &walk.xsd11 {
        diags.push(Diagnostic::warning(
            DiagSource::Import,
            Some(x.pos),
            format!(
                "{}: XSD 1.1 construct {} is not supported (validation is XSD 1.0)",
                x.file, x.construct
            ),
        ));
    }
    for s in &built.splits {
        diags.push(Diagnostic::warning(
            DiagSource::Import,
            None,
            format!(
                "namespace {:?} is split across separately imported documents ({}); they are \
                 combined for validation",
                s.namespace,
                s.documents.join(", ")
            ),
        ));
    }
    for b in &definitions.bindings {
        let unsupported: Vec<&Operation> =
            b.operations.iter().filter(|o| !o.is_supported()).collect();
        if let Some(first) = unsupported.first()
            && let Support::Unsupported(reason) = &first.support
        {
            let why = match (&b.protocol, reason) {
                (Protocol::Soap11, UnsupportedReason::Invalid(_)) => {
                    "the WSDL is inconsistent".to_owned()
                }
                (_, r) => r.to_string(),
            };
            diags.push(Diagnostic::warning(
                DiagSource::Import,
                None,
                format!(
                    "binding {}: {} of {} operations listed as unsupported ({why})",
                    b.name.local,
                    unsupported.len(),
                    b.operations.len()
                ),
            ));
        }
    }

    let dispatch = dispatch_index(&definitions);
    let check = ImportCheck {
        files: files
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let st = &walk.files[i];
                FileInfo {
                    path: names[i].clone(),
                    kind: st.kind,
                    used: st.reached,
                    depth: st.depth,
                    encoding: st.encoding,
                    had_bom: st.had_bom,
                    target_namespace: matches!(st.kind, FileKind::Wsdl | FileKind::Xsd)
                        .then(|| st.tns.clone()),
                }
            })
            .collect(),
        references: walk.refs.clone(),
        split_namespaces: built.splits,
        xsd11: walk.xsd11.clone(),
        diagnostics: diags,
    };
    let report = report::build(
        files,
        &walk,
        &definitions,
        &check,
        &built.bundle,
        built.rpc_wrappers,
    );
    Wsdl {
        definitions,
        check,
        bundle: built.bundle,
        layout,
        report,
        dispatch,
    }
}

fn dispatch_index(defs: &Definitions) -> HashMap<QName, Vec<Dispatch>> {
    let mut ix: HashMap<QName, Vec<Dispatch>> = HashMap::new();
    for b in &defs.bindings {
        for op in b.operations.iter().filter(|o| o.is_supported()) {
            let header_elements = op
                .input
                .as_ref()
                .map(|m| {
                    m.headers
                        .iter()
                        .filter_map(|h| match &h.content {
                            PartContent::Element(q) => Some(q.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let d = Dispatch {
                operation: b.operation_ref(op),
                style: op.style,
                soap_action: op.soap_action.clone(),
                header_elements,
            };
            for q in op.body_elements(Direction::Input) {
                ix.entry(q).or_default().push(d.clone());
            }
        }
    }
    ix
}

impl Wsdl {
    /// The operation for a request body element. Several supported operations may share a
    /// body element (one portType bound twice); `hint` (the request's remembered operation)
    /// picks among them, otherwise the first in document order wins.
    pub fn dispatch(&self, body_element: &QName, hint: Option<&OperationRef>) -> Option<&Dispatch> {
        let all = self.dispatch.get(body_element)?;
        hint.and_then(|h| all.iter().find(|d| &d.operation == h))
            .or_else(|| all.first())
    }

    /// All supported operations whose input body element is `body_element`.
    pub fn dispatch_all(&self, body_element: &QName) -> &[Dispatch] {
        self.dispatch.get(body_element).map_or(&[], Vec::as_slice)
    }

    pub fn operation(&self, r: &OperationRef) -> Option<(&Binding, &Operation)> {
        let b = self.definitions.binding(&r.binding)?;
        let op = b.operations.iter().find(|o| o.name == r.operation)?;
        Some((b, op))
    }

    /// Project-relative destination of the entry WSDL.
    pub fn entry_dest(&self) -> &str {
        self.layout.first().map_or("", |l| l.dest.as_str())
    }

    /// Every supported operation, in document order: what a project can create requests for.
    pub fn supported_operations(&self) -> Vec<OperationRef> {
        self.definitions
            .bindings
            .iter()
            .flat_map(|b| {
                b.operations
                    .iter()
                    .filter(|o| o.is_supported())
                    .map(|o| b.operation_ref(o))
            })
            .collect()
    }

    /// `(port name, address)` of every port bound to a SOAP 1.1 binding, offered as servers
    /// when a project is created.
    pub fn soap11_addresses(&self) -> Vec<(String, String)> {
        self.definitions
            .services
            .iter()
            .flat_map(|s| &s.ports)
            .filter(|p| {
                self.definitions
                    .binding(&p.binding)
                    .is_some_and(|b| b.protocol == Protocol::Soap11)
            })
            .filter_map(|p| Some((p.name.clone(), p.address.clone()?)))
            .collect()
    }
}
