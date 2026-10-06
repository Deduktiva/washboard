//! Tests against `fixtures/customer` and small inline schemas.
//!
//! WP-WSDL builds real bundles; until it lands, bundles are assembled by hand here: the XSD
//! files as they are, and the `CustomerBinding.wsdl` inline schema extracted by hand with the
//! `wsdl:definitions` namespace declarations carried over and its `schemaLocation` rewritten
//! to the bundle URI, as PLAN §5 step 1 prescribes.

use std::path::PathBuf;

use crate::model::{QName, SchemaBundle, SchemaDoc, SchemaOrigin};
use crate::soap::{XSD_NS, XSI_NS};
use crate::xml;

use super::*;

const CUS: &str = "urn:example:customer";
const COM: &str = "urn:example:common";
const MSG: &str = "urn:example:customer:messages";
const AUDIT: &str = "urn:example:audit";

/// The inline schema of `fixtures/customer/CustomerBinding.wsdl`, extracted.
const CUSTOMER_INLINE: &str = r#"<xs:schema xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/"
                  xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
                  xmlns:soap12="http://schemas.xmlsoap.org/wsdl/soap12/"
                  xmlns:xs="http://www.w3.org/2001/XMLSchema"
                  xmlns:tns="urn:example:customer:service"
                  xmlns:msg="urn:example:customer:messages"
                  xmlns:cus="urn:example:customer" targetNamespace="urn:example:customer:messages" elementFormDefault="qualified">
      <xs:import namespace="urn:example:customer" schemaLocation="xsd/customer.xsd"/>

      <xs:element name="RequestContext">
        <xs:complexType>
          <xs:sequence>
            <xs:element name="correlationId" type="xs:string"/>
            <xs:element name="locale" type="xs:language" minOccurs="0"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>

      <xs:element name="GetCustomer">
        <xs:complexType>
          <xs:sequence>
            <xs:element name="customerId" type="cus:CustomerId"/>
            <xs:element name="includeOrders" type="xs:boolean" minOccurs="0"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>
      <xs:element name="GetCustomerResponse">
        <xs:complexType>
          <xs:sequence>
            <xs:element ref="cus:Customer"/>
            <xs:element ref="cus:Order" minOccurs="0" maxOccurs="unbounded"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>

      <xs:element name="CreateOrder">
        <xs:complexType>
          <xs:sequence>
            <xs:element ref="cus:Customer"/>
            <xs:element ref="cus:Order"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>
      <xs:element name="CreateOrderResponse">
        <xs:complexType>
          <xs:sequence>
            <xs:element name="orderId" type="xs:string"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>

      <xs:element name="CustomerFault">
        <xs:complexType>
          <xs:sequence>
            <xs:element name="code" type="xs:int"/>
            <xs:element name="text" type="xs:string"/>
          </xs:sequence>
        </xs:complexType>
      </xs:element>
    </xs:schema>"#;

const ROOT: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:washboard:root">
  <xs:import namespace="urn:example:customer:messages" schemaLocation="washboard:/inline/0.xsd"/>
</xs:schema>"#;

fn fixture(rel: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "..", "..", "fixtures", rel]
        .iter()
        .collect();
    let bytes = std::fs::read(&path).expect("fixture readable");
    xml::decode(&bytes).expect("fixture decodes").text
}

fn doc(uri: &str, target_ns: &str, origin: SchemaOrigin, text: String) -> SchemaDoc {
    SchemaDoc {
        uri: uri.into(),
        target_ns: target_ns.into(),
        origin,
        text,
    }
}

fn file(uri: &str, target_ns: &str) -> SchemaDoc {
    let origin = SchemaOrigin::File { path: uri.into() };
    doc(uri, target_ns, origin, fixture(&format!("customer/{uri}")))
}

fn customer_bundle() -> SchemaBundle {
    SchemaBundle {
        docs: vec![
            doc(
                "washboard:/root.xsd",
                "urn:washboard:root",
                SchemaOrigin::Generated,
                ROOT.into(),
            ),
            doc(
                "washboard:/inline/0.xsd",
                MSG,
                SchemaOrigin::InlineWsdl {
                    wsdl_path: "CustomerBinding.wsdl".into(),
                    index: 0,
                },
                CUSTOMER_INLINE.into(),
            ),
            file("xsd/customer.xsd", CUS),
            file("xsd/common/party.xsd", COM),
            file("xsd/common/party-ids.xsd", COM),
            file("xsd/ext/audit.xsd", AUDIT),
        ],
        root: "washboard:/root.xsd".into(),
    }
}

