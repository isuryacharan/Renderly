// Worker entry point. The worker waits for one "connect" message carrying a MessagePort and
// serves decode requests on that port. The port can be handed to any thread, so the render
// worker receives frames directly from the decoder, never through the main thread.

import { serveDecoder } from "./decoder";

self.onmessage = (event: MessageEvent<{ type: "connect" }>) => {
  const port = event.ports[0];
  if (event.data.type !== "connect" || !port) return;
  self.onmessage = null;
  serveDecoder(port);
};
