// The local edge serves built bytes and forwards requests to the shipped services.
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createReadStream, readFileSync, writeFileSync, existsSync, unlinkSync, realpathSync } from "node:fs";
import http from "node:http";
import https from "node:https";
import net from "node:net";
import { join, extname, sep } from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const config = JSON.parse(readFileSync(process.argv[2], "utf8"));
const servers = [];
let host;
let stopping;
const socket = join(config.root, "web.sock");
const control = net.createServer((stream) => {
  stream.once("data", async (data) => {
    if (data.toString() !== "stop\n") return stream.end("unknown command\n");
    try {
      await stop();
      stream.end("stopped\n");
    } catch (error) {
      stream.end(`${error}\n`);
    }
  });
});

async function stop() {
  if (stopping) return stopping;
  stopping = (async () => {
    for (const server of servers) {
      server.closeAllConnections();
      await new Promise((resolve) => server.close(resolve));
    }
    if (host && host.exitCode === null && host.signalCode === null) {
      const exited = once(host, "exit");
      host.kill("SIGTERM");
      const timer = setTimeout(() => host.kill("SIGKILL"), 70000);
      try {
        await exited;
      } finally {
        clearTimeout(timer);
      }
    }
    control.close();
    if (existsSync(socket)) unlinkSync(socket);
  })();
  return stopping;
}

for (const signal of ["SIGTERM", "SIGINT"]) process.on(signal, () => void stop());

async function listen(server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return `http://127.0.0.1:${server.address().port}`;
}

function proxy(request, response, target, path, headers = {}) {
  const url = new URL(path, target);
  const transport = url.protocol === "https:" ? https : http;
  const upstream = transport.request(
    url,
    {
      method: request.method,
      headers: { ...request.headers, host: url.host, ...headers },
      ca: readFileSync(config.ca),
    },
    (incoming) => {
      response.writeHead(incoming.statusCode, incoming.headers);
      incoming.pipe(response);
    },
  );
  upstream.on("error", (error) => {
    console.error(error.message);
    if (!response.headersSent) response.writeHead(502);
    response.end("The local service is unavailable.");
  });
  request.pipe(upstream);
}

const types = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".json": "application/json",
  ".woff2": "font/woff2",
};
function serve(dist, application, controlUrl) {
  return http.createServer((request, response) => {
    try {
      const url = new URL(request.url, "http://localhost");
      if (url.pathname.startsWith("/password/")) {
        return proxy(request, response, config.issuer, request.url);
      }
      if (url.pathname.startsWith("/api/")) {
        const audience = new URL(request.headers.referer ?? "http://localhost").pathname.split("/")[1];
        if (application || audience?.startsWith("urn:wamn:project-env:")) {
          const endpoint = JSON.parse(readFileSync(join(config.root, "application-endpoint.json"), "utf8"));
          return proxy(request, response, endpoint.base_url, request.url.slice(4), { host: endpoint.route_host });
        }
        return proxy(request, response, controlUrl, request.url.slice(4), { host: config.route_host });
      }
      if (url.pathname === "/config.json") {
        response.writeHead(200, { "content-type": "application/json", "cache-control": "no-cache" });
        return response.end(
          JSON.stringify(application ? { org: config.org, project: config.project } : { org: config.org }),
        );
      }
      const root = realpathSync(dist);
      const relative = decodeURIComponent(url.pathname).replace(/^\/+/, "");
      let path = join(root, relative);
      if (!existsSync(path) || !extname(path)) path = join(root, "index.html");
      path = realpathSync(path);
      if (!path.startsWith(root + sep)) {
        response.writeHead(404);
        return response.end();
      }
      response.writeHead(200, {
        "content-type": types[extname(path)] ?? "application/octet-stream",
        "cache-control": "no-cache",
      });
      createReadStream(path)
        .on("error", () => response.destroy())
        .pipe(response);
    } catch (error) {
      response.writeHead(503, { "content-type": "text/plain" });
      response.end(`The local application is not ready: ${error.message}`);
    }
  });
}

try {
  control.listen(socket);
  await once(control, "listening");
  // Reserve an ephemeral port before launching the same host binary as GCP.
  const probe = net.createServer();
  await listen(probe);
  const address = `127.0.0.1:${probe.address().port}`;
  await new Promise((resolve) => probe.close(resolve));
  const controlUrl = `http://${address}`;
  host = spawn(config.host, [...config.host_args, "--http-addr", address], {
    env: config.host_env,
    stdio: ["ignore", 2, 2],
  });
  host.on("error", (error) => {
    console.error(error);
    void stop();
  });
  host.on("exit", (code, signal) => {
    if (!stopping) {
      console.error(`control host exited: ${code ?? signal}`);
      process.exitCode = 1;
      void stop();
    }
  });
  const deadline = Date.now() + 110000;
  while (true) {
    if (host.exitCode !== null || host.signalCode !== null) throw new Error("control host stopped before readiness");
    try {
      const status = await new Promise((resolve, reject) => {
        const request = http.request(
          new URL(config.readiness.path, controlUrl),
          {
            method: config.readiness.method,
            headers: { host: config.route_host, "content-type": "application/json" },
            timeout: 1000,
          },
          (response) => {
            response.resume();
            resolve(response.statusCode);
          },
        );
        request.on("error", reject);
        request.on("timeout", () => request.destroy(new Error("control readiness timed out")));
        request.end();
      });
      if (status === 401) break;
    } catch {
      /* Wait for the exact control route to refuse an unsigned request. */
    }
    if (Date.now() >= deadline) throw new Error("control route did not become ready");
    await delay(100);
  }
  const platform = serve(config.platform_ui, false, controlUrl);
  servers.push(platform);
  const url = await listen(platform);
  const applications = [];
  for (const entry of config.packages) {
    const server = serve(entry.dist, true, controlUrl);
    servers.push(server);
    applications.push({ name: entry.name, url: await listen(server) });
  }
  writeFileSync(join(config.root, "web.json"), JSON.stringify({ url, applications }), { mode: 0o600 });
  process.stdout.write(`${url}\n`);
} catch (error) {
  console.error(error);
  process.exitCode = 1;
  await stop();
}