fn model() -> SchemaModel {
    let m = SchemaModel::build(&customer_bundle());
    assert_eq!(m.warnings(), &[], "fixture bundle builds cleanly");
    m
}

fn q(ns: &str, local: &str) -> QName {
    QName::new(ns, local)
}

fn path(steps: &[(&str, &str)]) -> Vec<PathStep> {
    steps
        .iter()
        .map(|(ns, l)| PathStep::new(q(ns, l)))
        .collect()
}

fn names(c: &ChildCompletions) -> Vec<String> {
    c.elements.iter().map(|e| e.name.to_string()).collect()
}

/// Small single-namespace bundle for targeted tests; `body` goes inside `xs:schema`.
fn tiny(body: &str) -> SchemaModel {
    let text = format!(
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t"
            targetNamespace="urn:t" elementFormDefault="qualified">{body}</xs:schema>"#
    );
    SchemaModel::build(&SchemaBundle {
        docs: vec![doc("t.xsd", "urn:t", SchemaOrigin::Generated, text)],
        root: "t.xsd".into(),
    })
}

fn well_formed(xml: &str) {
    roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("not well-formed ({e}):\n{xml}"));
}

// --- Required fixture cases (TASKS.md WP-SCHEMA acceptance) ---------------------------------

#[test]
fn party_offers_concrete_derived_types_for_xsi_type() {
    let m = model();
    let p = path(&[(MSG, "CreateOrder"), (CUS, "Customer"), (CUS, "party")]);
    let got: Vec<QName> = m
        .xsi_type_candidates(&p)
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(
        got,
        vec![q(COM, "Person"), q(COM, "Company"), q(COM, "PublicCompany")]
    );
    // The abstract declared type needs xsi:type, so it is offered as a required attribute.
    let attrs = m.attributes(&p);
    let xsi = attrs
        .attributes
        .iter()
        .find(|a| a.name == q(XSI_NS, "type"))
        .expect("xsi:type offered");
    assert!(xsi.required);
    assert_eq!(xsi.source, SuggestionSource::Xsi);
}

#[test]
fn contact_method_position_offers_substitution_members() {
    let m = model();
    let c = m.child_elements(&path(&[(MSG, "CreateOrder"), (CUS, "Customer")]));
    assert_eq!(
        names(&c),
        vec![
            q(CUS, "id").to_string(),
            q(CUS, "party").to_string(),
            q(COM, "Email").to_string(),
            q(COM, "Phone").to_string(),
            q(CUS, "extensions").to_string(),
        ]
    );
    let email = &c.elements[2];
    assert_eq!(
        email.source,
        SuggestionSource::Substitution {
            head: q(COM, "ContactMethod")
        }
    );
    assert_eq!(email.max_occurs, MaxOccurs::Unbounded);
    let party = &c.elements[1];
    assert!(party.type_is_abstract && party.has_derived_types);
    assert_eq!(party.type_name, Some(q(COM, "Party")));
    assert_eq!(
        (party.min_occurs, party.max_occurs),
        (1, MaxOccurs::Bounded(1))
    );
}

#[test]
fn extensions_offer_audit_info_via_wildcard() {
    let m = model();
    let p = path(&[(MSG, "CreateOrder"), (CUS, "Customer"), (CUS, "extensions")]);
    let c = m.child_elements(&p);
    let audit = c
        .elements
        .iter()
        .find(|e| e.name == q(AUDIT, "AuditInfo"))
        .expect("audit:AuditInfo offered");
    assert_eq!(audit.source, SuggestionSource::Wildcard);
    // ##other excludes the customer namespace itself and no-namespace elements.
    assert!(
        c.elements
            .iter()
            .all(|e| e.name.ns != CUS && !e.name.ns.is_empty())
    );
    assert_eq!(c.wildcards.len(), 1);
    assert_eq!(c.wildcards[0].process_contents, ProcessContents::Lax);
    assert_eq!(
        c.wildcards[0].namespaces,
        NamespaceConstraint::Not(CUS.into())
    );
}

