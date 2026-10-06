# Test fixtures

Shared ground truth for all work packages. Small on purpose; each file exists to exercise
specific features from `docs/PLAN.md`. Do not "clean up" these files: BOMs, odd prefixes and
deliberate errors are the point. `.gitattributes` keeps them byte-exact.

## `customer/` — document/literal, multi-file

| File | Exercises |
|---|---|
| `CustomerService.wsdl` | UTF-8 BOM; `wsdl:import`; service with a SOAP 1.1 and a SOAP 1.2 port |
| `CustomerBinding.wsdl` | inline schema using prefixes declared only on `wsdl:definitions`; `soap:header part="header" use="literal"`; one op with `soapAction`, one without; SOAP 1.2 binding (must show as unsupported) |
| `xsd/customer.xsd` | import level 2; `xs:any ##other lax`; abstract type usage; abstract element `ref`; enums; attributes |
| `xsd/common/party.xsd` | import level 3; UTF-8 BOM; `xs:include`; abstract type with 2-level derivation (`Party → Company → PublicCompany`); substitution group (`ContactMethod` ← `Email`, `Phone`) |
| `xsd/common/party-ids.xsd` | level 4 via `xs:include`; patterns |
| `xsd/ext/audit.xsd` | element matched by the lax wildcard |

## `legacy-rpc/` — rpc/literal

`Legacy.wsdl`: rpc/literal binding whose `soap:body namespace` equals the inline schema's
`targetNamespace` (the §5.4 gotcha), `type=` parts, plus an rpc/encoded binding that must load
as unsupported.

## Requests and expectations

Every `requests/*.xml` starts with exactly one of:

```
<!-- expect: valid -->
<!-- expect: error line N: substring -->
```

An `error` expectation is met if validation yields at least one error on line `N` whose message
contains `substring` (case-insensitive). Line `N` is the line of the element's start tag.
All start tags carrying a checked error are on a single line, because libxml2 reports the line
where a start tag *ends* (see WP-VALIDATE in `docs/TASKS.md`).

## Oracle

`check_fixtures.py` validates every request with lxml/libxml2 using the pipeline from the plan
(inline schema extraction with namespace carry-over, rpc wrapper generation with `xs:include`,
dispatch by body QName). Run `python3 -I fixtures/check_fixtures.py [-v]` after changing a
fixture. It is a reference for behaviour, not product code.
