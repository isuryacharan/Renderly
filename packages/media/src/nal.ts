// Just enough H.264 bitstream reading to tell whether a coded frame is used as a reference.
//
// An exact seek must decode from the keyframe up to the target. Frames that no other frame
// references (in H.264, NAL units with nal_ref_idc = 0, usually B-frames) can be skipped when
// they come before the target: nothing decoded later depends on them. With two B-frames between
// references that removes about two thirds of the decode work.

/** Reads the NAL length-field size from an avcC record (ISO/IEC 14496-15, 5.3.3.1). */
export function avcLengthSize(avcC: Uint8Array): number | null {
  const byte = avcC[4];
  if (avcC[0] !== 1 || byte === undefined) return null;
  return (byte & 0b11) + 1;
}

/**
 * True when the first coded slice in a length-prefixed H.264 access unit has nal_ref_idc = 0.
 * Returns false when unsure (no slice found, malformed data), so the frame is decoded.
 */
export function isNonReferenceAvc(data: Uint8Array, lengthSize: number): boolean {
  let offset = 0;
  while (offset + lengthSize < data.length) {
    let length = 0;
    for (let i = 0; i < lengthSize; i++) length = length * 256 + (data[offset + i] ?? 0);
    const header = data[offset + lengthSize] ?? 0;
    const type = header & 0x1f;
    // Types 1-5 are coded slices; the first one decides for the whole picture.
    if (type >= 1 && type <= 5) return ((header >> 5) & 0b11) === 0;
    if (length === 0) return false;
    offset += lengthSize + length;
  }
  return false;
}