// --- Path resolution -------------------------------------------------------------------------

#[test]
fn xsi_type_switches_the_content_model() {
    let m = model();
    let mut p = path(&[(MSG, "CreateOrder"), (CUS, "Customer"), (CUS, "party")]);
    // Without xsi:type: the abstract type's own content.
    assert_eq!(
        names(&m.child_elements(&p)),
        vec![
            q(COM, "displayName").to_string(),
            q(COM, "country").to_string()
        ]
    );
    p[2].xsi_type = Some(q(COM, "PublicCompany"));
    let got = names(&m.child_elements(&p));
    let want: Vec<String> = ["displayName", "country", "vatId", "isin"]
        .iter()
        .map(|l| q(COM, l).to_string())
        .collect();
    assert_eq!(got, want);

    let info = m.element_info(&p).expect("resolves");
    assert_eq!(info.declared_type, Some(q(COM, "Party")));
    assert_eq!(info.actual_type, Some(q(COM, "PublicCompany")));
    assert!(!info.type_is_abstract);
}

#[test]
fn path_through_substitution_member_and_wildcard() {
    let m = model();
    let email = path(&[(MSG, "CreateOrder"), (CUS, "Customer"), (COM, "Email")]);
    assert_eq!(
        names(&m.child_elements(&email)),
        vec![q(COM, "address").to_string()]
    );
    let attrs = m.attributes(&email);
    let preferred = &attrs.attributes[0];
    assert_eq!(preferred.name, q("", "preferred"));
    assert_eq!(preferred.default.as_deref(), Some("false"));
    assert!(!preferred.required);
    let vals: Vec<String> = m
        .attribute_values(&email, &q("", "preferred"))
        .into_iter()
        .map(|v| v.value)
        .collect();
    assert_eq!(vals, ["true", "false"]);

    let audit = path(&[
        (MSG, "CreateOrder"),
        (CUS, "Customer"),
        (CUS, "extensions"),
        (AUDIT, "AuditInfo"),
    ]);
    assert_eq!(
        names(&m.child_elements(&audit)),
        vec![q(AUDIT, "user").to_string(), q(AUDIT, "at").to_string()]
    );
    // Unknown element under the lax wildcard: resolves, but there is nothing to suggest.
    let unknown = path(&[
        (MSG, "CreateOrder"),
        (CUS, "Customer"),
        (CUS, "extensions"),
        ("urn:example:unknown", "Whatever"),
    ]);
    assert!(m.element_info(&unknown).is_some());
    assert!(m.child_elements(&unknown).elements.is_empty());
    // Not allowed at all.
    assert!(
        m.element_info(&path(&[(MSG, "CreateOrder"), (CUS, "nope")]))
            .is_none()
    );
    assert!(
        m.element_info(&path(&[(CUS, "id")])).is_none(),
        "local element is not a root"
    );
}

#[test]
fn enumerations_attributes_and_hover() {
    let m = model();
    let status = path(&[(MSG, "CreateOrder"), (CUS, "Order"), (CUS, "status")]);
    let vals: Vec<String> = m
        .text_values(&status)
        .into_iter()
        .map(|v| v.value)
        .collect();
    assert_eq!(vals, ["NEW", "SHIPPED", "CANCELLED"]);
    let info = m.element_info(&status).expect("resolves");
    assert_eq!(info.declared_type, Some(q(CUS, "OrderStatus")));
    assert_eq!(
        (info.min_occurs, info.max_occurs),
        (0, MaxOccurs::Bounded(1))
    );

    let line = path(&[(MSG, "CreateOrder"), (CUS, "Order"), (CUS, "line")]);
    let attrs = m.attributes(&line);
    let got: Vec<(String, bool)> = attrs
        .attributes
        .iter()
        .map(|a| (a.name.local.clone(), a.required))
        .collect();
    assert_eq!(got, [("sku".to_string(), true), ("qty".to_string(), true)]);
    assert_eq!(
        attrs.attributes[1].type_name,
        Some(q(XSD_NS, "positiveInteger"))
    );

    let include = path(&[(MSG, "GetCustomer"), (MSG, "includeOrders")]);
    let vals: Vec<String> = m
        .text_values(&include)
        .into_iter()
        .map(|v| v.value)
        .collect();
    assert_eq!(vals, ["true", "false"]);

    let ti = m.type_info(&q(COM, "VatId")).expect("type");
    assert_eq!(ti.base, Some(q(XSD_NS, "string")));
    assert_eq!(ti.patterns, ["[A-Z]{2}[A-Z0-9]{2,12}"]);
    let ti = m.type_info(&q(COM, "PublicCompany")).expect("type");
    assert_eq!(ti.base, Some(q(COM, "Company")));
    assert_eq!(ti.derivation, Some(Derivation::Extension));
}

