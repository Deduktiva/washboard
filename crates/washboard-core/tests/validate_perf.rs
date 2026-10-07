//! libxml2 timings on the synthetic ~2 MB schema set (PLAN §5.1): schema compile and
//! per-request validation through the whole pipeline (`validate_request`).
//!
//! These numbers decide whether live validation is on (target < 100 ms per request) and
//! whether open-to-ready fits in 2 s. Timings are always printed
//! (`cargo test --release --test validate_perf -- --nocapture`) and asserted only in release
//! builds, since debug builds of this crate are unoptimized.

use std::fmt::Write as _;
use std::time::Duration;

use washboard_core::model::SchemaOrigin;
use washboard_core::validate::{self, RequestSchema};
use washboard_core::wsdl::{self, SourceFile, Sources};

mod common;
use common::{LARGE as SHAPE, check_size, generate, ns, prefix_decls, print_time, timed};

/// Body element of the one operation; T104 extends a 4-level chain, so its content model is
/// the longest kind.
const BODY_TYPE: usize = 104;
const RUNS: usize = 5;

/// A document/literal WSDL over the generated schema files, with one operation taking
/// `p0:E104`. The generated root document is dropped: the WSDL's inline schema imports every
/// namespace instead.
fn sources() -> Sources {
    let bundle = generate(&SHAPE);
    let files = bundle
        .docs
        .into_iter()
        .filter(|d| d.origin != SchemaOrigin::Generated)
        .map(|d| SourceFile::new(d.uri, d.text));
    let decls = prefix_decls(&SHAPE);
    let imports: String = (0..SHAPE.namespaces)
        .map(|m| {
            format!(
                r#"<xs:import namespace="{}" schemaLocation="ns{m}/main.xsd"/>"#,
                ns(m)
            )
        })
        .collect();
    let wsdl = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<wsdl:definitions xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/"
    xmlns:soap="http://schemas.xmlsoap.org/wsdl/soap/"
    xmlns:xs="http://www.w3.org/2001/XMLSchema"
    xmlns:tns="urn:synthetic:svc"{decls}
    targetNamespace="urn:synthetic:svc">
  <wsdl:types>
    <xs:schema targetNamespace="urn:synthetic:svc:types">{imports}</xs:schema>
  </wsdl:types>
  <wsdl:message name="In"><wsdl:part name="parameters" element="p0:E{BODY_TYPE}"/></wsdl:message>
  <wsdl:message name="Out"><wsdl:part name="parameters" element="p0:E0"/></wsdl:message>
  <wsdl:portType name="PT">
    <wsdl:operation name="Op"><wsdl:input message="tns:In"/><wsdl:output message="tns:Out"/>
    </wsdl:operation>
  </wsdl:portType>
  <wsdl:binding name="B" type="tns:PT">
    <soap:binding style="document" transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="Op">
      <soap:operation soapAction="urn:synthetic:Op"/>
      <wsdl:input><soap:body use="literal"/></wsdl:input>
      <wsdl:output><soap:body use="literal"/></wsdl:output>
    </wsdl:operation>
  </wsdl:binding>
  <wsdl:service name="S">
    <wsdl:port name="P" binding="tns:B"><soap:address location="http://localhost/"/></wsdl:port>
  </wsdl:service>
</wsdl:definitions>
"#
    );
    Sources::new(SourceFile::new("Synthetic.wsdl", wsdl), files)
}

/// Writes instances of the generated types. `depth` bounds the `next`/`cross` recursion;
/// `members` is the number of substitution group members per `Head` position, the knob for
/// request size. With `invalid`, every member's `xs:int` field holds a non-number.
struct Instance {
    members: usize,
    invalid: bool,
    ids: usize,
    out: String,
}

