//! SOAP 1.1 specifics: envelope namespace, fault parsing, envelope construction for templates.

/// SOAP 1.1 envelope namespace. SOAP 1.2 is not supported.
pub const SOAP11_ENV_NS: &str = "http://schemas.xmlsoap.org/soap/envelope/";
/// WSDL SOAP 1.1 binding namespace (`soap:binding`, `soap:operation`, `soap:body`, …).
pub const WSDL_SOAP11_NS: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
/// WSDL SOAP 1.2 binding namespace; bindings using it are listed as unsupported.
pub const WSDL_SOAP12_NS: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";
pub const WSDL_NS: &str = "http://schemas.xmlsoap.org/wsdl/";
pub const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
pub const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";