#[test]
fn global_elements_exclude_abstract_heads() {
    let m = model();
    let g = m.global_elements();
    assert!(g.contains(&q(MSG, "GetCustomer")));
    assert!(g.contains(&q(COM, "Email")));
    assert!(!g.contains(&q(COM, "ContactMethod")));
}

// --- Templates -------------------------------------------------------------------------------

#[test]
fn template_get_customer() {
    let m = model();
    let t = m
        .template(&q(MSG, "GetCustomer"), &TemplateOptions::default())
        .expect("template");
    println!("{}", t.xml);
    assert_eq!(
        t.xml,
        r#"<msg:GetCustomer xmlns:msg="urn:example:customer:messages">
  <msg:customerId>1</msg:customerId>
  <!-- optional -->
  <msg:includeOrders>true</msg:includeOrders>
</msg:GetCustomer>"#
    );
    assert_eq!(t.namespaces, [("msg".to_string(), MSG.to_string())]);
    assert!(!t.truncated);
    well_formed(&t.xml);
}

#[test]
fn template_create_order() {
    let m = model();
    let t = m
        .template(&q(MSG, "CreateOrder"), &TemplateOptions::default())
        .expect("template");
    println!("{}", t.xml);
    well_formed(&t.xml);
    let expected = r#"<msg:CreateOrder xmlns:msg="urn:example:customer:messages" xmlns:cus="urn:example:customer" xmlns:com="urn:example:common" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:audit="urn:example:audit">
  <cus:Customer>
    <cus:id>1</cus:id>
    <!-- xsi:type alternatives: com:Company, com:PublicCompany -->
    <cus:party xsi:type="com:Person">
      <com:displayName>?</com:displayName>
      <com:country>?</com:country>
      <com:firstName>?</com:firstName>
      <com:lastName>?</com:lastName>
      <!-- optional -->
      <com:birthDate>2026-01-01</com:birthDate>
    </cus:party>
    <!-- optional; substitutes abstract com:ContactMethod; alternatives: com:Phone -->
    <com:Email>
      <com:address>?</com:address>
    </com:Email>
    <!-- optional -->
    <cus:extensions>
      <!-- any element from: any namespace except urn:example:customer (lax); known: audit:AuditInfo, com:Email, com:Phone, msg:RequestContext, msg:GetCustomer, … -->
    </cus:extensions>
  </cus:Customer>
  <cus:Order>
    <cus:line sku="?" qty="1"/>
    <!-- optional -->
    <cus:status>NEW</cus:status>
  </cus:Order>
</msg:CreateOrder>"#;
    assert_eq!(t.xml, expected);
}

#[test]
fn template_options_and_errors() {
    let m = model();
    let opts = TemplateOptions {
        prefixes: vec![("m".into(), MSG.into())],
        declare_namespaces: false,
        indent: "\t".into(),
        ..TemplateOptions::default()
    };
    let t = m.template(&q(MSG, "GetCustomer"), &opts).expect("template");
    assert!(
        t.xml.starts_with("<m:GetCustomer>\n\t<m:customerId>"),
        "{}",
        t.xml
    );
    assert_eq!(t.namespaces, [("m".to_string(), MSG.to_string())]);
    assert_eq!(
        m.template(&q(MSG, "Nope"), &opts),
        Err(SchemaError::UnknownElement(q(MSG, "Nope")))
    );
    // An abstract root is replaced by its first concrete member.
    let t = m
        .template(&q(COM, "ContactMethod"), &TemplateOptions::default())
        .expect("template");
    assert!(
        t.xml.starts_with(
            "<!-- substitutes abstract com:ContactMethod; alternatives: com:Phone -->\n\
             <com:Email xmlns:com=\"urn:example:common\">"
        ),
        "{}",
        t.xml
    );
}

