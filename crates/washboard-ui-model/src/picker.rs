//! The operation picker's list (Project ▸ New Request…, PLAN §4 "Requests"): which operations
//! a search shows, grouped and ordered. The front end only draws the result.

use crate::window::{OperationNode, PortNode, ServiceNode};

/// One group of the picker's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerSection {
    /// "Service › Port", or "Unsupported" for the operations that cannot be created, which
    /// come last. `None` when the project has a single supported port: its operations need no
    /// header, even when a SOAP 1.2 or rpc/encoded port sits next to it.
    pub title: Option<String>,
    pub rows: Vec<OperationNode>,
}

/// The sections showing the operations of `services` that match `query`. A row matches when
/// every space-separated word of the query is a case-insensitive substring of its operation
/// name, input element, service or port name. Within a section, operations whose name starts
/// with the query come first; otherwise everything stays in WSDL order. Sections without a
/// match are left out, so no match at all gives an empty list.
pub fn pick_operations(services: &[ServiceNode], query: &str) -> Vec<PickerSection> {
    let query = query.trim().to_lowercase();
    let words: Vec<&str> = query.split_whitespace().collect();
    let supported = |p: &PortNode| p.operations.iter().any(|o| o.unsupported.is_none());
    let ports = services.iter().flat_map(|s| &s.ports);
    let headers = ports.filter(|p| supported(p)).count() > 1;
    let mut sections = Vec::new();
    let mut unsupported = Vec::new();
    for service in services {
        for port in &service.ports {
            let (rows, cannot): (Vec<_>, Vec<_>) = port
                .operations
                .iter()
                .filter(|op| {
                    let input = op.input.rsplit(':').next().unwrap_or_default();
                    let fields =
                        [op.name(), input, &service.name, &port.name].map(str::to_lowercase);
                    words.iter().all(|w| fields.iter().any(|f| f.contains(w)))
                })
                .cloned()
                .partition(|op| op.unsupported.is_none());
            unsupported.extend(cannot);
            if !rows.is_empty() {
                let title = headers.then(|| format!("{} › {}", service.name, port.name));
                sections.push(PickerSection { title, rows });
            }
        }
    }
    if !unsupported.is_empty() {
        sections.push(PickerSection {
            title: Some("Unsupported".into()),
            rows: unsupported,
        });
    }
    for section in &mut sections {
        // Stable: each half keeps WSDL order.
        section
            .rows
            .sort_by_key(|op| !op.name().to_lowercase().starts_with(&query));
    }
    sections
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use washboard_core::model::{OperationRef, QName};
    use washboard_core::schema::SchemaModel;
    use washboard_core::wsdl::{self, Sources};

    use super::*;
    use crate::window::operation_tree;

    fn tree(entry: &str) -> Vec<ServiceNode> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(entry);
        let dir = path.parent().expect("a folder").to_owned();
        let wsdl = wsdl::load(&Sources::from_disk(&path, &[dir]).expect("sources"));
        operation_tree(&wsdl, &SchemaModel::build(&wsdl.bundle))
    }

    /// Each section's title and its rows as `name (input)`.
    fn shown(sections: &[PickerSection]) -> Vec<(Option<&str>, Vec<String>)> {
        sections
            .iter()
            .map(|s| {
                let rows = s.rows.iter().map(|r| format!("{} ({})", r.name(), r.input));
                (s.title.as_deref(), rows.collect())
            })
            .collect()
    }

    fn op(name: &str) -> OperationNode {
        OperationNode {
            operation: OperationRef {
                binding: QName::new("urn:t", "B"),
                operation: name.into(),
            },
            unsupported: None,
            input: format!("t:{name}Request"),
        }
    }

    fn service(name: &str, ports: &[(&str, &[&str])]) -> ServiceNode {
        ServiceNode {
            name: name.into(),
            ports: ports
                .iter()
                .map(|(port, ops)| PortNode {
                    name: (*port).into(),
                    protocol: None,
                    operations: ops.iter().map(|o| op(o)).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn one_supported_port_needs_no_header() {
        let customer = tree("customer/CustomerService.wsdl");
        assert_eq!(
            shown(&pick_operations(&customer, "")),
            [
                (
                    None,
                    vec![
                        "GetCustomer (msg:GetCustomer)".to_string(),
                        "CreateOrder (msg:CreateOrder)".to_string(),
                    ]
                ),
                (
                    Some("Unsupported"),
                    vec!["GetCustomer (msg:GetCustomer)".to_string()]
                ),
            ]
        );
        let legacy = tree("legacy-rpc/Legacy.wsdl");
        assert_eq!(
            shown(&pick_operations(&legacy, "")),
            [
                (None, vec!["Lookup (rpc)".to_string()]),
                (Some("Unsupported"), vec!["Lookup (rpc)".to_string()]),
            ]
        );
    }

    #[test]
    fn two_supported_ports_get_headers() {
        let services = [
            service("Shop", &[("Orders", &["Create", "List"])]),
            service("Admin", &[("Users", &["Create"])]),
        ];
        let sections = pick_operations(&services, "");
        let titles: Vec<_> = sections.iter().map(|s| s.title.as_deref()).collect();
        assert_eq!(titles, [Some("Shop › Orders"), Some("Admin › Users")]);
        assert_eq!(
            shown(&pick_operations(&services, "admin")),
            [(
                Some("Admin › Users"),
                vec!["Create (t:CreateRequest)".to_string()]
            )]
        );
    }

    #[test]
    fn every_word_matches_somewhere_in_any_order() {
        let services = [
            service("Shop", &[("Orders", &["Recreate", "Create", "List"])]),
            service("Admin", &[("Users", &["Create"])]),
        ];
        let names = |query: &str| -> Vec<String> {
            pick_operations(&services, query)
                .iter()
                .flat_map(|s| s.rows.iter().map(|r| r.name().to_string()))
                .collect()
        };
        assert_eq!(names("orders cre"), ["Recreate", "Create"]);
        assert_eq!(names("ORDERS CRE"), ["Recreate", "Create"]);
        // The input element's local name, not its prefix.
        assert_eq!(names("listrequest"), ["List"]);
        assert!(names("t:").is_empty());
        assert!(names("nothing").is_empty());
        // Prefix matches first, then the rest, each in WSDL order.
        assert_eq!(names("cre"), ["Create", "Recreate", "Create"]);
    }

    #[test]
    fn unsupported_operations_keep_their_section() {
        let legacy = tree("legacy-rpc/Legacy.wsdl");
        let sections = pick_operations(&legacy, "encoded");
        assert_eq!(
            shown(&sections),
            [(Some("Unsupported"), vec!["Lookup (rpc)".to_string()])]
        );
        assert!(sections[0].rows[0].unsupported.is_some());
    }
}
