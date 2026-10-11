# Send

- Toolbar: server popup, Send (⌘↩). Sends to `server.url` with `Content-Type: text/xml;
  charset=utf-8` and the `SOAPAction` header from the binding operation ([WSDL](wsdl.md)).
- The request is validated first; **Send is blocked** while it has errors
  ([validation](validation.md)). It is sent as written: Send never formats it.
- Basic auth: `Authorization` header built up front (preemptive, no 401 round-trip).
- TLS: platform verification; if the server has "ignore TLS errors", certificate and hostname
  checks are disabled for that request only. The log records whether verification was skipped.
- No redirects and no proxies ([network](network.md)).
- The exchange goes into the request's history ([response pane](response-pane.md)) and the
  app's [HTTP log](http-log.md).