#[test]
fn template_depth_limit_and_node_budget() {
    let m = tiny(
        r#"<xs:element name="node" type="t:Node"/>
           <xs:complexType name="Node"><xs:sequence>
             <xs:element name="value" type="xs:int"/>
             <xs:element name="child" type="t:Node" minOccurs="0"/>
           </xs:sequence></xs:complexType>"#,
    );
    let t = m
        .template(&q("urn:t", "node"), &TemplateOptions::default())
        .expect("template");
    well_formed(&t.xml);
    assert!(t.truncated);
    assert_eq!(t.xml.matches("<t:child>").count(), 6, "{}", t.xml);
    assert!(
        t.xml.contains("<!-- … truncated: type t:Node -->"),
        "{}",
        t.xml
    );

    let small = TemplateOptions {
        max_nodes: 3,
        ..TemplateOptions::default()
    };
    let t = m.template(&q("urn:t", "node"), &small).expect("template");
    well_formed(&t.xml);
    assert!(t.truncated);
    assert_eq!(
        t.xml
            .matches("truncated: template limit of 3 elements")
            .count(),
        1,
        "{}",
        t.xml
    );
}

#[test]
fn template_choice_repetition_and_simple_content() {
    let m = tiny(
        r#"<xs:element name="r"><xs:complexType><xs:sequence>
             <xs:choice>
               <xs:element name="a" type="xs:string"/>
               <xs:sequence><xs:element name="b" type="xs:string"/><xs:element name="c" type="xs:string"/></xs:sequence>
               <xs:any namespace='##local' processContents="skip"/>
             </xs:choice>
             <xs:element name="two" type="xs:decimal" minOccurs="2" maxOccurs="5"/>
             <xs:element name="amount">
               <xs:complexType><xs:simpleContent><xs:extension base="xs:decimal">
                 <xs:attribute name="currency" use="required" fixed="EUR"/>
               </xs:extension></xs:simpleContent></xs:complexType>
             </xs:element>
             <xs:element name="fixed" type="xs:string" fixed="X &amp; Y"/>
           </xs:sequence></xs:complexType></xs:element>"#,
    );
    let t = m
        .template(&q("urn:t", "r"), &TemplateOptions::default())
        .expect("template");
    println!("{}", t.xml);
    well_formed(&t.xml);
    let expected = r#"<t:r xmlns:t="urn:t">
  <!-- choice; alternatives: (t:b, t:c), any element -->
  <t:a>?</t:a>
  <t:two>0</t:two>
  <t:two>0</t:two>
  <t:amount currency="EUR">0</t:amount>
  <t:fixed>X &amp; Y</t:fixed>
</t:r>"#;
    assert_eq!(t.xml, expected);
}

// --- XSD features ----------------------------------------------------------------------------

#[test]
fn chameleon_include_takes_the_including_namespace() {
    let cham = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" elementFormDefault="qualified">
        <xs:complexType name="Money"><xs:sequence>
          <xs:element name="amount" type="xs:decimal"/>
          <xs:element name="unit" type="Unit"/>
        </xs:sequence></xs:complexType>
        <xs:simpleType name="Unit"><xs:restriction base="xs:string">
          <xs:enumeration value="EUR"/><xs:enumeration value="USD"/>
        </xs:restriction></xs:simpleType>
      </xs:schema>"#;
    let mk = |ns: &str| {
        format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:p="{ns}"
                targetNamespace="{ns}" elementFormDefault="qualified">
              <xs:include schemaLocation="common/money.xsd"/>
              <xs:element name="price" type="p:Money"/>
            </xs:schema>"#
        )
    };
    let bundle = SchemaBundle {
        docs: vec![
            doc("a.xsd", "urn:a", SchemaOrigin::Generated, mk("urn:a")),
            doc("b.xsd", "urn:b", SchemaOrigin::Generated, mk("urn:b")),
            doc("common/money.xsd", "", SchemaOrigin::Generated, cham.into()),
        ],
        root: String::new(),
    };
    let m = SchemaModel::build(&bundle);
    assert_eq!(m.warnings(), &[]);
    for ns in ["urn:a", "urn:b"] {
        let p = path(&[(ns, "price")]);
        assert_eq!(
            names(&m.child_elements(&p)),
            vec![q(ns, "amount").to_string(), q(ns, "unit").to_string()]
        );
        let unit = path(&[(ns, "price"), (ns, "unit")]);
        let vals: Vec<String> = m.text_values(&unit).into_iter().map(|v| v.value).collect();
        assert_eq!(vals, ["EUR", "USD"]);
    }
    assert!(
        m.type_info(&q("", "Money")).is_none(),
        "chameleon not also no-namespace"
    );
}

