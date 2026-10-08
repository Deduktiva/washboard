//! Parsing WSDL and XSD text into a `roxmltree` tree, with one set of options.

/// Parses decoded WSDL or XSD text. Internal DTD subsets occur in old WSDLs and in
/// XMLSchema-style documents, so they are allowed; roxmltree never loads external entities and
/// guards against entity expansion bombs.
///
/// Requests and responses are parsed with roxmltree's defaults instead: SOAP 1.1 forbids a
/// DTD in a message.
pub(crate) fn parse_wsdl_or_xsd(text: &str) -> Result<roxmltree::Document<'_>, roxmltree::Error> {
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(text, opts)
}
