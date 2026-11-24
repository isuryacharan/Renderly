// Worker entry point for exports. Created by `createExporter()`.

import type { Endpoint } from "@renderly/media";
import { serveExporter } from "./exporter";

serveExporter(self as unknown as Endpoint);