#[test]
fn groups_attribute_groups_restriction_and_block() {
    let m = tiny(
        r#"<xs:group name="g"><xs:sequence>
             <xs:element name="x" type="xs:string"/>
             <xs:element name="y" type="xs:string" minOccurs="0"/>
           </xs:sequence></xs:group>
           <xs:attributeGroup name="ag">
             <xs:attribute name="id" type="xs:ID" use="required"/>
             <xs:attribute name="lang" type="t:Lang"/>
             <xs:anyAttribute namespace='##other' processContents="lax"/>
           </xs:attributeGroup>
           <xs:simpleType name="Lang"><xs:union memberTypes="t:A t:B"/></xs:simpleType>
           <xs:simpleType name="A"><xs:restriction base="xs:string"><xs:enumeration value="de"/></xs:restriction></xs:simpleType>
           <xs:simpleType name="B"><xs:restriction base="xs:string"><xs:enumeration value="en"/></xs:restriction></xs:simpleType>
           <xs:complexType name="Base"><xs:sequence><xs:group ref="t:g"/></xs:sequence>
             <xs:attributeGroup ref="t:ag"/></xs:complexType>
           <xs:complexType name="Narrow"><xs:complexContent><xs:restriction base="t:Base">
             <xs:sequence><xs:element name="x" type="xs:string"/></xs:sequence>
             <xs:attribute name="lang" use="prohibited"/>
           </xs:restriction></xs:complexContent></xs:complexType>
           <xs:complexType name="Wide"><xs:complexContent><xs:extension base="t:Base">
             <xs:choice maxOccurs="unbounded"><xs:element name="p" type="xs:int"/><xs:element name="q" type="xs:int"/></xs:choice>
           </xs:extension></xs:complexContent></xs:complexType>
           <xs:element name="open" type="t:Base"/>
           <xs:element name="closed" type="t:Base" block="extension"/>"#,
    );
    assert_eq!(m.warnings(), &[]);
    let open = path(&[("urn:t", "open")]);
    let c = m.child_elements(&open);
    assert_eq!(
        names(&c),
        vec![q("urn:t", "x").to_string(), q("urn:t", "y").to_string()]
    );
    assert_eq!(c.elements[1].min_occurs, 0);
    let a = m.attributes(&open);
    let got: Vec<&str> = a.attributes.iter().map(|a| a.name.local.as_str()).collect();
    assert_eq!(got, ["id", "lang", "type"]);
    assert_eq!(
        a.wildcard.as_ref().map(|w| w.process_contents),
        Some(ProcessContents::Lax)
    );
    let vals: Vec<String> = m
        .attribute_values(&open, &q("", "lang"))
        .into_iter()
        .map(|v| v.value)
        .collect();
    assert_eq!(vals, ["de", "en"]);

    let cands = |p: &[PathStep]| -> Vec<String> {
        m.xsi_type_candidates(p)
            .into_iter()
            .map(|t| t.name.local)
            .collect()
    };
    assert_eq!(cands(&open), ["Base", "Narrow", "Wide"]);
    assert_eq!(cands(&path(&[("urn:t", "closed")])), ["Base", "Narrow"]);

    let narrow = vec![PathStep::new(q("urn:t", "open")).with_xsi_type(q("urn:t", "Narrow"))];
    assert_eq!(
        names(&m.child_elements(&narrow)),
        vec![q("urn:t", "x").to_string()]
    );
    let got: Vec<String> = m
        .attributes(&narrow)
        .attributes
        .into_iter()
        .map(|a| a.name.local)
        .collect();
    assert_eq!(got, ["id", "type"]);

    let wide = vec![PathStep::new(q("urn:t", "open")).with_xsi_type(q("urn:t", "Wide"))];
    let c = m.child_elements(&wide);
    assert_eq!(names(&c).len(), 4);
    assert_eq!(
        (c.elements[2].min_occurs, c.elements[2].max_occurs),
        (0, MaxOccurs::Unbounded)
    );
}

