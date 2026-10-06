//! Performance of the Rust schema model on a synthetic ~2 MB bundle (PLAN §5.1).
//!
//! Targets: build < 500 ms, completion queries < 50 ms. Timings are always printed
//! (`cargo test --release --test schema_perf -- --nocapture`) and asserted only in release
//! builds, since debug builds of this crate are unoptimized.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use washboard_core::model::{QName, SchemaBundle, SchemaDoc, SchemaOrigin};
use washboard_core::schema::{PathStep, SchemaModel, TemplateOptions};

const XS: &str = "http://www.w3.org/2001/XMLSchema";

fn ns(n: usize) -> String {
    format!("urn:synthetic:ns{n}")
}

/// Shape of the generated schema set: `namespaces` target namespaces, each split over
/// `files` documents (file 0 includes the rest and imports every other namespace),
/// `types` complex types per namespace.
struct Shape {
    namespaces: usize,
    files: usize,
    types: usize,
}

/// Generates a bundle with the features that make real schemas expensive: namespaces split
/// across included files, cross-namespace references, recursive types, 5-level extension
/// chains, substitution groups whose members live in other namespaces, `xs:any`, attribute
/// groups, enumerations and documentation.
fn generate(shape: &Shape) -> SchemaBundle {
    let mut docs = Vec::new();
    let mut root = format!(r#"<xs:schema xmlns:xs="{XS}" targetNamespace="urn:synthetic:root">"#);
    for n in 0..shape.namespaces {
        let mut files: Vec<String> = (0..shape.files).map(|_| String::new()).collect();
        let main = &mut files[0];
        for f in 1..shape.files {
            let _ = write!(main, r#"<xs:include schemaLocation="ns{n}/part{f}.xsd"/>"#);
        }
        for m in (0..shape.namespaces).filter(|&m| m != n) {
            let _ = write!(
                main,
                r#"<xs:import namespace="{}" schemaLocation="ns{m}/main.xsd"/>"#,
                ns(m)
            );
        }
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
                   </xs:sequence><xs:attributeGroup ref="p{n}:AG"/>"#,
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
        let decls: String = (0..shape.namespaces)
            .map(|m| format!(r#" xmlns:p{m}="{}""#, ns(m)))
            .collect();
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

fn time<T>(label: &str, f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let v = f();
    let d = start.elapsed();
    println!("{label:<40} {:>8.2} ms", d.as_secs_f64() * 1000.0);
    (v, d)
}

#[test]
fn large_bundle_build_and_queries() {
    let shape = Shape {
        namespaces: 8,
        files: 5,
        types: 220,
    };
    let bundle = generate(&shape);
    let bytes: usize = bundle.docs.iter().map(|d| d.text.len()).sum();
    println!(
        "synthetic bundle: {} documents, {:.2} MB",
        bundle.docs.len(),
        bytes as f64 / 1e6
    );
    assert!(
        bytes >= 2_000_000,
        "generator should produce at least 2 MB, got {bytes}"
    );

    let release = !cfg!(debug_assertions);
    let (m, build) = time("build", || SchemaModel::build(&bundle));
    assert_eq!(m.warnings(), &[], "synthetic bundle builds cleanly");

    let n0 = ns(0);
    let q = |n: &str, l: &str| QName::new(n, l);
    // A deep path through recursion and cross-namespace references; T104 extends a
    // 4-level chain (T100..T104), so its content model is the longest kind.
    let mut path = vec![PathStep::new(q(&n0, "E104"))];
    let mut cur_type = 104;
    for _ in 0..6 {
        let next = (cur_type + 1) % shape.types;
        path.push(PathStep::new(q(&n0, &format!("next{cur_type}"))));
        cur_type = next;
    }
    let (children, t_children) = time("child_elements (deep path)", || m.child_elements(&path));
    // 5 levels of 9 fields + 2 choice branches, plus 20 substitution members per level
    // (deduplicated: they come from the same head).
    assert!(children.elements.len() > 40, "{}", children.elements.len());
    assert!(
        children
            .elements
            .iter()
            .any(|e| e.name == q(&ns(1), "Member3"))
    );

    // T110 has xs:any ##other: wildcard expansion to all globals of 7 other namespaces.
    let any_path = [PathStep::new(q(&n0, "E110"))];
    let (wild, t_wild) = time("child_elements (wildcard expansion)", || {
        m.child_elements(&any_path)
    });
    assert!(
        wild.elements.len() > 7 * shape.types,
        "{}",
        wild.elements.len()
    );

    let (attrs, t_attrs) = time("attributes", || m.attributes(&path));
    assert!(
        attrs
            .attributes
            .iter()
            .any(|a| a.name.local == "id" && a.required)
    );

    let code = [
        PathStep::new(q(&n0, "E104")),
        PathStep::new(q(&n0, "code104")),
    ];
    let (vals, t_vals) = time("text_values", || m.text_values(&code));
    assert_eq!(vals.len(), 12);

    let base = [PathStep::new(q(&n0, "E100"))];
    let (cands, t_xsi) = time("xsi_type_candidates", || m.xsi_type_candidates(&base));
    assert_eq!(cands.len(), 5, "T100 and its 4-level extension chain");

    let (info, t_info) = time("element_info", || m.element_info(&path));
    assert!(info.is_some());

    let (tpl, t_tpl) = time("template (E104, defaults)", || {
        m.template(&q(&n0, "E104"), &TemplateOptions::default())
    });
    let tpl = tpl.expect("template");
    println!(
        "template: {} bytes, truncated: {}",
        tpl.xml.len(),
        tpl.truncated
    );
    assert!(
        tpl.truncated,
        "recursive schema hits the depth limit or node budget"
    );

    if release {
        assert!(build < Duration::from_millis(500), "build took {build:?}");
        for (label, d) in [
            ("child_elements", t_children),
            ("wildcard", t_wild),
            ("attributes", t_attrs),
            ("text_values", t_vals),
            ("xsi_type_candidates", t_xsi),
            ("element_info", t_info),
        ] {
            assert!(d < Duration::from_millis(50), "{label} took {d:?}");
        }
        assert!(
            t_tpl < Duration::from_millis(200),
            "template took {t_tpl:?}"
        );
    }
}
