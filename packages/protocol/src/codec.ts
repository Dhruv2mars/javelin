// Protocol version 2 in TypeScript: the read messages a browser or Worker sends to a node.
// Mirrors `crates/javelin-protocol/src/codec.rs` byte for byte. Over HTTP a message is one
// body (tag first, no length prefix), so this module has no framing. Integers are
// big-endian; IDs travel as raw 32 bytes and are lowercase hex here.

export const VERSION = 2;

export const limits = {
  MAX_FRAME: 16 << 20,
  MAX_BLOB: 8 << 20,
  MAX_OBJECTS: 1024,
  MAX_WANT: 4096,
  MAX_CHANGESET_PART: 8 << 20,
  MAX_DETAIL: 1024,
} as const;

const tag = {
  GET: 0x12,
  OBJECTS: 0x20,
  RESOLVE: 0x40,
  RESOLVED: 0x41,
  CHANGESET: 0x42,
  CHANGESET_PART: 0x43,
  ATTRIBUTION: 0x44,
  ATTRIBUTED: 0x45,
  ERROR: 0x7f,
} as const;

/** Object kinds. Kind 2 (Manifest) is retired. */
export enum Kind {
  Blob = 1,
  Checkpoint = 3,
  Tree = 4,
}

/** 64 lowercase hex digits. */
export type Hex = string;

export type Attribution =
  | { kind: "human" }
  | { kind: "agent"; context: Hex | null; allowMissingTrace: boolean };

export type Publication = { layer: string; attribution: Attribution };

export type Message =
  | { type: "get"; items: [Kind, Hex][] }
  | { type: "objects"; objects: { kind: Kind; id: Hex; bytes: Uint8Array }[] }
  | { type: "resolve"; version: number | null }
  | { type: "resolved"; number: number; id: Hex; tree: Hex }
  | { type: "changeset"; version: number; offset: number }
  | { type: "changesetPart"; total: number; bytes: Uint8Array }
  | { type: "attribution"; version: number }
  | { type: "attributed"; number: number; publication: Publication | null }
  | { type: "error"; code: string; detail: string };

/** A refusal with a stable code, from the node or from decoding its reply. */
export class ProtocolError extends Error {
  constructor(
    readonly code: string,
    readonly detail: string,
  ) {
    super(`${code}: ${detail}`);
    this.name = "ProtocolError";
  }
}

const malformed = (detail: string) => new ProtocolError("malformed", detail);
const exceeded = (detail: string) => new ProtocolError("limit_exceeded", detail);

const ABSENT: Hex = "0".repeat(64);
const HEX = /^[0-9a-f]{64}$/;
const LAYER = /^[A-Za-z0-9._-]{1,128}$/;
const utf8 = new TextEncoder();
const strict = new TextDecoder("utf-8", { fatal: true });

export function toHex(bytes: Uint8Array): Hex {
  let out = "";
  for (const byte of bytes) out += byte.toString(16).padStart(2, "0");
  return out;
}

