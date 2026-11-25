// Media commands: adding probed files to the project and removing them. The file itself is never
// touched: a linked file stays where it is, and a copied one stays in the project folder until
// the cache is cleaned.

import { z } from "zod";
import { CommandError, defineCommand } from "./commands";
import { Id, Media } from "./schema";
import { deleteClips } from "./timeline-commands";

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

/** Adds media items. Ids are made by the importer, so a replay adds the same items. */
export const ImportMedia = defineCommand({
  type: "media.import",
  payload: z.strictObject({ items: z.array(Media).min(1) }),
  label: ({ items }) => `Import ${plural(items.length, "file")}`,
  apply: (project, { items }) => {
    const ids = new Set(project.media.map((m) => m.id));
    items.forEach((item, i) => {
      if (ids.has(item.id))
        throw new CommandError(
          "rejected",
          `Media ${item.id} is already in the project`,
          `items[${i}].id`,
        );
      ids.add(item.id);
      project.media.push(item);
    });
  },
});

/**
 * Removes media items. Clips that use them are removed too when `withClips` is set; otherwise
 * the command is refused, so nothing on the timeline disappears by surprise.
 */
export const RemoveMedia = defineCommand({
  type: "media.remove",
  payload: z.strictObject({
    ids: z
      .array(Id)
      .min(1)
      .refine((ids) => new Set(ids).size === ids.length, "lists an id twice"),
    withClips: z.boolean().default(false),
  }),
  label: ({ ids }) => `Remove ${plural(ids.length, "file")}`,
  apply: (project, { ids, withClips }) => {
    const removing = new Set(ids);
    for (const id of ids) {
      if (!project.media.some((m) => m.id === id))
        throw new CommandError("not-found", `No media ${id}`, "ids");
    }
    // The clips using the media, sequence by sequence.
    const users = project.sequences.map((s) =>
      s.tracks.flatMap((t) =>
        t.clips.filter((c) => c.media !== undefined && removing.has(c.media)).map((c) => c.id),
      ),
    );
    const count = users.reduce((n, list) => n + list.length, 0);
    if (count > 0 && !withClips)
      throw new CommandError("rejected", `The media is used by ${plural(count, "clip")}`, "ids");
    // Each sequence's magnetic tracks close up after its clips go.
    for (const list of users) if (list.length > 0) deleteClips(project, list);
    project.media = project.media.filter((m) => !removing.has(m.id));
  },
});
