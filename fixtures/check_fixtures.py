#!/usr/bin/env python3
"""Reference oracle for the fixtures, using lxml (libxml2) directly.

Checks that every request in fixtures/*/requests/ produces the outcome stated in its
`<!-- expect: ... -->` first line. It is a deliberately small re-implementation of the
validation pipeline from docs/PLAN.md §5/§5.4 and serves two purposes:

  * proves the fixtures themselves are correct before the Rust code exists, and
  * documents the expected behaviour in executable form.

Usage: python3 -I fixtures/check_fixtures.py   (needs `lxml`)

Not part of the product and not run in CI; the Rust test suite is authoritative.
"""

import copy
import os
import re
import sys
import tempfile

from lxml import etree

HERE = os.path.dirname(os.path.abspath(__file__))
WSDL = "http://schemas.xmlsoap.org/wsdl/"
SOAP_B = "http://schemas.xmlsoap.org/wsdl/soap/"
XS = "http://www.w3.org/2001/XMLSchema"
SOAP11 = "http://schemas.xmlsoap.org/soap/envelope/"
SOAP12 = "http://www.w3.org/2003/05/soap-envelope"

NONET = etree.XMLParser(no_network=True, resolve_entities=False)


def load(path):
    return etree.parse(path, NONET)


def wsdl_closure(entry):
    """Entry WSDL plus all wsdl:import'ed WSDLs."""
    seen, todo, out = set(), [os.path.abspath(entry)], []
    while todo:
        p = todo.pop()
        if p in seen:
            continue
        seen.add(p)
        doc = load(p)
        out.append((p, doc))
        for imp in doc.getroot().iterfind(f"{{{WSDL}}}import"):
            todo.append(os.path.join(os.path.dirname(p), imp.get("location")))
    return out


def resolve_qname(el, value):
    prefix, _, local = value.rpartition(":")
    return (el.nsmap.get(prefix or None, ""), local)


def extract_inline_schemas(wsdls, tmp):
    """Write each wsdl:types/xs:schema to a file, carrying over in-scope namespaces."""
    paths = []
    for i, (p, doc) in enumerate(wsdls):
        for j, s in enumerate(doc.getroot().iterfind(f"{{{WSDL}}}types/{{{XS}}}schema")):
            nsmap = dict(s.nsmap)  # includes ancestors' declarations in lxml
            new = etree.Element(s.tag, nsmap=nsmap, attrib=dict(s.attrib))
            for child in s:
                new.append(copy.deepcopy(child))
            # Relative schemaLocations resolve against the WSDL's directory.
            for imp in new.iter(f"{{{XS}}}import", f"{{{XS}}}include"):
                loc = imp.get("schemaLocation")
                if loc:
                    imp.set("schemaLocation", os.path.join(os.path.dirname(p), loc))
            out = os.path.join(tmp, f"inline-{i}-{j}.xsd")
            etree.ElementTree(new).write(out)
            paths.append((s.get("targetNamespace", ""), out))
    return paths


def operations(wsdls):
    """Map body wrapper QName -> (style, binding/op name, parts) for supported operations."""
    messages, bindings, ops = {}, [], {}
    for _, doc in wsdls:
        r = doc.getroot()
        for m in r.iterfind(f"{{{WSDL}}}message"):
            messages[m.get("name")] = m
        bindings += list(r.iterfind(f"{{{WSDL}}}binding"))
    port_types = {}
    for _, doc in wsdls:
        for pt in doc.getroot().iterfind(f"{{{WSDL}}}portType"):
            port_types[pt.get("name")] = pt
    for b in bindings:
        sb = b.find(f"{{{SOAP_B}}}binding")
        if sb is None:
            continue  # SOAP 1.2 or other: unsupported
        pt = port_types[resolve_qname(b, b.get("type"))[1]]
        for bop in b.iterfind(f"{{{WSDL}}}operation"):
            so = bop.find(f"{{{SOAP_B}}}operation")
            style = (so.get("style") if so is not None else None) or sb.get("style", "document")
            body = bop.find(f"{{{WSDL}}}input/{{{SOAP_B}}}body")
            if body.get("use") != "literal":
                continue  # rpc/encoded: unsupported
            pop = pt.find(f"{{{WSDL}}}operation[@name='{bop.get('name')}']")
            msg = messages[resolve_qname(pop, pop.find(f"{{{WSDL}}}input").get("message"))[1]]
            parts = list(msg.iterfind(f"{{{WSDL}}}part"))
            if style == "rpc":
                ops[(body.get("namespace"), bop.get("name"))] = ("rpc", bop.get("name"), parts)
            else:
                for part in parts:
                    ops[resolve_qname(part, part.get("element"))] = ("document", bop.get("name"), [])
    return ops