impl Instance {
    /// An element named `p{elem_ns}:{name}` whose type is `p{type_ns}:T{t}`.
    fn element(&mut self, elem_ns: usize, name: &str, type_ns: usize, t: usize, depth: usize) {
        self.ids += 1;
        let _ = write!(self.out, r#"<p{elem_ns}:{name} id="i{}">"#, self.ids);
        // Fields of the whole extension chain, base type first.
        let base = t - t % 5;
        for k in base..=t {
            let n = type_ns;
            if depth > 0 && k == t {
                let next = (k + 1) % SHAPE.types;
                self.element(n, &format!("next{k}"), n, next, depth - 1);
            }
            if depth > 0 && k == base {
                let other = (n + 1 + k % (SHAPE.namespaces - 1)) % SHAPE.namespaces;
                self.element(n, &format!("cross{k}"), other, k, depth - 1);
            }
            let _ = write!(
                self.out,
                "<p{n}:name{k}>name {k}</p{n}:name{k}><p{n}:code{k}>VALUE_{e}_{v}</p{n}:code{k}>\
                 <p{n}:count{k}>{k}</p{n}:count{k}><p{n}:date{k}>2026-10-07</p{n}:date{k}>",
                e = k % 10,
                v = k % 12
            );
            if k == t {
                // Members of this namespace's Head live in the next namespace.
                let m = (n + 1) % SHAPE.namespaces;
                for j in 0..self.members {
                    let j = j % 20;
                    let extra = if self.invalid {
                        "x".to_owned()
                    } else {
                        j.to_string()
                    };
                    let _ = write!(
                        self.out,
                        "\n<p{m}:Member{j}><p{n}:common>member {j}</p{n}:common>\
                         <p{m}:extra{j}>{extra}</p{m}:extra{j}></p{m}:Member{j}>"
                    );
                }
            }
            let _ = writeln!(self.out, "<p{n}:a{k}>choice</p{n}:a{k}>");
        }
        let _ = writeln!(self.out, "</p{elem_ns}:{name}>");
    }

    fn request(depth: usize, members: usize, invalid: bool) -> String {
        let decls = prefix_decls(&SHAPE);
        let mut inst = Instance {
            members,
            invalid,
            ids: 0,
            out: format!(
                r#"<soapenv:Envelope xmlns:soapenv="http://schemas.xmlsoap.org/soap/envelope/"{decls}>
<soapenv:Body>
"#
            ),
        };
        inst.element(0, &format!("E{BODY_TYPE}"), 0, BODY_TYPE, depth);
        inst.out.push_str("</soapenv:Body>\n</soapenv:Envelope>\n");
        inst.out
    }
}

#[test]
fn compile_and_validate_requests() {
    let release = !cfg!(debug_assertions);
    let loaded = wsdl::load(&sources());
    assert!(
        !loaded.check.has_errors(),
        "synthetic WSDL loads: {:#?}",
        loaded.check.diagnostics
    );
    check_size(&loaded.bundle);

    // Compile: once per project open, on a background thread. A fresh compile each run, as on
    // every open; the schema of the last run is kept.
    let (schema, compile) = timed(RUNS, || RequestSchema::compile(&loaded.bundle));
    let schema = schema.unwrap_or_else(|d| panic!("bundle compiles: {d:#?}"));
    print_time("compile (RequestSchema)", compile, "");

    let cases = [
        // (label, depth, members, invalid)
        ("small", 0, 0, false),
        ("medium", 2, 20, false),
        ("large", 3, 60, false),
        ("very large", 3, 200, false),
        ("huge", 4, 600, false),
        ("large, error in every member", 3, 60, true),
    ];
    let mut timings = Vec::new();
    for (label, depth, members, invalid) in cases {
        let text = Instance::request(depth, members, invalid);
        let (v, t) = timed(RUNS, || {
            validate::validate_request(&loaded, &schema, &text, None)
        });
        let errors = v.diagnostics.iter().filter(|d| d.is_error()).count();
        print_time(
            &format!("validate_request ({label})"),
            t,
            &format!(
                "({:.0} KB, {} lines, {errors} errors)",
                text.len() as f64 / 1e3,
                text.lines().count()
            ),
        );
        assert_eq!(
            v.operation().map(|o| o.operation.as_str()),
            Some("Op"),
            "{label}: dispatches"
        );
        if invalid {
            assert!(errors > 0, "{label}: has errors");
        } else {
            assert_eq!(errors, 0, "{label}: valid, got {:#?}", v.diagnostics);
        }
        timings.push((label, text.len(), t));
    }

    if release {
        assert!(compile < Duration::from_secs(2), "compile took {compile:?}");
        // Live validation's budget, for requests up to the size the editor is built for
        // (PLAN §2: typing stays responsive in a 1 MB file).
        for (label, len, t) in timings {
            if len <= 1_000_000 {
                assert!(
                    t < Duration::from_millis(100),
                    "{label} ({len} bytes) took {t:?}"
                );
            }
        }
    }
}
