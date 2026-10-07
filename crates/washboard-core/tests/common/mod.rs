//! Synthetic schema sets shared by the performance tests (PLAN §5.1 "Fixture corpus").

use std::fmt::Write as _;

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

/// Generates a bundle with the features that make real schemas expensive: namespaces split
/// across included files, cross-namespace references, recursive types, 5-level extension
/// chains, substitution groups whose members live in other namespaces, `xs:any`, attribute
/// groups, enumerations and documentation.
pub fn generate(shape: &Shape) -> SchemaBundle {
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
