// Worker entry point. Created by `createDecodeClient()`.

import { serveDecoder, type Endpoint } from "./decoder";

serveDecoder(self as unknown as Endpoint);
