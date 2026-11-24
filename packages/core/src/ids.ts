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

export function createId(random: RandomBytes = cryptoBytes): string {
  const bytes = random(new Uint8Array(ID_LENGTH));
  let id = "";
  for (const byte of bytes) id += ALPHABET[byte & 31];
  return id;
}
