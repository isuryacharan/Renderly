import { describe, expect, it } from "vitest";
import { avcLengthSize, isNonReferenceAvc } from "./nal";

/** Builds a length-prefixed access unit from NAL units given as [header, ...payload]. */
function accessUnit(lengthSize: number, ...nals: number[][]): Uint8Array {
  const bytes: number[] = [];
  for (const nal of nals) {
    const length = nal.length;
    for (let i = lengthSize - 1; i >= 0; i--) bytes.push((length >> (8 * i)) & 0xff);
    bytes.push(...nal);
  }
  return new Uint8Array(bytes);
}

const AUD = [0x09, 0xf0];
const SEI = [0x06, 0x05, 0x01];
const IDR = [0x65, 0x88, 0x84]; // nal_ref_idc 3, type 5
const P_REF = [0x41, 0x9a, 0x00]; // nal_ref_idc 2, type 1
const B_NONREF = [0x01, 0x9e, 0x00]; // nal_ref_idc 0, type 1

describe("isNonReferenceAvc", () => {
  it("skips a non-reference slice after an AUD and SEI", () => {
    expect(isNonReferenceAvc(accessUnit(4, AUD, SEI, B_NONREF), 4)).toBe(true);
  });

  it("keeps reference slices and IDR pictures", () => {
    expect(isNonReferenceAvc(accessUnit(4, AUD, P_REF), 4)).toBe(false);
    expect(isNonReferenceAvc(accessUnit(4, IDR), 4)).toBe(false);
  });

  it("works with 1- and 2-byte length fields", () => {
    expect(isNonReferenceAvc(accessUnit(1, B_NONREF), 1)).toBe(true);
    expect(isNonReferenceAvc(accessUnit(2, SEI, B_NONREF), 2)).toBe(true);
  });

  it("keeps the frame when it cannot find a slice", () => {
    expect(isNonReferenceAvc(accessUnit(4, AUD, SEI), 4)).toBe(false);
    expect(isNonReferenceAvc(new Uint8Array([0, 0, 0, 0, 0]), 4)).toBe(false);
    expect(isNonReferenceAvc(new Uint8Array(), 4)).toBe(false);
  });
});

describe("avcLengthSize", () => {
  it("reads lengthSizeMinusOne from an avcC record", () => {
    expect(avcLengthSize(new Uint8Array([1, 0x4d, 0x40, 0x34, 0xff]))).toBe(4);
    expect(avcLengthSize(new Uint8Array([1, 0x4d, 0x40, 0x34, 0xfc]))).toBe(1);
  });

  it("rejects something that is not avcC", () => {
    expect(avcLengthSize(new Uint8Array([0, 0, 0, 1, 0x67]))).toBeNull();
  });
});
