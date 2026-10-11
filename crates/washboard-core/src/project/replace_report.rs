//! What replacing a project's WSDL changed (PLAN §4 "Replace WSDL"): operations added and
//! removed, and the requests that no longer validate. Requests are never rewritten; the report
//! only names them.
//!
//! One computation for the app and the CLI. The app swaps the files on the main thread and
//! builds the report on a worker once the new schema has compiled, so the inputs are plain
//! values: the old operations, the new [`Wsdl`] and its schema, and the request texts read
//! before the swap.

use crate::model::{OperationRef, RequestId};
use crate::validate::{RequestSchema, validate_request};
use crate::wsdl::Wsdl;
use crate::xml::check_well_formed;

use super::{Project, ProjectError};

/// The report shown after Replace WSDL.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplaceReport {
    /// Supported operations of the new WSDL that the old one lacked, in the new one's order.
    pub added: Vec<OperationRef>,
    /// Supported operations of the old WSDL that the new one lacks, in the old one's order.
    pub removed: Vec<OperationRef>,
    /// Requests that fail validation against the new WSDL, in the order they were given.
    pub invalid: Vec<RequestId>,
}

/// A request as the report checks it: its operation hint and its text on disk.
#[derive(Debug, Clone)]
pub struct RequestText {
    pub id: RequestId,
    pub operation: Option<OperationRef>,
    pub text: String,
}

impl ReplaceReport {
    /// Compares `old` (the previous WSDL's supported operations) with `new` and validates every
    /// request against `new`. Without a usable `schema` (the new set does not compile), a
    /// request counts as invalid only when it isn't well-formed: nothing else can be checked.
    pub fn new(
        old: &[OperationRef],
        new: &Wsdl,
        schema: Option<&RequestSchema>,
        requests: &[RequestText],
    ) -> Self {
        let new_operations = new.supported_operations();
        let invalid = requests
            .iter()
            .filter(|r| match schema {
                Some(s) => validate_request(new, s, &r.text, r.operation.as_ref()).has_errors(),
                None => check_well_formed(&r.text).is_err(),
            })
            .map(|r| r.id)
            .collect();
        ReplaceReport {
            added: missing_from(&new_operations, old),
            removed: missing_from(old, &new_operations),
            invalid,
        }
    }

    /// Nothing was added or removed and every request still validates.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.invalid.is_empty()
    }
}

/// Every request of `project` with its text, in [`Project::requests`] order. A request whose
/// file can't be read can't be checked; it is left out rather than failing the report.
pub fn request_texts(project: &Project) -> Result<Vec<RequestText>, ProjectError> {
    Ok(project
        .requests()?
        .into_iter()
        .filter_map(|meta| {
            let text = project.read_request(meta.id).ok()?;
            Some(RequestText {
                id: meta.id,
                operation: meta.operation,
                text,
            })
        })
        .collect())
}

/// The operations of `a` that `b` lacks, in `a`'s order.
fn missing_from(a: &[OperationRef], b: &[OperationRef]) -> Vec<OperationRef> {
    a.iter().filter(|op| !b.contains(op)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{customer, legacy, read_fixture};

    fn request(fixture: &str, operation: Option<OperationRef>) -> RequestText {
        RequestText {
            id: RequestId::new(),
            operation,
            text: read_fixture(fixture),
        }
    }

    fn operation(w: &Wsdl, name: &str) -> OperationRef {
        w.supported_operations()
            .into_iter()
            .find(|op| op.operation == name)
            .expect("operation")
    }

    #[test]
    fn same_wsdl_changes_nothing_but_still_reports_broken_requests() {
        let w = customer();
        let schema = RequestSchema::compile(&w.bundle).expect("compiles");
        let get = operation(&w, "GetCustomer");
        let valid = request(
            "customer/requests/valid-get-customer.xml",
            Some(get.clone()),
        );
        let invalid = request("customer/requests/invalid-bad-simple-type.xml", Some(get));
        let report = ReplaceReport::new(
            &w.supported_operations(),
            &w,
            Some(&schema),
            &[valid, invalid.clone()],
        );
        assert!(
            report.added.is_empty() && report.removed.is_empty(),
            "{report:?}"
        );
        assert_eq!(report.invalid, [invalid.id]);
    }

    #[test]
    fn a_different_wsdl_adds_removes_and_invalidates() {
        let old = legacy();
        let new = customer();
        let schema = RequestSchema::compile(&new.bundle).expect("compiles");
        let lookup = request(
            "legacy-rpc/requests/valid-lookup.xml",
            Some(operation(&old, "Lookup")),
        );
        let report = ReplaceReport::new(
            &old.supported_operations(),
            &new,
            Some(&schema),
            std::slice::from_ref(&lookup),
        );
        assert_eq!(report.added, new.supported_operations());
        assert_eq!(report.removed, old.supported_operations());
        assert_eq!(report.invalid, [lookup.id]);
        assert!(!report.is_empty());
    }

    #[test]
    fn without_a_schema_only_well_formedness_counts() {
        let w = customer();
        let valid = request("customer/requests/invalid-bad-simple-type.xml", None);
        let broken = request("customer/requests/invalid-not-well-formed.xml", None);
        let report = ReplaceReport::new(
            &w.supported_operations(),
            &w,
            None,
            &[valid, broken.clone()],
        );
        assert_eq!(report.invalid, [broken.id]);
        assert!(!report.is_empty());
    }
}
