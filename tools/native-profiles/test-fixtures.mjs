import { createHash } from 'node:crypto';

export const fakeID = label => createHash('sha256').update(`SYNTHETIC TEST ONLY ${label}`).digest('hex');
const empty = fakeID('empty');
const pools = new Map();

// These IDs are arbitrary labels, not native hashes. This builder tests recorded
// consistency only; it supplies no accepted native or performance evidence.
export function fixture(name, id) {
  const entries = name === 'entries8' ? 8 : name === 'entries128' ? 128 : 32;
  if (!pools.has(entries)) {
    const blobs = Array.from({ length: entries }, (_, i) => ({ id: fakeID(`blob-${entries}-${i}`),
      length: 262144 / entries, sha256: fakeID(`payload-${entries}-${i}`) }));
    const manifests = Array.from({ length: 128 }, (_, index) => {
      const shared = index < 64;
      const variant = index % 64;
      const canonical = Object.fromEntries(blobs.map((blob, i) => [`f${String(i).padStart(3, '0')}`,
        { kind: 'regular', content: blobs[shared ? 0 : i].id, executable: i < 6 && (variant & (1 << i)) !== 0 }]));
      return { id: fakeID(`manifest-${entries}-${index}`), shared, variant, canonical,
        length: Buffer.byteLength(JSON.stringify(canonical)) };
    });
    pools.set(entries, { blobs, manifests });
  }
  const { blobs, manifests } = pools.get(entries);
  const f = { case: name, depth: name === 'depth4' ? 4 : name === 'depth64' ? 64 : 16, entries,
    shared: name !== 'distinct-blobs', distinct_manifests: name === 'distinct-manifests',
    pool: blobs.map(b => b.id), shared_manifests: manifests.slice(0, 64).map(m => m.id),
    distinct_blob_manifests: manifests.slice(64).map(m => m.id), source_checkpoint: fakeID(id) };
  return { type: 'fixture', fixture: f, blobs, manifests, history: history(f), check: check(f),
    payload_bytes: 262144, verified_blob_reads: entries, verified_manifest_reads: 128 };
}
export function at(f, n) {
  return (f.shared ? f.shared_manifests : f.distinct_blob_manifests)[f.distinct_manifests ? n % f.depth : 0];
}
export function history(f) {
  return [{ number: 0, manifest: empty, operation: null }, ...Array.from({ length: f.depth }, (_, i) =>
    ({ number: i + 1, manifest: at(f, i + 1), operation: `seed-${i + 1}` }))];
}
export function check(f, versions = f.depth + 1, attempts = f.depth, layers = 1) {
  return { versions, objects: f.entries + 130, attempts, layers };
}
export function timing(phase, wall_ns = 100, cpu_ns = 10) {
  return { type: 'timing', phase, wall_ns, cpu_ns, peak_rss_bytes: 1024, error: null };
}
export function prepared(event) {
  const f = event.fixture;
  return [timing('init'), ...f.pool.map(() => timing('put_blob')),
    ...Array.from({ length: 128 }, () => timing('put_manifest')),
    ...Array.from({ length: f.depth }, () => timing('seed_advance')),
    timing('layer_base'), timing('create_layer'), event];
}
