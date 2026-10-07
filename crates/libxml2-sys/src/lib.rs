//! FFI bindings to libxml2: parser, XML Schema compile/validate, structured errors,
//! resource loader.
//!
//! Owned by WP-LIBXML2 (`docs/TASKS.md`). The default build compiles a pinned libxml2
//! release from source and links it statically, with HTTP/FTP support disabled.
//! Setting `WASHBOARD_LIBXML2=pkg-config` links a system copy found via pkg-config instead
//! (Homebrew on macOS, distro package on Linux) — for faster local builds only, never for
//! release builds. See `docs/PLAN.md` §5 "libxml2: vendored, not system" and `README.md`.
//!
//! Hand-written for exactly what `washboard_core::validate` uses; no bindgen. Everything here
//! is checked against the 2.15 headers and needs at least 2.14 (resource loaders). Structs
//! are declared only as far as we read them and are only ever used behind pointers.
//! The safe wrapper lives in `washboard-core/src/validate/xsd.rs`.

#![allow(non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_int, c_long, c_ushort, c_void};

/// `xmlChar` is a UTF-8 byte.
pub type xmlChar = u8;

/// Declares opaque types: only ever handled through raw pointers.
macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        #[derive(Debug)]
        pub struct $name {
            _data: [u8; 0],
            _marker: core::marker::PhantomData<(*mut u8, core::marker::PhantomPinned)>,
        }
    )*};
}

opaque!(
    xmlAttr,
    xmlParserCtxt,
    xmlParserInput,
    xmlSchema,
    xmlSchemaParserCtxt,
    xmlSchemaValidCtxt,
);

// --- xmlerror.h -----------------------------------------------------------------------------

/// `xmlErrorLevel`
pub type xmlErrorLevel = c_int;
pub const XML_ERR_NONE: xmlErrorLevel = 0;
pub const XML_ERR_WARNING: xmlErrorLevel = 1;
pub const XML_ERR_ERROR: xmlErrorLevel = 2;
pub const XML_ERR_FATAL: xmlErrorLevel = 3;

/// `xmlErrorDomain` (subset)
pub const XML_FROM_PARSER: c_int = 1;
pub const XML_FROM_IO: c_int = 8;
pub const XML_FROM_SCHEMASP: c_int = 16;
pub const XML_FROM_SCHEMASV: c_int = 17;

/// `xmlParserErrors` (subset). Returned by resource loaders.
pub type xmlParserErrors = c_int;
pub const XML_ERR_OK: xmlParserErrors = 0;
pub const XML_ERR_NO_MEMORY: xmlParserErrors = 2;
pub const XML_IO_UNKNOWN: xmlParserErrors = 1500;
pub const XML_IO_ENOENT: xmlParserErrors = 1524;
pub const XML_IO_NETWORK_ATTEMPT: xmlParserErrors = 1543;
/// The XML declaration names another encoding than the input's.
pub const XML_WAR_ENCODING_MISMATCH: xmlParserErrors = 113;
// Schema validity errors (`XML_FROM_SCHEMASV`). The numbering is contiguous in `xmlerror.h`,
// so the first and last of a run also bound it in a range pattern.
pub const XML_SCHEMAV_VALUE: xmlParserErrors = 1822;
pub const XML_SCHEMAV_CVC_DATATYPE_VALID_1_2_3: xmlParserErrors = 1826;
pub const XML_SCHEMAV_CVC_TYPE_3_1_2: xmlParserErrors = 1828;
pub const XML_SCHEMAV_CVC_ENUMERATION_VALID: xmlParserErrors = 1840;
pub const XML_SCHEMAV_CVC_COMPLEX_TYPE_2_2: xmlParserErrors = 1842;
/// "The element declaration is abstract".
pub const XML_SCHEMAV_CVC_ELT_2: xmlParserErrors = 1846;
pub const XML_SCHEMAV_CVC_ELT_5_2_1: xmlParserErrors = 1855;
pub const XML_SCHEMAV_CVC_ELT_5_2_2_2_2: xmlParserErrors = 1858;
pub const XML_SCHEMAV_CVC_AU: xmlParserErrors = 1874;
/// "The type definition is abstract".
pub const XML_SCHEMAV_CVC_TYPE_2: xmlParserErrors = 1876;

/// `struct _xmlError` (public, layout stable since 2.0).
#[repr(C)]
#[derive(Debug)]
pub struct xmlError {
    pub domain: c_int,
    pub code: c_int,
    pub message: *mut c_char,
    pub level: xmlErrorLevel,
    pub file: *mut c_char,
    pub line: c_int,
    pub str1: *mut c_char,
    pub str2: *mut c_char,
    pub str3: *mut c_char,
    pub int1: c_int,
    /// Column for parser errors (0 if unknown).
    pub int2: c_int,
    pub ctxt: *mut c_void,
    /// The `xmlNode` the error refers to, if any.
    pub node: *mut c_void,
}

