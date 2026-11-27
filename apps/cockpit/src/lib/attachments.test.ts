import { describe, expect, it } from "vitest";
import { DaemonError } from "./daemon";
import { RpcCode } from "./protocol";
import { guessKind, isStaleAttachment, pastedName, pathPaste, previewOf, shellQuote, sizeText, toBase64 } from "./attachments";

describe("attachments", () => {
  it("base64 is the standard alphabet, large inputs included", () => {
    expect(toBase64(new TextEncoder().encode("hi?>"))).toBe("aGk/Pg==");
    const big = new Uint8Array(100_000).fill(0xfb);
    expect(atob(toBase64(big)).length).toBe(100_000);
  });

  it("a path for a shell: single-quoted, a quote inside closed and escaped", () => {
    expect(shellQuote("/tmp/a b.png")).toBe("'/tmp/a b.png'");
    expect(shellQuote("/tmp/it's.png")).toBe("'/tmp/it'\\''s.png'");
    expect(shellQuote("/tmp/$(x)`y`;z")).toBe("'/tmp/$(x)`y`;z'");
  });

  it("a terminal paste of paths: quoted words, control characters gone, markers only in bracketed mode", () => {
    expect(pathPaste(["/tmp/a b.png", "/tmp/it's.har"], false)).toBe("'/tmp/a b.png' '/tmp/it'\\''s.har'");
    expect(pathPaste(["/tmp/a.png"], true)).toBe("\x1b[200~'/tmp/a.png'\x1b[201~");
    // A name cannot close the paste early or press Enter.
    expect(pathPaste(["/tmp/x\x1b[201~\r\nrm.png\x9b"], true)).toBe("\x1b[200~'/tmp/x[201~rm.png'\x1b[201~");
  });

  it("sizes read like a person writes them", () => {
    expect(sizeText(512)).toBe("512 B");
    expect(sizeText(2048)).toBe("2 KB");
    expect(sizeText(1_572_864)).toBe("1.5 MB");
  });

  it("a thumbnail only for a raster image (never SVG), as a data: URL", () => {
    expect(previewOf("image/png", "AAAA")).toBe("data:image/png;base64,AAAA");
    expect(previewOf("image/webp", "AAAA")).toBe("data:image/webp;base64,AAAA");
    expect(previewOf("image/svg+xml", "AAAA")).toBeNull();
    expect(previewOf("text/plain", "AAAA")).toBeNull();
  });

  it("guesses a dropped file's kind by its extension (the daemon decides by its bytes)", () => {
    expect(guessKind("/a/Shot 1.PNG")).toBe("image");
    expect(guessKind("/a/x.jpeg")).toBe("image");
    expect(guessKind("/a/trace.har")).toBe("file");
  });

  it("a pasted blob without a name gets one", () => {
    expect(pastedName("", "image/png")).toBe("pasted-image.png");
    expect(pastedName("", "application/octet-stream")).toBe("pasted-file");
    expect(pastedName("shot.png", "image/png")).toBe("shot.png");
  });

  it("an id the daemon no longer knows (it restarted) is told apart from other refusals", () => {
    const stale = new DaemonError({ kind: "rpc", code: RpcCode.INVALID_PARAMS, message: "unknown attachment 01: attach the file again" });
    const other = new DaemonError({ kind: "rpc", code: RpcCode.INVALID_PARAMS, message: "message too long" });
    expect(isStaleAttachment(stale)).toBe(true);
    expect(isStaleAttachment(other)).toBe(false);
    expect(isStaleAttachment(new Error("unknown attachment"))).toBe(false);
  });
});
