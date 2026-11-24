// Dev only: serves files from test-media/ with HTTP Range support so benchmarks can run without
// a file picker. Users never go through this; they open files from disk.
//
//   GET /__test-media/list         JSON [{ name, size }]
//   GET /__test-media/file/<name>  the file (206 Partial Content when a Range header is sent)
//   POST /__test-media/upload/<name>  streams the request body to test-media/out/<name>, so an
//                                    exported file can be checked with ffprobe

import {
  createReadStream,
  createWriteStream,
  existsSync,
  mkdirSync,
  readdirSync,
  statSync,
} from "node:fs";
import { basename, join } from "node:path";
import type { Plugin } from "vite";

const VIDEO = /\.(mp4|mov|m4v|mkv|webm)$/i;

export function testMedia(dir: string): Plugin {
  return {
    name: "renderly-test-media",
    apply: "serve",
    configureServer(server) {
      server.middlewares.use("/__test-media", (req, res) => {
        const path = decodeURIComponent((req.url ?? "/").split("?")[0] ?? "/");

        if (req.method === "POST" && path.startsWith("/upload/")) {
          const name = basename(path.slice("/upload/".length));
          if (!VIDEO.test(name)) {
            res.statusCode = 400;
            res.end();
            return;
          }
          const outDir = join(dir, "out");
          mkdirSync(outDir, { recursive: true });
          const out = createWriteStream(join(outDir, name));
          req.pipe(out);
          out.on("finish", () => {
            res.setHeader("Content-Type", "application/json");
            res.end(JSON.stringify({ bytes: statSync(join(outDir, name)).size }));
          });
          out.on("error", () => {
            res.statusCode = 500;
            res.end();
          });
          return;
        }

        if (path === "/list") {
          const files = existsSync(dir)
            ? readdirSync(dir)
                .filter((name) => VIDEO.test(name))
                .map((name) => ({ name, size: statSync(join(dir, name)).size }))
            : [];
          res.setHeader("Content-Type", "application/json");
          res.end(JSON.stringify(files));
          return;
        }

        const name = basename(path.replace(/^\/file\//, ""));
        const file = join(dir, name);
        if (!path.startsWith("/file/") || !VIDEO.test(name) || !existsSync(file)) {
          res.statusCode = 404;
          res.end();
          return;
        }

        const size = statSync(file).size;
        res.setHeader("Accept-Ranges", "bytes");
        res.setHeader("Content-Type", "application/octet-stream");
        res.setHeader("Cache-Control", "no-store");

        let start = 0;
        let end = size - 1;
        const range = /bytes=(\d*)-(\d*)/.exec(req.headers.range ?? "");
        if (range) {
          const [, from = "", to = ""] = range;
          start = from ? Number(from) : Math.max(0, size - Number(to));
          end = from && to ? Math.min(Number(to), size - 1) : size - 1;
          if (start > end) {
            res.statusCode = 416;
            res.setHeader("Content-Range", `bytes */${size}`);
            res.end();
            return;
          }
          res.statusCode = 206;
          res.setHeader("Content-Range", `bytes ${start}-${end}/${size}`);
        }
        res.setHeader("Content-Length", String(end - start + 1));
        if (req.method === "HEAD") {
          res.end();
          return;
        }
        const stream = createReadStream(file, { start, end });
        req.on("close", () => stream.destroy());
        stream.pipe(res);
      });
    },
  };
}
