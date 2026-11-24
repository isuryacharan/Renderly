// Worker entry point for audio decoding. Created by `createAudioOutput()`.

import type { Endpoint } from "@renderly/media";
import { serveAudioDecoder } from "./audio-decoder";

serveAudioDecoder(self as unknown as Endpoint);
