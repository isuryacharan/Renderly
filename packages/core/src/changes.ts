// What a change did, as entity ids: added, removed and changed. Computed by comparing the project
// before and after. Immer shares every object it did not touch, so an entity whose own fields are
// identical by reference is unchanged, and a clip edit does not mark its track and sequence.

export interface Changes {
  added: string[];
  removed: string[];
  changed: string[];
}

type Entity = Record<string, unknown> & { id: string };

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);
const isEntity = (value: unknown): value is Entity =>
  isObject(value) && typeof value["id"] === "string";
const isEntityList = (value: unknown): value is Entity[] =>
  Array.isArray(value) && value.length > 0 && value.every(isEntity);

/** Every object with an `id`, anywhere in the document, by id. */
function entities(document: unknown, into = new Map<string, Entity>()): Map<string, Entity> {
  if (Array.isArray(document)) {
    for (const item of document) entities(item, into);
  } else if (isObject(document)) {
    if (isEntity(document)) into.set(document.id, document);
    for (const value of Object.values(document)) {
      if (typeof value === "object" && value !== null) entities(value, into);
    }
  }
  return into;
}

/** True when an entity's own fields differ. Child entities count only by their ids and order. */
function ownFieldsDiffer(before: Entity, after: Entity): boolean {
  if (before === after) return false;
  const keys = new Set([...Object.keys(before), ...Object.keys(after)]);
  for (const key of keys) {
    const a = before[key];
    const b = after[key];
    if (a === b) continue;
    if (
      (isEntityList(a) || (Array.isArray(a) && a.length === 0)) &&
      (isEntityList(b) || (Array.isArray(b) && b.length === 0))
    ) {
      const ids = (list: Entity[]) => list.map((e) => e.id).join("\u0000");
      if (ids(a as Entity[]) !== ids(b as Entity[])) return true;
      continue;
    }
    return true; // a scalar or a plain object (components, fingerprint) that is not shared
  }
  return false;
}

export function diffEntities(before: unknown, after: unknown): Changes {
  const old = entities(before);
  const now = entities(after);
  const changes: Changes = { added: [], removed: [], changed: [] };
  for (const [id, entity] of now) {
    const previous = old.get(id);
    if (!previous) changes.added.push(id);
    else if (ownFieldsDiffer(previous, entity)) changes.changed.push(id);
  }
  for (const id of old.keys()) if (!now.has(id)) changes.removed.push(id);
  return changes;
}