export function fromHex(hex: Hex): Uint8Array {
  if (!HEX.test(hex)) throw new ProtocolError("invalid_input", "ID must be 64 lowercase hex digits");
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

class Writer {
  private parts: number[] = [];
  u8(n: number) {
    this.parts.push(n & 0xff);
  }
  u32(n: number) {
    for (let shift = 24; shift >= 0; shift -= 8) this.u8(n >>> shift);
  }
  u64(n: number) {
    if (!Number.isSafeInteger(n) || n < 0) throw new ProtocolError("invalid_input", "number out of range");
    this.u32(Math.floor(n / 2 ** 32));
    this.u32(n >>> 0);
  }
  bytes(b: Uint8Array) {
    for (const byte of b) this.parts.push(byte);
  }
  id(hex: Hex) {
    this.bytes(fromHex(hex));
  }
  short(text: string) {
    const b = utf8.encode(text);
    if (b.length > 255) throw new ProtocolError("malformed", "text exceeds 255 bytes");
    this.u8(b.length);
    this.bytes(b);
  }
  done(): Uint8Array<ArrayBuffer> {
    return Uint8Array.from(this.parts);
  }
}

class Reader {
  private at = 0;
  constructor(private body: Uint8Array) {}
  take(n: number): Uint8Array {
    if (n > this.body.length - this.at) throw malformed("message ends inside a field");
    const out = this.body.subarray(this.at, this.at + n);
    this.at += n;
    return out;
  }
  u8(): number {
    return this.take(1)[0];
  }
  u16(): number {
    const b = this.take(2);
    return (b[0] << 8) | b[1];
  }
  u32(): number {
    const b = this.take(4);
    return ((b[0] << 24) >>> 0) + (b[1] << 16) + (b[2] << 8) + b[3];
  }
  u64(): number {
    const high = this.u32();
    const low = this.u32();
    const n = high * 2 ** 32 + low;
    if (!Number.isSafeInteger(n)) throw exceeded("number exceeds 2^53");
    return n;
  }
  id(): Hex {
    return toHex(this.take(32));
  }
  kind(): Kind {
    const k = this.u8();
    if (k !== Kind.Blob && k !== Kind.Checkpoint && k !== Kind.Tree) throw malformed("unknown object kind");
    return k;
  }
  flag(): boolean {
    const f = this.u8();
    if (f > 1) throw malformed("invalid flag");
    return f === 1;
  }
  count(max: number): number {
    const n = this.u32();
    if (n > max) throw exceeded("count exceeds limit");
    return n;
  }
  text(len: number): string {
    try {
      return strict.decode(this.take(len));
    } catch (error) {
      if (error instanceof ProtocolError) throw error;
      throw malformed("text is not UTF-8");
    }
  }
  short(): string {
    return this.text(this.u8());
  }
  optionalId(): Hex | null {
    const present = this.flag();
    const id = this.id();
    if (present) return id;
    if (id !== ABSENT) throw malformed("absent ID must be zeroed");
    return null;
  }
  attribution(): Attribution {
    switch (this.u8()) {
      case 0:
        return { kind: "human" };
      case 1:
        return { kind: "agent", context: this.optionalId(), allowMissingTrace: this.flag() };
      default:
        throw malformed("unknown attribution");
    }
  }
  end() {
    if (this.at !== this.body.length) throw malformed("trailing bytes after message");
  }
}

/** Encodes one message body. Counts and sizes are checked against the version 2 limits. */
export function encode(message: Message): Uint8Array<ArrayBuffer> {
  const w = new Writer();
  switch (message.type) {
    case "get":
      if (message.items.length > limits.MAX_WANT) throw exceeded("count exceeds limit");
      w.u8(tag.GET);
      w.u32(message.items.length);
      for (const [kind, id] of message.items) {
        w.u8(kind);
        w.id(id);
      }
      break;
    case "objects":
      if (message.objects.length > limits.MAX_OBJECTS) throw exceeded("count exceeds limit");
      w.u8(tag.OBJECTS);
      w.u32(message.objects.length);
      for (const { kind, id, bytes } of message.objects) {
        w.u8(kind);
        w.id(id);
        w.u32(bytes.length);
        w.bytes(bytes);
      }
      break;
    case "resolve":
      w.u8(tag.RESOLVE);
      w.u8(message.version === null ? 0 : 1);
      w.u64(message.version ?? 0);
      break;
    case "resolved":
      w.u8(tag.RESOLVED);
      w.u64(message.number);
      w.id(message.id);
      w.id(message.tree);
      break;
    case "changeset":
      w.u8(tag.CHANGESET);
      w.u64(message.version);
      w.u64(message.offset);
      break;
    case "changesetPart":
      if (message.bytes.length > limits.MAX_CHANGESET_PART) throw exceeded("changeset part exceeds limit");
      w.u8(tag.CHANGESET_PART);
      w.u64(message.total);
      w.u32(message.bytes.length);
      w.bytes(message.bytes);
      break;
    case "attribution":
      w.u8(tag.ATTRIBUTION);
      w.u64(message.version);
      break;
    case "attributed": {
      w.u8(tag.ATTRIBUTED);
      w.u64(message.number);
      const p = message.publication;
      w.u8(p ? 1 : 0);
      w.short(p ? p.layer : "");
      if (p?.attribution.kind === "human") w.u8(0);
      if (p?.attribution.kind === "agent") {
        w.u8(1);
        w.u8(p.attribution.context ? 1 : 0);
        w.id(p.attribution.context ?? ABSENT);
        w.u8(p.attribution.allowMissingTrace ? 1 : 0);
      }
      break;
    }
    case "error": {
      w.u8(tag.ERROR);
      w.short(message.code);
      let detail = utf8.encode(message.detail);
      if (detail.length > limits.MAX_DETAIL) detail = detail.subarray(0, limits.MAX_DETAIL);
      w.u8(detail.length >> 8);
      w.u8(detail.length);
      w.bytes(detail);
      break;
    }
  }
  const body = w.done();
  if (body.length > limits.MAX_FRAME) throw exceeded("frame exceeds size limit");
  return body;
}

/** Decodes one message body. Unknown tags and noncanonical forms are malformed. */
export function decode(body: Uint8Array): Message {
  if (body.length > limits.MAX_FRAME) throw exceeded("frame exceeds size limit");
  const r = new Reader(body);
  let message: Message;
  switch (r.u8()) {
    case tag.GET: {
      const n = r.count(limits.MAX_WANT);
      const items: [Kind, Hex][] = [];
      for (let i = 0; i < n; i++) items.push([r.kind(), r.id()]);
      message = { type: "get", items };
      break;
    }
    case tag.OBJECTS: {
      const n = r.count(limits.MAX_OBJECTS);
      const objects = [];
      for (let i = 0; i < n; i++) {
        const kind = r.kind();
        const id = r.id();
        const len = r.u32();
        if (len > limits.MAX_BLOB) throw exceeded("object exceeds size limit");
        objects.push({ kind, id, bytes: r.take(len).slice() });
      }
      message = { type: "objects", objects };
      break;
    }
    case tag.RESOLVE: {
      const present = r.u8();
      const version = r.u64();
      if (present > 1 || (present === 0 && version !== 0)) throw malformed("invalid version selector");
      message = { type: "resolve", version: present ? version : null };
      break;
    }
    case tag.RESOLVED:
      message = { type: "resolved", number: r.u64(), id: r.id(), tree: r.id() };
      break;
    case tag.CHANGESET:
      message = { type: "changeset", version: r.u64(), offset: r.u64() };
      break;
    case tag.CHANGESET_PART: {
      const total = r.u64();
      const len = r.u32();
      if (len > limits.MAX_CHANGESET_PART) throw exceeded("changeset part exceeds limit");
      message = { type: "changesetPart", total, bytes: r.take(len).slice() };
      break;
    }
    case tag.ATTRIBUTION:
      message = { type: "attribution", version: r.u64() };
      break;
    case tag.ATTRIBUTED: {
      const number = r.u64();
      let publication: Publication | null = null;
      if (r.flag()) {
        const layer = r.short();
        if (!LAYER.test(layer)) throw malformed("invalid Layer ID");
        publication = { layer, attribution: r.attribution() };
      } else if (r.short() !== "") {
        throw malformed("absent publication must be empty");
      }
      message = { type: "attributed", number, publication };
      break;
    }
    case tag.ERROR: {
      const code = r.short();
      const len = r.u16();
      if (len > limits.MAX_DETAIL) throw exceeded("error detail exceeds limit");
      message = { type: "error", code, detail: r.text(len) };
      break;
    }
    default:
      throw malformed("unknown or unsupported message tag");
  }
  r.end();
  return message;
}