#[test]
fn substitution_groups_are_transitive_and_honour_block() {
    let m = tiny(
        r#"<xs:complexType name="T"/>
           <xs:complexType name="U"><xs:complexContent><xs:extension base="t:T"/></xs:complexContent></xs:complexType>
           <xs:element name="head" type="t:T" abstract="true"/>
           <xs:element name="mid" substitutionGroup="t:head" abstract="true"/>
           <xs:element name="leaf" substitutionGroup="t:mid"/>
           <xs:element name="ext" type="t:U" substitutionGroup="t:head"/>
           <xs:element name="strict" type="t:T" block="extension"/>
           <xs:element name="s2" type="t:U" substitutionGroup="t:strict"/>
           <xs:element name="s3" type="t:T" substitutionGroup="t:strict"/>
           <xs:element name="r"><xs:complexType><xs:sequence>
             <xs:element ref="t:head"/><xs:element ref="t:strict"/>
           </xs:sequence></xs:complexType></xs:element>"#,
    );
    assert_eq!(m.warnings(), &[]);
    let got: Vec<String> = m
        .child_elements(&path(&[("urn:t", "r")]))
        .elements
        .into_iter()
        .map(|e| e.name.local)
        .collect();
    assert_eq!(got, ["leaf", "ext", "strict", "s3"]);
    // Resolving through a transitive member works, and it inherits the head's type.
    let leaf = path(&[("urn:t", "r"), ("urn:t", "leaf")]);
    assert_eq!(
        m.element_info(&leaf).and_then(|i| i.declared_type),
        Some(q("urn:t", "T"))
    );
}

#[test]
fn redefine_replaces_type_and_keeps_original_as_base() {
    let orig = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
        <xs:complexType name="T"><xs:sequence><xs:element name="a" type="xs:string"/></xs:sequence></xs:complexType>
      </xs:schema>"#;
    let redef = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
        <xs:redefine schemaLocation="orig.xsd">
          <xs:complexType name="T"><xs:complexContent><xs:extension base="t:T">
            <xs:sequence><xs:element name="b" type="xs:string"/></xs:sequence>
          </xs:extension></xs:complexContent></xs:complexType>
        </xs:redefine>
        <xs:element name="e" type="t:T"/>
      </xs:schema>"#;
    let m = SchemaModel::build(&SchemaBundle {
        docs: vec![
            doc("redef.xsd", "urn:t", SchemaOrigin::Generated, redef.into()),
            doc("orig.xsd", "urn:t", SchemaOrigin::Generated, orig.into()),
        ],
        root: String::new(),
    });
    assert_eq!(m.warnings(), &[]);
    assert_eq!(
        names(&m.child_elements(&path(&[("urn:t", "e")]))),
        vec![q("urn:t", "a").to_string(), q("urn:t", "b").to_string()]
    );
}