pub type xmlStructuredErrorFunc =
    Option<unsafe extern "C" fn(user: *mut c_void, error: *const xmlError)>;

// --- tree.h ---------------------------------------------------------------------------------

/// `xmlElementType` (subset)
pub type xmlElementType = c_int;
pub const XML_ELEMENT_NODE: xmlElementType = 1;
pub const XML_ATTRIBUTE_NODE: xmlElementType = 2;
pub const XML_ENTITY_REF_NODE: xmlElementType = 5;
pub const XML_DOCUMENT_NODE: xmlElementType = 9;

/// `struct _xmlNode` (public). `xmlAttr` shares the fields up to `doc`.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNode {
    pub _private: *mut c_void,
    pub type_: xmlElementType,
    /// Local name for elements and attributes.
    pub name: *const xmlChar,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub ns: *mut c_void,
    pub content: *mut xmlChar,
    pub properties: *mut c_void,
    pub nsDef: *mut c_void,
    pub psvi: *mut c_void,
    pub line: c_ushort,
    pub extra: c_ushort,
}

/// Leading fields of `struct _xmlNs` up to `prefix`; what `xmlNode::ns` points at. Never use
/// by value.
#[repr(C)]
#[derive(Debug)]
pub struct xmlNs {
    pub next: *mut xmlNs,
    pub type_: c_int,
    /// Namespace URI.
    pub href: *const xmlChar,
    pub prefix: *const xmlChar,
}

/// Leading fields of `struct _xmlDoc` up to `URL`. Never use by value.
#[repr(C)]
#[derive(Debug)]
pub struct xmlDoc {
    pub _private: *mut c_void,
    pub type_: xmlElementType,
    pub name: *mut c_char,
    pub children: *mut xmlNode,
    pub last: *mut xmlNode,
    pub parent: *mut xmlNode,
    pub next: *mut xmlNode,
    pub prev: *mut xmlNode,
    pub doc: *mut xmlDoc,
    pub compression: c_int,
    pub standalone: c_int,
    pub intSubset: *mut c_void,
    pub extSubset: *mut c_void,
    pub oldNs: *mut c_void,
    pub version: *const xmlChar,
    pub encoding: *const xmlChar,
    pub ids: *mut c_void,
    pub refs: *mut c_void,
    /// The document's base URI: whatever URL the resource loader gave its input.
    pub URL: *const xmlChar,
}

// --- parser.h -------------------------------------------------------------------------------

/// `xmlParserOption` (subset)
pub const XML_PARSE_NOENT: c_int = 1 << 1;
pub const XML_PARSE_NOERROR: c_int = 1 << 5;
pub const XML_PARSE_NOWARNING: c_int = 1 << 6;
pub const XML_PARSE_NONET: c_int = 1 << 11;
pub const XML_PARSE_NODICT: c_int = 1 << 12;
pub const XML_PARSE_BIG_LINES: c_int = 1 << 22;
pub const XML_PARSE_NO_XXE: c_int = 1 << 23;

/// `xmlResourceType`
pub type xmlResourceType = c_int;
pub const XML_RESOURCE_UNKNOWN: xmlResourceType = 0;
pub const XML_RESOURCE_MAIN_DOCUMENT: xmlResourceType = 1;
pub const XML_RESOURCE_DTD: xmlResourceType = 2;
pub const XML_RESOURCE_GENERAL_ENTITY: xmlResourceType = 3;
pub const XML_RESOURCE_PARAMETER_ENTITY: xmlResourceType = 4;

/// `xmlParserInputFlags`
pub type xmlParserInputFlags = c_int;
pub const XML_INPUT_BUF_STATIC: xmlParserInputFlags = 1 << 1;
pub const XML_INPUT_BUF_ZERO_TERMINATED: xmlParserInputFlags = 1 << 2;
pub const XML_INPUT_UNZIP: xmlParserInputFlags = 1 << 3;
pub const XML_INPUT_NETWORK: xmlParserInputFlags = 1 << 4;

/// Custom resource loader (since 2.14). `ctxt` is the user data registered with the loader.
/// On success, set `*out` to a new input and return [`XML_ERR_OK`].
pub type xmlResourceLoader = Option<
    unsafe extern "C" fn(
        ctxt: *mut c_void,
        url: *const c_char,
        public_id: *const c_char,
        type_: xmlResourceType,
        flags: xmlParserInputFlags,
        out: *mut *mut xmlParserInput,
    ) -> xmlParserErrors,
>;

