// Worker entry point. Created by `createPreview()`.

import { serveRenderer, type RendererScope } from "./renderer";

serveRenderer(self as unknown as RendererScope);
