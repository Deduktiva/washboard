//! Performance of the Rust schema model on a synthetic ~2 MB bundle (PLAN §5.1).
//!
//! Targets: build < 500 ms, completion queries < 50 ms. Timings are always printed
//! (`cargo test --release --test schema_perf -- --nocapture`) and asserted only in release
//! builds, since debug builds of this crate are unoptimized.

use std::time::{Duration, Instant};

use washboard_core::model::QName;
use washboard_core::schema::{PathStep, SchemaModel, TemplateOptions};

mod common;
use common::{Shape, generate, ns};

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
