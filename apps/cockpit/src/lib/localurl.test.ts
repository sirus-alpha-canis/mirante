import { describe, expect, it } from "vitest";
import { checkLocalUrl } from "./localurl";

const ok = (input: string, self?: string) => {
  const r = checkLocalUrl(input, self);
  return r.ok ? r.url : r.why;
};

describe("checkLocalUrl (the Browser tab's address bar)", () => {
  it("takes a dev server on localhost or 127.0.0.1 on an explicit port, normalized", () => {
    expect(ok("http://localhost:3000")).toBe("http://localhost:3000/");
    expect(ok("http://127.0.0.1:5173/app?x=1#top")).toBe("http://127.0.0.1:5173/app?x=1#top");
    expect(ok("http://LOCALHOST:8080/")).toBe("http://localhost:8080/");
    // Typed without a scheme, or just a port.
    expect(ok("localhost:3000/docs")).toBe("http://localhost:3000/docs");
    expect(ok("127.0.0.1:4000")).toBe("http://127.0.0.1:4000/");
    expect(ok("3000")).toBe("http://localhost:3000/");
    expect(ok("  http://localhost:1  ")).toBe("http://localhost:1/");
    expect(ok("http://localhost:65535")).toBe("http://localhost:65535/");
  });

  it("refuses https and every other scheme (file:, javascript:, data:, ws:)", () => {
    expect(ok("https://localhost:3000")).toBe("scheme");
    expect(ok("file:///etc/passwd")).toBe("scheme");
    expect(ok("ws://localhost:3000")).toBe("scheme");
    expect(ok("data:text/html,<b>x</b>")).toBe("scheme");
    expect(ok("javascript:alert(1)")).toBe("scheme");
    expect(ok("about:blank")).toBe("scheme");
  });

  it("refuses other hosts, the LAN, IPv6 and look-alikes", () => {
    expect(ok("http://example.com:3000")).toBe("host");
    expect(ok("http://192.168.1.2:3000")).toBe("host");
    expect(ok("http://0.0.0.0:3000")).toBe("host");
    expect(ok("http://[::1]:3000")).toBe("host");
    expect(ok("http://localhost.:3000")).toBe("host");
    expect(ok("http://localhost.evil.example:3000")).toBe("host");
    expect(ok("http://127.0.0.2:3000")).toBe("host");
  });

  it("refuses userinfo (user:pass@, or a host hidden behind @)", () => {
    expect(ok("http://user:pw@localhost:3000")).toBe("userinfo");
    expect(ok("http://localhost:3000@evil.example:80")).toBe("userinfo");
    expect(ok("http://x@localhost:3000")).toBe("userinfo");
  });

  it("refuses a missing or out-of-range port (80 is dropped by URL, so it counts as missing)", () => {
    expect(ok("http://localhost")).toBe("port");
    expect(ok("http://localhost/")).toBe("port");
    expect(ok("http://localhost:80")).toBe("port");
    expect(ok("http://localhost:0")).toBe("port");
    expect(ok("http://localhost:65536")).toBe("parse");
    expect(ok("")).toBe("parse");
    expect(ok("http://")).toBe("parse");
  });

  it("refuses the app's own origin (the cockpit's dev server in a dev build)", () => {
    expect(ok("http://127.0.0.1:1420/", "http://127.0.0.1:1420")).toBe("self");
    expect(ok("http://localhost:1420/", "http://127.0.0.1:1420")).toBe("http://localhost:1420/");
    expect(ok("http://127.0.0.1:1420/", "tauri://localhost")).toBe("http://127.0.0.1:1420/");
  });

  it("refuses anything very long", () => {
    expect(ok(`http://localhost:3000/${"a".repeat(5000)}`)).toBe("parse");
  });
});
