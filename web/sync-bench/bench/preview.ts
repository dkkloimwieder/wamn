import { fileURLToPath } from "node:url";
import { preview } from "vite";

export function preparedPreview() {
  const outDir = process.env.WAMN_SYNC_BENCH_DIST;
  if (!outDir) throw new Error("WAMN_SYNC_BENCH_DIST is required");
  return preview({
    root: fileURLToPath(new URL("..", import.meta.url)),
    build: { outDir },
    preview: { port: 0, host: "127.0.0.1" },
  });
}
