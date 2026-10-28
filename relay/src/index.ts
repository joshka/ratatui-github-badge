// Relays a badge's WebSocket to `cargo badge --relay`. Both connect out to the same path, so
// neither needs to reach the other directly. Each path is a room, run by one Durable Object.
//
// The badge sends its output as binary messages, which go to every host in the room. Hosts send
// command lines as text, which go to the badge.

import { DurableObject } from "cloudflare:workers";

interface Env {
  ROOMS: DurableObjectNamespace<Room>;
  /// Set with `wrangler secret put RELAY_TOKEN`; badge.toml's relay.token must match.
  RELAY_TOKEN: string;
}

type Role = "badge" | "host";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected a WebSocket upgrade\n", { status: 426 });
    }
    if (!env.RELAY_TOKEN || !(await tokenMatches(request, env.RELAY_TOKEN))) {
      return new Response("bad token\n", { status: 401 });
    }
    const url = new URL(request.url);
    const role = url.searchParams.get("role");
    if (role !== "badge" && role !== "host") {
      return new Response("role must be badge or host\n", { status: 400 });
    }
    const room = env.ROOMS.get(env.ROOMS.idFromName(url.pathname));
    return room.fetch(request);
  },
};

/// Compare digests, so the comparison takes the same time however much of the token matches.
async function tokenMatches(request: Request, expected: string): Promise<boolean> {
  const given = request.headers.get("Authorization")?.replace(/^Bearer /, "") ?? "";
  const digest = async (text: string) =>
    new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
  const [a, b] = await Promise.all([digest(given), digest(expected)]);
  return crypto.subtle.timingSafeEqual(a, b);
}

export class Room extends DurableObject<Env> {
  async fetch(request: Request): Promise<Response> {
    const role = new URL(request.url).searchParams.get("role") as Role;
    // A reconnecting badge replaces its old, possibly half-dead, connection.
    if (role === "badge") {
      for (const old of this.ctx.getWebSockets("badge")) old.close(1000, "replaced");
    }
    const [client, server] = Object.values(new WebSocketPair());
    // Hibernation: the object can sleep between messages without dropping the sockets.
    this.ctx.acceptWebSocket(server, [role]);
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const to: Role = this.role(ws) === "badge" ? "host" : "badge";
    for (const peer of this.ctx.getWebSockets(to)) {
      try {
        peer.send(message);
      } catch {
        // Closing; webSocketClose cleans up.
      }
    }
  }

  // The runtime answers the close handshake itself (compatibility date 2026 and later).
  async webSocketClose(ws: WebSocket): Promise<void> {
    // Stop the badge streaming frames nobody is watching.
    if (this.role(ws) === "host" && this.ctx.getWebSockets("host").every((h) => h === ws)) {
      for (const badge of this.ctx.getWebSockets("badge")) badge.send("mirror off\n");
    }
  }

  private role(ws: WebSocket): Role {
    return this.ctx.getTags(ws)[0] as Role;
  }
}
