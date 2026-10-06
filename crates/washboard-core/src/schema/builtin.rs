//! XSD 1.0 built-in datatypes: their derivation and template placeholders.

/// A built-in type, identified by its index in [`BUILTINS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Builtin(pub(crate) u8);

pub(crate) struct BuiltinInfo {
    pub name: &'static str,
    pub base: Option<&'static str>,
    /// Template placeholder; replaced by the user, but chosen to be valid where that is cheap.
    pub placeholder: &'static str,
    pub numeric: bool,
}

const fn b(
    name: &'static str,
    base: Option<&'static str>,
    placeholder: &'static str,
    numeric: bool,
) -> BuiltinInfo {
    BuiltinInfo {
        name,
        base,
        placeholder,
        numeric,
    }
}

pub(crate) static BUILTINS: &[BuiltinInfo] = &[
    b("anyType", None, "?", false),
    b("anySimpleType", Some("anyType"), "?", false),
    b("string", Some("anySimpleType"), "?", false),
    b("normalizedString", Some("string"), "?", false),
    b("token", Some("normalizedString"), "?", false),
    b("language", Some("token"), "en", false),
    b("Name", Some("token"), "?", false),
    b("NCName", Some("Name"), "?", false),
    b("ID", Some("NCName"), "?", false),
    b("IDREF", Some("NCName"), "?", false),
    b("IDREFS", Some("anySimpleType"), "?", false),
    b("ENTITY", Some("NCName"), "?", false),
    b("ENTITIES", Some("anySimpleType"), "?", false),
    b("NMTOKEN", Some("token"), "?", false),
    b("NMTOKENS", Some("anySimpleType"), "?", false),
    b("boolean", Some("anySimpleType"), "false", false),
    b("decimal", Some("anySimpleType"), "0", true),
    b("integer", Some("decimal"), "0", true),
    b("nonPositiveInteger", Some("integer"), "0", true),
    b("negativeInteger", Some("nonPositiveInteger"), "-1", true),
    b("long", Some("integer"), "0", true),
    b("int", Some("long"), "0", true),
    b("short", Some("int"), "0", true),
    b("byte", Some("short"), "0", true),
    b("nonNegativeInteger", Some("integer"), "0", true),
    b("unsignedLong", Some("nonNegativeInteger"), "0", true),
    b("unsignedInt", Some("unsignedLong"), "0", true),
    b("unsignedShort", Some("unsignedInt"), "0", true),
    b("unsignedByte", Some("unsignedShort"), "0", true),
    b("positiveInteger", Some("nonNegativeInteger"), "1", true),
    b("float", Some("anySimpleType"), "0", true),
    b("double", Some("anySimpleType"), "0", true),
    b("duration", Some("anySimpleType"), "P0D", false),
    b(
        "dateTime",
        Some("anySimpleType"),
        "2026-01-01T00:00:00",
        false,
    ),
    b("time", Some("anySimpleType"), "00:00:00", false),
    b("date", Some("anySimpleType"), "2026-01-01", false),
    b("gYearMonth", Some("anySimpleType"), "2026-01", false),
    b("gYear", Some("anySimpleType"), "2026", false),
    b("gMonthDay", Some("anySimpleType"), "--01-01", false),
    b("gDay", Some("anySimpleType"), "---01", false),
    b("gMonth", Some("anySimpleType"), "--01", false),
    b("hexBinary", Some("anySimpleType"), "", false),
    b("base64Binary", Some("anySimpleType"), "", false),
    b("anyURI", Some("anySimpleType"), "?", false),
    b("QName", Some("anySimpleType"), "?", false),
    b("NOTATION", Some("anySimpleType"), "?", false),
];

pub(crate) const ANY_TYPE: Builtin = Builtin(0);
pub(crate) const ANY_SIMPLE_TYPE: Builtin = Builtin(1);

impl Builtin {
    pub(crate) fn by_name(local: &str) -> Option<Builtin> {
        BUILTINS
            .iter()
            .position(|b| b.name == local)
            .and_then(|i| u8::try_from(i).ok())
            .map(Builtin)
    }

    pub(crate) fn info(self) -> &'static BuiltinInfo {
        // Builtin values are only created from indexes into BUILTINS.
        &BUILTINS[usize::from(self.0)]
    }

    pub(crate) fn base(self) -> Option<Builtin> {
        self.info().base.and_then(Builtin::by_name)
    }

    pub(crate) fn is_boolean(self) -> bool {
        self.info().name == "boolean"
    }

    pub(crate) fn is_any_type(self) -> bool {
        self == ANY_TYPE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_base_exists() {
        for info in BUILTINS {
            if let Some(base) = info.base {
                assert!(Builtin::by_name(base).is_some(), "{base}");
            }
        }
        assert_eq!(Builtin::by_name("anyType"), Some(ANY_TYPE));
        assert_eq!(Builtin::by_name("anySimpleType"), Some(ANY_SIMPLE_TYPE));
    }
}