/// Process-global fallback loader, used wherever no per-context loader is set. Return null
/// to refuse.
pub type xmlExternalEntityLoader = Option<
    unsafe extern "C" fn(
        url: *const c_char,
        public_id: *const c_char,
        context: *mut xmlParserCtxt,
    ) -> *mut xmlParserInput,
>;

unsafe extern "C" {
    // parser.h
    pub fn xmlInitParser();
    /// Deprecated upstream and not thread-safe: an unsynchronized write of a global. Call
    /// once, before any parsing, and never change it afterwards.
    pub fn xmlSetExternalEntityLoader(f: xmlExternalEntityLoader);
    pub fn xmlNewParserCtxt() -> *mut xmlParserCtxt;
    pub fn xmlFreeParserCtxt(ctxt: *mut xmlParserCtxt);
    pub fn xmlCtxtSetErrorHandler(
        ctxt: *mut xmlParserCtxt,
        handler: xmlStructuredErrorFunc,
        data: *mut c_void,
    );
    pub fn xmlCtxtSetResourceLoader(
        ctxt: *mut xmlParserCtxt,
        loader: xmlResourceLoader,
        vctxt: *mut c_void,
    );
    pub fn xmlCtxtReadMemory(
        ctxt: *mut xmlParserCtxt,
        buffer: *const c_char,
        size: c_int,
        url: *const c_char,
        encoding: *const c_char,
        options: c_int,
    ) -> *mut xmlDoc;
    /// Copies `mem` unless `XML_INPUT_BUF_STATIC` is given; `url` is the base URI.
    pub fn xmlNewInputFromMemory(
        url: *const c_char,
        mem: *const c_void,
        size: usize,
        flags: xmlParserInputFlags,
    ) -> *mut xmlParserInput;

    // tree.h
    pub fn xmlFreeDoc(doc: *mut xmlDoc);
    pub fn xmlDocGetRootElement(doc: *const xmlDoc) -> *mut xmlNode;
    pub fn xmlGetLineNo(node: *const xmlNode) -> c_long;
    /// The attribute `name` in namespace `ns` (null: no namespace) of `node`, or null.
    pub fn xmlHasNsProp(
        node: *const xmlNode,
        name: *const xmlChar,
        ns: *const xmlChar,
    ) -> *mut xmlAttr;
    /// Unlinks and frees `cur`. 0 on success.
    pub fn xmlRemoveProp(cur: *mut xmlAttr) -> c_int;

    // xmlschemas.h
    pub fn xmlSchemaNewParserCtxt(url: *const c_char) -> *mut xmlSchemaParserCtxt;
    pub fn xmlSchemaFreeParserCtxt(ctxt: *mut xmlSchemaParserCtxt);
    pub fn xmlSchemaSetParserStructuredErrors(
        ctxt: *mut xmlSchemaParserCtxt,
        serror: xmlStructuredErrorFunc,
        ctx: *mut c_void,
    );
    pub fn xmlSchemaSetResourceLoader(
        ctxt: *mut xmlSchemaParserCtxt,
        loader: xmlResourceLoader,
        data: *mut c_void,
    );
    pub fn xmlSchemaParse(ctxt: *mut xmlSchemaParserCtxt) -> *mut xmlSchema;
    pub fn xmlSchemaFree(schema: *mut xmlSchema);
    pub fn xmlSchemaNewValidCtxt(schema: *mut xmlSchema) -> *mut xmlSchemaValidCtxt;
    pub fn xmlSchemaFreeValidCtxt(ctxt: *mut xmlSchemaValidCtxt);
    pub fn xmlSchemaSetValidStructuredErrors(
        ctxt: *mut xmlSchemaValidCtxt,
        serror: xmlStructuredErrorFunc,
        ctx: *mut c_void,
    );
    /// 0 if valid, a positive error code if invalid, -1 on internal error.
    pub fn xmlSchemaValidateDoc(ctxt: *mut xmlSchemaValidCtxt, instance: *mut xmlDoc) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layout of the structs we read, as printed by a C probe against the 2.15 headers
    /// on LP64 (Linux x86_64/aarch64, macOS).
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn struct_layout_matches_headers() {
        use std::mem::{offset_of, size_of};
        assert_eq!(size_of::<xmlError>(), 88);
        assert_eq!(offset_of!(xmlError, node), 80);
        assert_eq!(size_of::<xmlNode>(), 120);
        assert_eq!(offset_of!(xmlNode, type_), 8);
        assert_eq!(offset_of!(xmlNode, parent), 40);
        assert_eq!(offset_of!(xmlNode, next), 48);
        assert_eq!(offset_of!(xmlNode, doc), 64);
        assert_eq!(offset_of!(xmlDoc, URL), 136);
    }
}