def rpc_wrapper_schema(ops, inline, tmp):
    """Generate wrapper elements for rpc operations (§5.4), including the real schema of the
    same namespace instead of importing it (libxml2 imports a namespace only once)."""
    by_ns = {}
    for (ns, local), (style, _, parts) in ops.items():
        if style == "rpc":
            by_ns.setdefault(ns, []).append((local, parts))
    out = []
    for ns, wrappers in by_ns.items():
        s = etree.Element(f"{{{XS}}}schema", nsmap={"xs": XS}, targetNamespace=ns)
        same = [p for tns, p in inline if tns == ns]
        for p in same:
            etree.SubElement(s, f"{{{XS}}}include", schemaLocation=p)
        for local, parts in wrappers:
            el = etree.SubElement(s, f"{{{XS}}}element", name=local)
            seq = etree.SubElement(etree.SubElement(el, f"{{{XS}}}complexType"), f"{{{XS}}}sequence")
            for part in parts:
                ns_t, local_t = resolve_qname(part, part.get("type"))
                seq_el = etree.SubElement(seq, f"{{{XS}}}element", name=part.get("name"))
                seq_el.set("type", f"{{{ns_t}}}{local_t}")
        path = os.path.join(tmp, f"rpc-{abs(hash(ns))}.xsd")
        # Re-serialize with proper prefixes for the Clark-notation type values.
        text = etree.tostring(s).decode()
        nsdecl = {}
        def repl(m):
            uri, loc = m.group(1), m.group(2)
            pfx = nsdecl.setdefault(uri, f"t{len(nsdecl)}")
            return f'type="{pfx}:{loc}"'
        text = re.sub(r'type="\{([^}]*)\}([^"]*)"', repl, text)
        decls = " ".join(f'xmlns:{p}="{u}"' for u, p in nsdecl.items())
        text = text.replace("<xs:schema ", f"<xs:schema {decls} ", 1)
        with open(path, "w") as f:
            f.write(text)
        out.append((ns, path))
    return out


def compile_schema(inline, rpc, tmp):
    rpc_ns = {ns for ns, _ in rpc}
    root = etree.Element(f"{{{XS}}}schema", nsmap={"xs": XS}, targetNamespace="urn:washboard:root")
    for ns, p in rpc:
        etree.SubElement(root, f"{{{XS}}}import", namespace=ns, schemaLocation=p)
    for ns, p in inline:
        if ns not in rpc_ns:
            etree.SubElement(root, f"{{{XS}}}import", namespace=ns, schemaLocation=p)
    path = os.path.join(tmp, "root.xsd")
    etree.ElementTree(root).write(path)
    return etree.XMLSchema(load(path))


def expectations(path):
    with open(path, encoding="utf-8") as f:
        first = f.readline()
    m = re.match(r"<!-- expect: (valid|error line (\d+): (.+?)) -->", first)
    if not m:
        raise SystemExit(f"{path}: missing expect comment")
    return None if m.group(1) == "valid" else (int(m.group(2)), m.group(3))


def check_request(path, schema, ops):
    """Returns list of (line, message)."""
    try:
        doc = etree.parse(path, NONET)
    except etree.XMLSyntaxError as e:
        return [(e.lineno, f"not well-formed: {e.msg}")]
    env = doc.getroot()
    if env.tag == f"{{{SOAP12}}}Envelope":
        return [(env.sourceline, "SOAP 1.2 envelopes are not supported")]
    if env.tag != f"{{{SOAP11}}}Envelope":
        return [(env.sourceline, "root is not a SOAP 1.1 Envelope")]
    errs = []
    header = env.find(f"{{{SOAP11}}}Header")
    body = env.find(f"{{{SOAP11}}}Body")
    blocks = list(header) if header is not None else []
    for child in body:
        qn = etree.QName(child)
        if (qn.namespace, qn.localname) not in ops:
            errs.append((child.sourceline, f"no operation for body element {qn}"))
        else:
            blocks.append(child)
    for block in blocks:
        if not isinstance(block.tag, str):
            continue
        if not schema.validate(etree.ElementTree(block)):
            errs += [(e.line, e.message) for e in schema.error_log]
    return errs


def main():
    failures = 0
    for project, entry in [("customer", "CustomerService.wsdl"), ("legacy-rpc", "Legacy.wsdl")]:
        pdir = os.path.join(HERE, project)
        with tempfile.TemporaryDirectory() as tmp:
            wsdls = wsdl_closure(os.path.join(pdir, entry))
            inline = extract_inline_schemas(wsdls, tmp)
            ops = operations(wsdls)
            rpc = rpc_wrapper_schema(ops, inline, tmp)
            schema = compile_schema(inline, rpc, tmp)
            rdir = os.path.join(pdir, "requests")
            for name in sorted(os.listdir(rdir)):
                path = os.path.join(rdir, name)
                exp = expectations(path)
                errs = check_request(path, schema, ops)
                if exp is None:
                    ok = not errs
                else:
                    line, needle = exp
                    ok = any(l == line and needle.lower() in m.lower() for l, m in errs)
                status = "ok  " if ok else "FAIL"
                failures += not ok
                print(f"{status} {project}/{name}")
                if not ok or "-v" in sys.argv:
                    for l, m in errs:
                        print(f"       line {l}: {m}")
    print(f"libxml2 {etree.LIBXML_VERSION}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
