# No network except configured servers

- The only code that opens sockets is `core::http`, and it only accepts a `Server` value.
- No update checker, analytics, crash reporter, or remote fonts/images. Sparkle etc. excluded.
- libxml2: custom entity loader + `XML_PARSE_NONET` ([schema bundle](schema-bundle.md)).
- Redirects disabled; a 3xx is shown as the response.
- Proxy: ureq's env proxy detection disabled; system proxy settings are ignored. No proxy support.
- DNS lookups for the server host are, of course, still made.
- CI check: `cargo deny check bans` with a ban list in `deny.toml` (no `reqwest`, `hyper`,
  telemetry crates), and `washboard-core/tests/network_boundary.rs`: network-capable crates
  reach the dependency graph only through core's `ureq` dependency, and socket/HTTP APIs appear
  in source only under `core/src/http/`. The one exception is `washboard-test-support`, whose
  local HTTP server answers the send tests; the same test checks that every crate takes it as a
  dev-dependency only, so it never ships.
- Optional (later): App Sandbox with only `com.apple.security.network.client`; doesn't restrict
  hosts but limits blast radius.
