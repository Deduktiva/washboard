//! WSDL 1.1 model: definitions merged across `wsdl:import`, services, ports, bindings,
//! operations, messages, SOAP 1.1 binding details (style, soapAction, body/header parts),
//! plus extraction of inline `wsdl:types` schemas and the import graph.
//!
//! Owned by WP-WSDL (`docs/TASKS.md`). Details: `docs/PLAN.md` §4 (import check), §5, §5.3, §5.4.
