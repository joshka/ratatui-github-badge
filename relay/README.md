# Badge relay

A Cloudflare Worker that lets `cargo badge --relay` mirror and drive the badge over the
internet. The badge and the host both connect out to the relay, so this works on conference
Wi-Fi that isolates clients or filters inbound traffic.

- Each URL path is a room, run by one Durable Object. The badge and the host use the same URL.
- The badge sends its log and mirror stream as binary messages; hosts send command lines as
  text.
- Every connection must send `Authorization: Bearer <RELAY_TOKEN>`.
- When the last host leaves, the relay tells the badge `mirror off`.
- The Durable Object uses the WebSocket hibernation API, so an idle room costs nothing. A
  connected badge sends a perf line every 2 s, which keeps the room awake.

## Deploy

```sh
cd relay
npm install
npx wrangler secret put RELAY_TOKEN   # paste a long random string, e.g. from `openssl rand -hex 32`
npx wrangler deploy
```

Then add the URL and the same token to `badge.toml`, along with a network for the badge to join:

```toml
[[wifi]]
ssid = "Conference WiFi"
password = "hunter2"

[relay]
url = "wss://badge-relay.<your-subdomain>.workers.dev/badge/<your-handle>"
token = "<the same RELAY_TOKEN>"
```

Rebuild and flash with `cargo run --release`, then run `cargo badge --relay`.

## Local testing

```sh
echo 'RELAY_TOKEN=local-test-token' > .dev.vars
npx wrangler dev
```

Use `url = "ws://<your laptop's IP>:8787/badge/test"` in `badge.toml`. `ws://` skips TLS on the
badge.

## Security

The badge doesn't verify the relay's TLS certificate, since it has no certificate store.
Someone who can intercept the badge's traffic could read the token and the mirror stream. With
the token, they could press keys on the badge. BOOTSEL only works over USB.
