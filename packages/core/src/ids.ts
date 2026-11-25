// Ids for everything in a project: short, URL-safe, random. 16 base-32 characters is 80 bits,
// enough that ids made on different machines never collide in practice.

const ALPHABET = "0123456789abcdefghjkmnpqrstvwxyz"; // Crockford base 32, lower case
export const ID_LENGTH = 16;
export const ID_PATTERN = /^[0-9a-hjkmnp-tv-z]{16}$/;

/** Fills a byte array with random values. Injected in tests for repeatable ids. */
export type RandomBytes = (bytes: Uint8Array<ArrayBuffer>) => Uint8Array<ArrayBuffer>;

const cryptoBytes: RandomBytes = (bytes) => {
  crypto.getRandomValues(bytes);
  return bytes;
};

/**
 * An id derived from a string, for things a command creates without being told their id (the
 * right half of a split). The same seed always gives the same id, so commands stay deterministic.
 * Two FNV-1a 64-bit hashes give the 80 bits.
 */
export function deriveId(seed: string): string {
  const fnv = (text: string) => {
    let hash = 0xcbf29ce484222325n;
    for (let i = 0; i < text.length; i++) {
      hash ^= BigInt(text.charCodeAt(i));
      hash = (hash * 0x100000001b3n) & 0xffffffffffffffffn;
    }
    return hash;
  };
  const bits = (fnv(seed) << 16n) ^ fnv(`${seed}#`);
  let id = "";
  for (let i = 0; i < ID_LENGTH; i++) id += ALPHABET[Number((bits >> BigInt(5 * i)) & 31n)];
  return id;
}

export function createId(random: RandomBytes = cryptoBytes): string {
  const bytes = random(new Uint8Array(ID_LENGTH));
  let id = "";
  for (const byte of bytes) id += ALPHABET[byte & 31];
  return id;
}