#[test]
fn rpc_wrapper_including_real_schema_of_same_namespace() {
    // Shape of the WP-WSDL output for fixtures/legacy-rpc (PLAN §5.4): unqualified parts.
    let real = r#"<xs:schema xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:tns="urn:example:legacy" targetNamespace="urn:example:legacy">
      <xs:complexType name="Address"><xs:sequence>
        <xs:element name="street" type="xs:string"/><xs:element name="city" type="xs:string"/>
        <xs:element name="zip" type="xs:string" minOccurs="0"/>
      </xs:sequence></xs:complexType></xs:schema>"#;
    let rpc = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t0="urn:example:legacy" targetNamespace="urn:example:legacy">
      <xs:include schemaLocation="washboard:/inline/0.xsd"/>
      <xs:element name="Lookup"><xs:complexType><xs:sequence>
        <xs:element name="customerNo" type="xs:string"/><xs:element name="asOf" type="xs:date"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="LookupResponse"><xs:complexType><xs:sequence>
        <xs:element name="result" type="t0:Address"/>
      </xs:sequence></xs:complexType></xs:element></xs:schema>"#;
    let m = SchemaModel::build(&SchemaBundle {
        docs: vec![
            doc(
                "washboard:/inline/0.xsd",
                "urn:example:legacy",
                SchemaOrigin::Generated,
                real.into(),
            ),
            doc(
                "washboard:/rpc/0.xsd",
                "urn:example:legacy",
                SchemaOrigin::Generated,
                rpc.into(),
            ),
        ],
        root: String::new(),
    });
    assert_eq!(m.warnings(), &[]);
    let t = m
        .template(
            &q("urn:example:legacy", "Lookup"),
            &TemplateOptions::default(),
        )
        .expect("template");
    assert_eq!(
        t.xml,
        "<tns:Lookup xmlns:tns=\"urn:example:legacy\">\n  <customerNo>?</customerNo>\n  \
         <asOf>2026-01-01</asOf>\n</tns:Lookup>"
    );
    let r = path(&[("urn:example:legacy", "LookupResponse"), ("", "result")]);
    assert_eq!(names(&m.child_elements(&r)), vec!["street", "city", "zip"]);
}

// --- Robustness ------------------------------------------------------------------------------

#[test]
fn malformed_and_cyclic_schemas_do_not_panic() {
    let m = tiny(
        r#"<xs:group name="g1"><xs:sequence><xs:group ref="t:g2"/></xs:sequence></xs:group>
           <xs:group name="g2"><xs:sequence><xs:group ref="t:g1"/></xs:sequence></xs:group>
           <xs:complexType name="A"><xs:complexContent><xs:extension base="t:B"/></xs:complexContent></xs:complexType>
           <xs:complexType name="B"><xs:complexContent><xs:extension base="t:A">
             <xs:sequence><xs:group ref="t:g1"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
           <xs:attributeGroup name="ag"><xs:attributeGroup ref="t:ag"/></xs:attributeGroup>
           <xs:complexType name="C"><xs:attributeGroup ref="t:ag"/></xs:complexType>
           <xs:element name="a" type="t:A" substitutionGroup="t:b"/>
           <xs:element name="b" substitutionGroup="t:a"/>
           <xs:element name="c" type="t:C"/>
           <xs:element name="d" type="t:Missing" minOccurs="x"/>
           <xs:element name="e" type="nope:X"/>
           <xs:element/>
           <xs:simpleType name="L"><xs:list itemType="t:L"/></xs:simpleType>
           <xs:element name="l" type="t:L"/>"#,
    );
    assert!(!m.warnings().is_empty());
    for root in ["a", "b", "c", "d", "e", "l"] {
        let p = path(&[("urn:t", root)]);
        let _ = m.child_elements(&p);
        let _ = m.attributes(&p);
        let _ = m.text_values(&p);
        let _ = m.xsi_type_candidates(&p);
        let _ = m.element_info(&p);
        let t = m
            .template(&q("urn:t", root), &TemplateOptions::default())
            .expect("template");
        well_formed(&t.xml);
    }

    let broken = SchemaModel::build(&SchemaBundle {
        docs: vec![
            doc("x.xsd", "", SchemaOrigin::Generated, "<xs:schema".into()),
            doc("y.xsd", "", SchemaOrigin::Generated, "<notaschema/>".into()),
            doc(
                "z.xsd",
                "urn:z",
                SchemaOrigin::Generated,
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:z">
                     <xs:include schemaLocation="missing.xsd"/>
                     <xs:import namespace="urn:elsewhere" schemaLocation="http://example.invalid/x.xsd"/>
                   </xs:schema>"#
                    .into(),
            ),
        ],
        root: String::new(),
    });
    let msgs: Vec<&str> = broken
        .warnings()
        .iter()
        .map(|w| w.message.as_str())
        .collect();
    assert_eq!(msgs.len(), 4, "{msgs:?}");
    assert!(msgs[0].starts_with("not well-formed"), "{msgs:?}");
    assert_eq!(broken.warnings()[0].uri, "x.xsd");
    assert!(broken.global_elements().is_empty());
}
