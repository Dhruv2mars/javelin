// The TypeScript codec against byte vectors the Rust codec produces, and its refusals.
import { expect, test } from "bun:test";
import {
  Kind,
  type Message,
  ProtocolError,
  decode,
  decodeChunk,
  encode,
  objectId,
  parseChangeset,
} from "../../../packages/protocol/src";

const hex = (s: string) => Uint8Array.from(s.match(/../g)!.map((b) => parseInt(b, 16)));
const id = (n: number) => n.toString(16).padStart(2, "0").repeat(32);

test("every read message round-trips", () => {
  const messages: Message[] = [
    { type: "resolve", version: null },
    { type: "resolve", version: 7 },
    { type: "resolved", number: 2 ** 40, id: id(1), tree: id(2) },
    { type: "get", items: [[Kind.Tree, id(3)], [Kind.Blob, id(4)]] },
    { type: "objects", objects: [{ kind: Kind.Blob, id: id(5), bytes: Uint8Array.of(1, 2, 3) }] },
    { type: "changeset", version: 3, offset: 10 },
    { type: "changesetPart", total: 3, bytes: new TextEncoder().encode("{}x") },
    { type: "attribution", version: 2 },
    { type: "attributed", number: 0, publication: null },
    { type: "attributed", number: 1, publication: { layer: "work", attribution: { kind: "human" } } },
    {
      type: "attributed",
      number: 2,
      publication: { layer: "lists", attribution: { kind: "agent", context: null, allowMissingTrace: true } },
    },
    {
      type: "attributed",
      number: 3,
      publication: { layer: "docs", attribution: { kind: "agent", context: id(9), allowMissingTrace: false } },
    },
    { type: "readDisclosure" },
    { type: "disclosure", delayed: true, delay: null, frontier: 4, embargoes: [] },
    {
      type: "disclosure",
      delayed: true,
      delay: 604800,
      frontier: 2,
      embargoes: [
        { version: 3, until: null },
        { version: 5, until: 1_900_000_000 },
      ],
    },
    { type: "error", code: "forbidden", detail: "a browser origin may only send read messages" },
  ];
  for (const message of messages) expect(decode(encode(message))).toEqual(message);
});

test("encodings match the Rust codec byte for byte", () => {
  // Resolve { version: None }: tag 0x40, a zero flag, eight zero bytes.
  expect(encode({ type: "resolve", version: null })).toEqual(hex("40000000000000000000"));
  // ReadDisclosure is its tag. An outsider's Disclosure: delayed, no delay, frontier 2, no
  // embargoes.
  expect(encode({ type: "readDisclosure" })).toEqual(hex("48"));
  expect(decode(hex(`4901000000000000000000${"00000000000000" + "02"}00000000`))).toEqual({
    type: "disclosure",
    delayed: true,
    delay: null,
    frontier: 2,
    embargoes: [],
  });
  // Attributed for World 2 from the agent Layer `second` with the missing-trace override,
  // as `javelin-node` sent it.
  const fromNode = hex(`45000000000000000201067365636f6e640100${"00".repeat(32)}01`);
  expect(decode(fromNode)).toEqual({
    type: "attributed",
    number: 2,
    publication: { layer: "second", attribution: { kind: "agent", context: null, allowMissingTrace: true } },
  });
  expect(encode(decode(fromNode) as Message)).toEqual(fromNode);
});

test("noncanonical and unknown bodies are malformed", () => {
  const refused = (bytes: Uint8Array) => {
    try {
      decode(bytes);
    } catch (error) {
      return (error as ProtocolError).code;
    }
    return "accepted";
  };
  expect(refused(Uint8Array.of(0x04))).toBe("malformed");
  // Resolve with a zero flag but a nonzero version.
  expect(refused(hex("40000000000000000001"))).toBe("malformed");
  // Absent publication with a Layer name.
  expect(refused(hex("45000000000000000300016161"))).toBe("malformed");
  // Trailing bytes.
  expect(refused(hex("4400000000000000020000"))).toBe("malformed");
  // A present optional ID must not be absent-shaped: flag 2 is invalid.
  expect(refused(hex(`450000000000000002010161010200${"00".repeat(32)}00`))).toBe("malformed");
  // Kind 2 (Manifest) is retired.
  expect(refused(hex(`1200000001020${"0".repeat(63)}`))).toBe("malformed");
  // Counts over the limit are refused before reading items.
  expect(refused(hex("1200001001"))).toBe("limit_exceeded");
});

test("tree chunks and changesets decode", async () => {
  const name = new TextEncoder().encode("a.txt");
  const chunk = Uint8Array.from([0, 1, 0, name.length, ...name, ...hex(id(7)), 4, 0, 3, 0x73, 0x72, 0x63, ...hex(id(8))]);
  expect(decodeChunk(chunk)).toEqual({
    level: 0,
    items: [
      { name: "a.txt", type: "file", entry: { kind: "regular", content: id(7), executable: false } },
      { name: "src", type: "dir", id: id(8) },
    ],
  });
  // Names must ascend.
  const swapped = Uint8Array.from([0, 4, 0, 3, 0x73, 0x72, 0x63, ...hex(id(8)), 1, 0, name.length, ...name, ...hex(id(7))]);
  expect(() => decodeChunk(swapped)).toThrow(ProtocolError);
  expect(await objectId(Kind.Tree, Uint8Array.of(0))).toHaveLength(64);
  const changes = parseChangeset(
    new TextEncoder().encode(`{"a":null,"b":{"kind":"regular","content":"${id(1)}","executable":true}}`),
  );
  expect([...changes]).toEqual([
    ["a", null],
    ["b", { kind: "regular", content: id(1), executable: true }],
  ]);
});
