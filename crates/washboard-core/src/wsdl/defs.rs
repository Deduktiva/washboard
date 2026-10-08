//! WSDL 1.1 definitions, merged across the `wsdl:import` closure by QName.
//!
//! Operations are resolved here into what the rest of the app needs (body parts, headers,
//! soapAction, style). Operations we cannot handle are kept and marked with a [`Support`]
//! reason so the UI can list them greyed out (`docs/PLAN.md` §1, §5.3).

use std::collections::HashMap;

use crate::diag::{DiagSource, Diagnostic, TextPos};
use crate::model::QName;
use crate::soap::{WSDL_NS, WSDL_SOAP11_NS, WSDL_SOAP12_NS};

use crate::diag::LineIndex;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Definitions {
    pub services: Vec<Service>,
    pub bindings: Vec<Binding>,
    pub port_types: Vec<PortType>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub name: QName,
    pub ports: Vec<Port>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    pub name: String,
    pub binding: QName,
    /// `soap:address` / `soap12:address` / other `*:address` location. Pre-fills the default
    /// server; never contacted implicitly.
    pub address: Option<String>,
}

/// The binding's wire protocol, from its extension element.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Protocol {
    Soap11,
    Soap12,
    /// HTTP GET/POST bindings and anything else; `namespace` of the extension element,
    /// empty when there is none.
    Other {
        namespace: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Style {
    Document,
    Rpc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Use {
    Literal,
    Encoded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: QName,
    pub port_type: QName,
    pub protocol: Protocol,
    /// `soap:binding style`; operations may override it.
    pub style: Style,
    pub transport: Option<String>,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Operation {
    pub name: String,
    /// Effective style: `soap:operation style`, else the binding's.
    pub style: Style,
    /// `None` when absent or empty; sent as `SOAPAction: ""` then.
    pub soap_action: Option<String>,
    pub input: Option<OperationMessage>,
    pub output: Option<OperationMessage>,
    pub faults: Vec<OperationFault>,
    pub support: Support,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Input,
    Output,
}

impl Operation {
    pub fn is_supported(&self) -> bool {
        self.support == Support::Supported
    }

    pub fn message(&self, dir: Direction) -> Option<&OperationMessage> {
        match dir {
            Direction::Input => self.input.as_ref(),
            Direction::Output => self.output.as_ref(),
        }
    }

    /// QNames of the elements expected as children of `soap:Body`.
    ///
    /// Document style: the `element` of each body part (parts declared with `type=` cannot
    /// appear and are skipped). Rpc style: the generated wrapper, named after the operation
    /// (`<name>Response` for the output) in the `soap:body namespace` (PLAN §5.4).
    pub fn body_elements(&self, dir: Direction) -> Vec<QName> {
        let Some(m) = self.message(dir) else {
            return Vec::new();
        };
        match self.style {
            Style::Rpc => vec![self.rpc_wrapper(dir)],
            Style::Document => m
                .body_parts
                .iter()
                .filter_map(|p| match &p.content {
                    PartContent::Element(q) => Some(q.clone()),
                    _ => None,
                })
                .collect(),
        }
    }

    /// The rpc wrapper element QName for `dir`, regardless of style.
    pub fn rpc_wrapper(&self, dir: Direction) -> QName {
        let ns = self
            .message(dir)
            .and_then(|m| m.namespace.clone())
            .unwrap_or_default();
        match dir {
            Direction::Input => QName::new(ns, self.name.clone()),
            Direction::Output => QName::new(ns, format!("{}Response", self.name)),
        }
    }
}

/// Input or output of a binding operation, resolved against the portType and message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationMessage {
    pub message: Option<QName>,
    /// `soap:body use`.
    pub usage: Use,
    /// `soap:body namespace` (the rpc wrapper namespace).
    pub namespace: Option<String>,
    /// Parts carried in the body, in order: `soap:body parts="…"` if given, else the
    /// message's parts minus those bound as headers.
    pub body_parts: Vec<Part>,
    pub headers: Vec<HeaderPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderPart {
    pub message: QName,
    pub part: String,
    pub usage: Use,
    /// The part's declaration; `Missing` when the message or part does not exist.
    pub content: PartContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFault {
    pub name: String,
    pub message: Option<QName>,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub name: QName,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub content: PartContent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartContent {
    Element(QName),
    Type(QName),
    /// Neither attribute, or the referenced part does not exist.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortType {
    pub name: QName,
    pub operations: Vec<AbstractOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbstractOperation {
    pub name: String,
    pub input: Option<MessageRef>,
    pub output: Option<MessageRef>,
    pub faults: Vec<MessageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRef {
    pub name: Option<String>,
    pub message: Option<QName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Supported,
    Unsupported(UnsupportedReason),
}

impl Support {
    /// Why the operation can't be used; what the UI and the CLI show next to it.
    pub fn reason(&self) -> Option<&UnsupportedReason> {
        match self {
            Support::Supported => None,
            Support::Unsupported(reason) => Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedReason {
    /// SOAP 1.2 binding.
    Soap12,
    /// HTTP GET/POST or unknown binding.
    NotSoap,
    /// `use="encoded"` on the body or a header (rpc/encoded or document/encoded).
    Encoded,
    /// The WSDL is inconsistent (missing portType operation, message …); details in the
    /// import check diagnostics.
    Invalid(String),
}

impl std::fmt::Display for UnsupportedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnsupportedReason::Soap12 => f.write_str("SOAP 1.2 is not supported"),
            UnsupportedReason::NotSoap => f.write_str("not a SOAP binding"),
            UnsupportedReason::Encoded => f.write_str("use=\"encoded\" is not supported"),
            UnsupportedReason::Invalid(m) => f.write_str(m),
        }
    }
}

impl Definitions {
    pub fn binding(&self, name: &QName) -> Option<&Binding> {
        self.bindings.iter().find(|b| &b.name == name)
    }

    pub fn message(&self, name: &QName) -> Option<&Message> {
        self.messages.iter().find(|m| &m.name == name)
    }

    pub fn port_type(&self, name: &QName) -> Option<&PortType> {
        self.port_types.iter().find(|p| &p.name == name)
    }
}

// ---------------------------------------------------------------------------------------
// Parsing

struct RawBinding {
    name: QName,
    port_type: Option<QName>,
    protocol: Protocol,
    style: Style,
    transport: Option<String>,
    ops: Vec<RawOp>,
    file: usize,
    pos: TextPos,
}

struct RawOp {
    name: String,
    soap_action: Option<String>,
    style: Option<Style>,
    input: Option<RawIo>,
    output: Option<RawIo>,
    faults: Vec<String>,
    pos: TextPos,
}

#[derive(Default)]
struct RawIo {
    name: Option<String>,
    usage: Option<Use>,
    namespace: Option<String>,
    parts: Option<Vec<String>>,
    headers: Vec<RawHeader>,
}

struct RawHeader {
    message: Option<QName>,
    part: String,
    usage: Use,
}

struct Ctx<'a> {
    diags: &'a mut Vec<Diagnostic>,
    name: &'a str,
}

impl Ctx<'_> {
    fn warn(&mut self, pos: TextPos, msg: impl std::fmt::Display) {
        self.diags.push(Diagnostic::warning(
            DiagSource::Import,
            Some(pos),
            format!("{}: {msg}", self.name),
        ));
    }
}

/// Resolves a QName-valued attribute in the scope of `node`.
pub(crate) fn resolve_qname(node: roxmltree::Node<'_, '_>, value: &str) -> Option<QName> {
    let value = value.trim();
    match value.split_once(':') {
        Some((prefix, local)) => {
            let ns = node.lookup_namespace_uri(Some(prefix))?;
            Some(QName::new(ns, local))
        }
        None => Some(QName::new(
            node.lookup_namespace_uri(None).unwrap_or(""),
            value,
        )),
    }
}

fn qattr(
    node: roxmltree::Node<'_, '_>,
    attr: &str,
    ctx: &mut Ctx<'_>,
    lines: &LineIndex<'_>,
) -> Option<QName> {
    let v = node.attribute(attr)?;
    let q = resolve_qname(node, v);
    if q.is_none() {
        ctx.warn(
            lines.pos(node.range().start),
            format_args!("{attr}={v:?} uses an undeclared namespace prefix"),
        );
    }
    q
}

fn is_wsdl(n: &roxmltree::Node<'_, '_>, local: &str) -> bool {
    n.is_element() && n.tag_name().namespace() == Some(WSDL_NS) && n.tag_name().name() == local
}

fn parse_use(n: roxmltree::Node<'_, '_>) -> Use {
    match n.attribute("use").map(str::trim) {
        Some("encoded") => Use::Encoded,
        _ => Use::Literal,
    }
}

fn parse_style(v: Option<&str>) -> Option<Style> {
    match v.map(str::trim) {
        Some("rpc") => Some(Style::Rpc),
        Some("document") => Some(Style::Document),
        _ => None,
    }
}

#[derive(Default)]
struct Raw {
    messages: Vec<Message>,
    port_types: Vec<PortType>,
    bindings: Vec<RawBinding>,
    services: Vec<Service>,
}

/// Builds the merged model from the WSDL files (in closure order; first definition wins).
/// `files` are `(file index, display name, decoded text)`.
pub(crate) fn build(files: &[(usize, &str, &str)], diags: &mut Vec<Diagnostic>) -> Definitions {
    let mut raw = Raw::default();
    for &(fi, name, text) in files {
        let Ok(doc) = crate::xml::parse_wsdl_or_xsd(text) else {
            continue;
        };
        let lines = LineIndex::new(text);
        let mut ctx = Ctx { diags, name };
        parse_file(fi, doc.root_element(), &lines, &mut ctx, &mut raw);
    }
    resolve(raw, files, diags)
}

fn parse_file(
    fi: usize,
    root: roxmltree::Node<'_, '_>,
    lines: &LineIndex<'_>,
    ctx: &mut Ctx<'_>,
    raw: &mut Raw,
) {
    let tns = root.attribute("targetNamespace").unwrap_or("").to_owned();
    for el in root.children().filter(|n| n.is_element()) {
        if el.tag_name().namespace() != Some(WSDL_NS) {
            continue;
        }
        let Some(local) = el.attribute("name").map(str::trim) else {
            if matches!(
                el.tag_name().name(),
                "message" | "portType" | "binding" | "service"
            ) {
                ctx.warn(
                    lines.pos(el.range().start),
                    format_args!("wsdl:{} without a name is ignored", el.tag_name().name()),
                );
            }
            continue;
        };
        let name = QName::new(tns.clone(), local);
        match el.tag_name().name() {
            "message" => raw.messages.push(Message {
                name,
                parts: el
                    .children()
                    .filter(|n| is_wsdl(n, "part"))
                    .map(|p| parse_part(p, ctx, lines))
                    .collect(),
            }),
            "portType" => raw.port_types.push(PortType {
                name,
                operations: el
                    .children()
                    .filter(|n| is_wsdl(n, "operation"))
                    .map(|op| {
                        let mref = |n: roxmltree::Node<'_, '_>, ctx: &mut Ctx<'_>| MessageRef {
                            name: n.attribute("name").map(str::to_owned),
                            message: qattr(n, "message", ctx, lines),
                        };
                        AbstractOperation {
                            name: op.attribute("name").unwrap_or("").to_owned(),
                            input: op
                                .children()
                                .find(|n| is_wsdl(n, "input"))
                                .map(|n| mref(n, ctx)),
                            output: op
                                .children()
                                .find(|n| is_wsdl(n, "output"))
                                .map(|n| mref(n, ctx)),
                            faults: op
                                .children()
                                .filter(|n| is_wsdl(n, "fault"))
                                .map(|n| mref(n, ctx))
                                .collect(),
                        }
                    })
                    .collect(),
            }),
            "binding" => {
                let b = parse_binding(fi, name, el, lines, ctx);
                raw.bindings.push(b);
            }
            "service" => raw.services.push(Service {
                name,
                ports: el
                    .children()
                    .filter(|n| is_wsdl(n, "port"))
                    .filter_map(|p| {
                        let binding = qattr(p, "binding", ctx, lines)?;
                        let address = p
                            .children()
                            .find(|n| {
                                n.is_element()
                                    && n.tag_name().name() == "address"
                                    && n.tag_name().namespace() != Some(WSDL_NS)
                            })
                            .and_then(|a| a.attribute("location"))
                            .map(|s| s.trim().to_owned());
                        Some(Port {
                            name: p.attribute("name").unwrap_or("").to_owned(),
                            binding,
                            address,
                        })
                    })
                    .collect(),
            }),
            _ => {}
        }
    }
}

fn parse_part(p: roxmltree::Node<'_, '_>, ctx: &mut Ctx<'_>, lines: &LineIndex<'_>) -> Part {
    let content = if let Some(q) = qattr(p, "element", ctx, lines) {
        PartContent::Element(q)
    } else if let Some(q) = qattr(p, "type", ctx, lines) {
        PartContent::Type(q)
    } else {
        PartContent::Missing
    };
    Part {
        name: p.attribute("name").unwrap_or("").to_owned(),
        content,
    }
}

fn parse_binding(
    fi: usize,
    name: QName,
    el: roxmltree::Node<'_, '_>,
    lines: &LineIndex<'_>,
    ctx: &mut Ctx<'_>,
) -> RawBinding {
    let ext = el
        .children()
        .find(|n| n.is_element() && n.tag_name().namespace() != Some(WSDL_NS));
    let (protocol, soap_ns) = match ext.and_then(|e| e.tag_name().namespace()) {
        Some(WSDL_SOAP11_NS) => (Protocol::Soap11, Some(WSDL_SOAP11_NS)),
        Some(WSDL_SOAP12_NS) => (Protocol::Soap12, Some(WSDL_SOAP12_NS)),
        ns => (
            Protocol::Other {
                namespace: ns.unwrap_or("").to_owned(),
            },
            None,
        ),
    };
    let is_soap = |n: &roxmltree::Node<'_, '_>, local: &str| {
        n.is_element() && soap_ns.is_some() && n.tag_name().namespace() == soap_ns && {
            n.tag_name().name() == local
        }
    };
    let sb = el.children().find(|n| is_soap(n, "binding"));
    let style = parse_style(sb.and_then(|b| b.attribute("style"))).unwrap_or(Style::Document);
    let transport = sb.and_then(|b| b.attribute("transport")).map(str::to_owned);
    let io = |n: roxmltree::Node<'_, '_>, ctx: &mut Ctx<'_>| {
        let mut r = RawIo {
            name: n.attribute("name").map(str::to_owned),
            ..RawIo::default()
        };
        for c in n.children() {
            if is_soap(&c, "body") {
                r.usage = Some(parse_use(c));
                r.namespace = c.attribute("namespace").map(str::to_owned);
                r.parts = c
                    .attribute("parts")
                    .map(|p| p.split_whitespace().map(str::to_owned).collect());
            } else if is_soap(&c, "header") {
                r.headers.push(RawHeader {
                    message: qattr(c, "message", ctx, lines),
                    part: c.attribute("part").unwrap_or("").trim().to_owned(),
                    usage: parse_use(c),
                });
            }
        }
        r
    };
    let ops = el
        .children()
        .filter(|n| is_wsdl(n, "operation"))
        .map(|op| {
            let so = op.children().find(|n| is_soap(n, "operation"));
            RawOp {
                name: op.attribute("name").unwrap_or("").to_owned(),
                soap_action: so
                    .and_then(|s| s.attribute("soapAction"))
                    .map(str::to_owned)
                    .filter(|s| !s.is_empty()),
                style: parse_style(so.and_then(|s| s.attribute("style"))),
                input: op
                    .children()
                    .find(|n| is_wsdl(n, "input"))
                    .map(|n| io(n, ctx)),
                output: op
                    .children()
                    .find(|n| is_wsdl(n, "output"))
                    .map(|n| io(n, ctx)),
                faults: op
                    .children()
                    .filter(|n| is_wsdl(n, "fault"))
                    .filter_map(|n| n.attribute("name").map(str::to_owned))
                    .collect(),
                pos: lines.pos(op.range().start),
            }
        })
        .collect();
    RawBinding {
        name,
        port_type: qattr(el, "type", ctx, lines),
        protocol,
        style,
        transport,
        ops,
        file: fi,
        pos: lines.pos(el.range().start),
    }
}

// ---------------------------------------------------------------------------------------
// Merging and resolution

fn dedup<T>(
    items: Vec<T>,
    key: impl Fn(&T) -> &QName,
    what: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<T> {
    let mut seen: HashMap<QName, ()> = HashMap::new();
    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let k = key(&it).clone();
        if seen.insert(k.clone(), ()).is_some() {
            diags.push(Diagnostic::warning(
                DiagSource::Import,
                None,
                format!("{what} {k} is defined more than once; the first definition is used"),
            ));
        } else {
            out.push(it);
        }
    }
    out
}

fn resolve(raw: Raw, files: &[(usize, &str, &str)], diags: &mut Vec<Diagnostic>) -> Definitions {
    let messages = dedup(raw.messages, |m| &m.name, "wsdl:message", diags);
    let port_types = dedup(raw.port_types, |p| &p.name, "wsdl:portType", diags);
    let raw_bindings = dedup(raw.bindings, |b| &b.name, "wsdl:binding", diags);
    let services = dedup(raw.services, |s| &s.name, "wsdl:service", diags);

    let msg_ix: HashMap<&QName, &Message> = messages.iter().map(|m| (&m.name, m)).collect();
    let pt_ix: HashMap<&QName, &PortType> = port_types.iter().map(|p| (&p.name, p)).collect();
    let file_name = |fi: usize| {
        files
            .iter()
            .find(|(i, _, _)| *i == fi)
            .map_or("", |(_, n, _)| *n)
    };

    let mut bindings = Vec::with_capacity(raw_bindings.len());
    for rb in raw_bindings {
        let fname = file_name(rb.file);
        let mut warn = |pos: TextPos, msg: String| {
            diags.push(Diagnostic::warning(
                DiagSource::Import,
                Some(pos),
                format!("{fname}: {msg}"),
            ));
        };
        let pt = rb.port_type.as_ref().and_then(|q| pt_ix.get(q).copied());
        if pt.is_none() {
            warn(
                rb.pos,
                format!(
                    "binding {} refers to unknown portType {}",
                    rb.name.local,
                    rb.port_type
                        .as_ref()
                        .map_or_else(|| "(none)".to_owned(), ToString::to_string)
                ),
            );
        }
        let mut operations = Vec::with_capacity(rb.ops.len());
        for op in rb.ops {
            let style = op.style.unwrap_or(rb.style);
            let in_name = op.input.as_ref().and_then(|i| i.name.as_deref());
            let aop = pt.and_then(|pt| {
                let mut c = pt.operations.iter().filter(|o| o.name == op.name);
                match in_name {
                    Some(n) => pt.operations.iter().find(|o| {
                        o.name == op.name
                            && o.input.as_ref().and_then(|i| i.name.as_deref()) == Some(n)
                    }),
                    None => c.next(),
                }
                .or_else(|| pt.operations.iter().find(|o| o.name == op.name))
            });
            let mut invalid: Option<String> = None;
            if pt.is_some() && aop.is_none() {
                let m = format!("operation {} is not declared in the portType", op.name);
                warn(op.pos, m.clone());
                invalid = Some(m);
            } else if pt.is_none() {
                invalid = Some("the binding's portType is missing".to_owned());
            }
            let mut message_io = |raw_io: Option<RawIo>,
                                  mref: Option<&MessageRef>,
                                  warn: &mut dyn FnMut(TextPos, String)|
             -> Option<OperationMessage> {
                let raw_io = raw_io?;
                let msg_name = mref.and_then(|m| m.message.clone());
                let msg = msg_name.as_ref().and_then(|q| msg_ix.get(q).copied());
                if let (Some(q), None) = (&msg_name, msg) {
                    let m = format!("operation {}: message {q} is not defined", op.name);
                    warn(op.pos, m.clone());
                    invalid.get_or_insert(m);
                }
                let headers: Vec<HeaderPart> = raw_io
                    .headers
                    .iter()
                    .map(|h| {
                        let hm = h.message.as_ref().and_then(|q| msg_ix.get(q).copied());
                        let content = hm
                            .and_then(|m| m.parts.iter().find(|p| p.name == h.part))
                            .map_or(PartContent::Missing, |p| p.content.clone());
                        if content == PartContent::Missing {
                            warn(
                                op.pos,
                                format!(
                                    "operation {}: soap:header part {:?} of message {} \
                                     is not defined",
                                    op.name,
                                    h.part,
                                    h.message
                                        .as_ref()
                                        .map_or_else(|| "(none)".into(), ToString::to_string)
                                ),
                            );
                        }
                        HeaderPart {
                            message: h.message.clone().unwrap_or_else(|| QName::new("", "")),
                            part: h.part.clone(),
                            usage: h.usage,
                            content,
                        }
                    })
                    .collect();
                let all_parts = msg.map_or(&[][..], |m| m.parts.as_slice());
                let body_parts = match &raw_io.parts {
                    Some(names) => names
                        .iter()
                        .filter_map(|n| {
                            let p = all_parts.iter().find(|p| &p.name == n);
                            if p.is_none() {
                                warn(
                                    op.pos,
                                    format!(
                                        "operation {}: soap:body parts names unknown part {n:?}",
                                        op.name
                                    ),
                                );
                            }
                            p.cloned()
                        })
                        .collect(),
                    None => all_parts
                        .iter()
                        .filter(|p| {
                            !headers
                                .iter()
                                .any(|h| Some(&h.message) == msg_name.as_ref() && h.part == p.name)
                        })
                        .cloned()
                        .collect(),
                };
                // A missing soap:body is treated leniently as literal with all parts.
                Some(OperationMessage {
                    message: msg_name,
                    usage: raw_io.usage.unwrap_or(Use::Literal),
                    namespace: raw_io.namespace,
                    body_parts,
                    headers,
                })
            };
            let input = message_io(op.input, aop.and_then(|a| a.input.as_ref()), &mut warn);
            let output = message_io(op.output, aop.and_then(|a| a.output.as_ref()), &mut warn);
            let faults = op
                .faults
                .iter()
                .map(|fname| {
                    let mref = aop.and_then(|a| {
                        a.faults
                            .iter()
                            .find(|f| f.name.as_deref() == Some(fname.as_str()))
                    });
                    let message = mref.and_then(|m| m.message.clone());
                    let parts = message
                        .as_ref()
                        .and_then(|q| msg_ix.get(q))
                        .map(|m| m.parts.clone())
                        .unwrap_or_default();
                    OperationFault {
                        name: fname.clone(),
                        message,
                        parts,
                    }
                })
                .collect();
            let encoded = [&input, &output].iter().any(|m| {
                m.as_ref().is_some_and(|m| {
                    m.usage == Use::Encoded || m.headers.iter().any(|h| h.usage == Use::Encoded)
                })
            });
            let support = match &rb.protocol {
                Protocol::Soap12 => Support::Unsupported(UnsupportedReason::Soap12),
                Protocol::Other { .. } => Support::Unsupported(UnsupportedReason::NotSoap),
                Protocol::Soap11 if encoded => Support::Unsupported(UnsupportedReason::Encoded),
                Protocol::Soap11 => match invalid {
                    Some(m) => Support::Unsupported(UnsupportedReason::Invalid(m)),
                    None => Support::Supported,
                },
            };
            if support == Support::Supported && style == Style::Document {
                for m in [&input, &output].into_iter().flatten() {
                    for p in &m.body_parts {
                        if !matches!(p.content, PartContent::Element(_)) {
                            warn(
                                op.pos,
                                format!(
                                    "operation {}: document-style part {:?} has no element=; \
                                     it cannot be dispatched or validated",
                                    op.name, p.name
                                ),
                            );
                        }
                    }
                }
            }
            operations.push(Operation {
                name: op.name,
                style,
                soap_action: op.soap_action,
                input,
                output,
                faults,
                support,
            });
        }
        bindings.push(Binding {
            name: rb.name,
            port_type: rb.port_type.unwrap_or_else(|| QName::new("", "")),
            protocol: rb.protocol,
            style: rb.style,
            transport: rb.transport,
            operations,
        });
    }
    for s in &services {
        for p in &s.ports {
            if !bindings.iter().any(|b| b.name == p.binding) {
                diags.push(Diagnostic::warning(
                    DiagSource::Import,
                    None,
                    format!(
                        "port {} of service {} refers to unknown binding {}",
                        p.name, s.name.local, p.binding
                    ),
                ));
            }
        }
    }
    Definitions {
        services,
        bindings,
        port_types,
        messages,
    }
}
