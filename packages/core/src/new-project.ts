// A new, empty project: one sequence, no tracks. The home screen's "New project".

import { createId, type RandomBytes } from "./ids";
import { CURRENT_VERSION, projectSchema, type Project } from "./schema";
import type { Rate } from "./time";

export interface NewProjectOptions {
  width?: number;
  height?: number;
  fps?: Rate;
  random?: RandomBytes;
}

export function newProject(name: string, options: NewProjectOptions = {}): Project {
  const id = () => (options.random ? createId(options.random) : createId());
  return projectSchema().parse({
    format: "renderly",
    version: CURRENT_VERSION,
    id: id(),
    name,
    revision: 0,
    sequences: [
      {
        id: id(),
        name: "Main",
        width: options.width ?? 1920,
        height: options.height ?? 1080,
        fps: options.fps ?? [30, 1],
      },
    ],
  }) as Project;
}
