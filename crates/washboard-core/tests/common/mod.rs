//! Synthetic schema sets shared by the performance tests (PLAN §5.1 "Fixture corpus").

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use washboard_core::model::{SchemaBundle, SchemaDoc, SchemaOrigin};

const XS: &str = "http://www.w3.org/2001/XMLSchema";

pub fn ns(n: usize) -> String {
    format!("urn:synthetic:ns{n}")
}

/// Shape of the generated schema set: `namespaces` target namespaces, each split over
/// `files` documents (file 0 includes the rest and imports every other namespace),
/// `types` complex types per namespace.
pub struct Shape {
    pub namespaces: usize,
    pub files: usize,
    pub types: usize,
}

/// The ~2 MB set the PLAN §5.1 targets refer to.
pub const LARGE: Shape = Shape {
    namespaces: 8,
    files: 5,
    types: 220,
};

/// ` xmlns:p0="…" xmlns:p1="…" …`: prefix `p{n}` for every namespace of `shape`, the prefixes
/// the generated schemas and the tests' instance documents use.
pub fn prefix_decls(shape: &Shape) -> String {
    (0..shape.namespaces)
        .map(|m| format!(r#" xmlns:p{m}="{}""#, ns(m)))
        .collect()
}

/// Prints the size of `bundle` and checks that it is at least the 2 MB the targets are for.
pub fn check_size(bundle: &SchemaBundle) {
    let bytes: usize = bundle.docs.iter().map(|d| d.text.len()).sum();
    println!(
        "bundle: {} documents, {:.2} MB",
        bundle.docs.len(),
        bytes as f64 / 1e6
    );
    assert!(
        bytes >= 2_000_000,
        "bundle should be at least 2 MB: {bytes}"
    );
}

/// Runs `f` `runs` times and returns its last result and the median duration.
pub fn timed<T>(runs: usize, mut f: impl FnMut() -> T) -> (T, Duration) {
    let mut times = Vec::with_capacity(runs);
    let mut last = None;
    for _ in 0..runs {
        let start = Instant::now();
        last = Some(f());
        times.push(start.elapsed());
    }
    times.sort();
    (last.expect("runs > 0"), times[runs / 2])
}

/// One aligned line of timing output; `note` follows the time.
pub fn print_time(label: &str, d: Duration, note: &str) {
    let line = format!("{label:<48} {:>9.2} ms  {note}", d.as_secs_f64() * 1000.0);
    println!("{}", line.trim_end());
}

/// Generates a bundle with the features that make real schemas expensive: namespaces split
/// across included files, cross-namespace references, recursive types, 5-level extension
/// chains, substitution groups whose members live in other namespaces, `xs:any`, attribute
/// groups, enumerations and documentation.
pub fn generate(shape: &Shape) -> SchemaBundle {
    let mut docs = Vec::new();
    let mut root = format!(r#"<xs:schema xmlns:xs="{XS}" targetNamespace="urn:synthetic:root">"#);
    for n in 0..shape.namespaces {
        let mut files: Vec<String> = (0..shape.files).map(|_| String::new()).collect();
        // Locations are relative to the including document, as in real schema sets. Every
        // file imports the other namespaces itself: an import in main.xsd does not extend to
        // the files it includes, and libxml2 rejects such references.
        for (f, file) in files.iter_mut().enumerate() {
            if f == 0 {
                for part in 1..shape.files {
                    let _ = write!(file, r#"<xs:include schemaLocation="part{part}.xsd"/>"#);
                }
            }
            for m in (0..shape.namespaces).filter(|&m| m != n) {
                let _ = write!(
                    file,
                    r#"<xs:import namespace="{}" schemaLocation="../ns{m}/main.xsd"/>"#,
                    ns(m)
                );
            }
        }
        let main = &mut files[0];
        let _ = write!(
            main,
            r#"<xs:complexType name="HeadType" abstract="true"><xs:sequence>
                 <xs:element name="common" type="xs:string"/></xs:sequence></xs:complexType>
               <xs:element name="Head" type="p{n}:HeadType" abstract="true"/>
               <xs:attributeGroup name="AG">
                 <xs:attribute name="id" type="xs:ID" use="required"/>
                 <xs:attribute name="mode" type="p{n}:Enum0"/>
                 <xs:anyAttribute namespace='##other' processContents="lax"/>
               </xs:attributeGroup>"#
        );
        // Members of the previous namespace's substitution group.
        let prev = (n + shape.namespaces - 1) % shape.namespaces;
        for j in 0..20 {
            let _ = write!(
                main,
                r#"<xs:element name="Member{j}" substitutionGroup="p{prev}:Head"><xs:complexType>
                     <xs:complexContent><xs:extension base="p{prev}:HeadType"><xs:sequence>
                       <xs:element name="extra{j}" type="xs:int"/>
                     </xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:element>"#
            );
        }
        for e in 0..10 {
            let _ = write!(
                main,
                r#"<xs:simpleType name="Enum{e}"><xs:annotation><xs:documentation>Enumeration {e}
                   of namespace {n}.</xs:documentation></xs:annotation><xs:restriction base="xs:string">"#
            );
            for v in 0..12 {
                let _ = write!(main, r#"<xs:enumeration value="VALUE_{e}_{v}"/>"#);
            }
            main.push_str("</xs:restriction></xs:simpleType>");
        }
        for i in 0..shape.types {
            let f = i % shape.files;
            let other = (n + 1 + i % (shape.namespaces - 1)) % shape.namespaces;
            let next = (i + 1) % shape.types;
            let doc = &mut files[f];
            let any = if i % 10 == 0 {
                r#"<xs:any namespace='##other' processContents="lax" minOccurs="0" maxOccurs="unbounded"/>"#
            } else {
                ""
            };
            // Attributes only on the base of each chain: an extension that repeats an
            // attribute use is an invalid schema.
            let attrs = if i % 5 == 0 {
                format!(r#"<xs:attributeGroup ref="p{n}:AG"/>"#)
            } else {
                String::new()
            };
            let fields = format!(
                r#"<xs:sequence>
                     <xs:element name="next{i}" type="p{n}:T{next}" minOccurs="0"/>
                     <xs:element name="cross{i}" type="p{other}:T{i}" minOccurs="0"/>
                     <xs:element name="name{i}" type="xs:string"><xs:annotation><xs:documentation>
                       Name field of type {i}, with some documentation text to make the
                       schema look like a real one.</xs:documentation></xs:annotation></xs:element>
                     <xs:element name="code{i}" type="p{n}:Enum{e}"/>
                     <xs:element name="count{i}" type="xs:int" minOccurs="0"/>
                     <xs:element name="date{i}" type="xs:date" minOccurs="0"/>
                     <xs:element ref="p{n}:Head" minOccurs="0" maxOccurs="unbounded"/>
                     <xs:choice><xs:element name="a{i}" type="xs:string"/><xs:element name="b{i}" type="xs:long"/></xs:choice>
                     {any}
                   </xs:sequence>{attrs}"#,
                e = i % 10
            );
            if i % 5 == 0 {
                let _ = write!(
                    doc,
                    r#"<xs:complexType name="T{i}">{fields}</xs:complexType>"#
                );
            } else {
                let _ = write!(
                    doc,
                    r#"<xs:complexType name="T{i}"><xs:complexContent><xs:extension base="p{n}:T{b}">{fields}</xs:extension></xs:complexContent></xs:complexType>"#,
                    b = i - 1
                );
            }
            let _ = write!(doc, r#"<xs:element name="E{i}" type="p{n}:T{i}"/>"#);
        }
        let decls = prefix_decls(shape);
        for (f, body) in files.into_iter().enumerate() {
            let uri = if f == 0 {
                format!("ns{n}/main.xsd")
            } else {
                format!("ns{n}/part{f}.xsd")
            };
            let text = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<xs:schema xmlns:xs="{XS}"{decls} targetNamespace="{}" elementFormDefault="qualified">{body}</xs:schema>"#,
                ns(n)
            );
            docs.push(SchemaDoc {
                uri: uri.clone(),
                target_ns: ns(n),
                origin: SchemaOrigin::File { path: uri },
                text,
            });
        }
        let _ = write!(
            root,
            r#"<xs:import namespace="{}" schemaLocation="ns{n}/main.xsd"/>"#,
            ns(n)
        );
    }
    root.push_str("</xs:schema>");
    docs.push(SchemaDoc {
        uri: "washboard:/root.xsd".into(),
        target_ns: "urn:synthetic:root".into(),
        origin: SchemaOrigin::Generated,
        text: root,
    });
    SchemaBundle {
        docs,
        root: "washboard:/root.xsd".into(),
    }
}
