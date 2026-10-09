/**
 * 自动启动待命脚本（Node 载荷，watcher 写盘后经随应用分发的脚本运行时执行）。
 *
 * 触发契约（watcher 据此编排）：
 * - 待命：占住端口循环 accept；收到的任何请求（含 GET 类探测与插件握手）把首行带「[watch] 」前缀
 *   打到 stdout 供日志展示，均不触发；插件握手端点（GET /__llama-panel-watch-ping）额外回约定标记，
 *   供 runtime 识别自家占位监听；
 * - 触发：仅真实 API 调用（POST/PUT/PATCH/DELETE 与 CORS 预检 OPTIONS）触发：向 stdout 打印一行
 *   「请求首行 + " | ua=" + User-Agent」（watcher 的启动信号，后缀供日志定位调用方），
 *   随即让出端口并保持该连接，等 llama-server 就绪（/health 不再返回 503，最长 3 分钟）后
 *   转发请求、双向回传，完成后自然退出；
 * - 触发路径退出码恒为 0（watcher 以 stdout 首行为准），其余退出 = 待命异常。
 *
 * 退出一律走 exitCode + 收敛句柄而不是 process.exit：stdout 是管道时写入异步落盘，
 * process.exit 会截断尚未刷出的触发行，watcher 就收不到启动信号。
 */
import { WATCH_PING_MARKER, WATCH_PING_PATH } from "../runtime";

/** 待命期间收到的非触发请求首行带此前缀打到 stdout，watcher 按前缀判日志行。 */
export const NOISE_PREFIX = "[watch] ";

const NODE_SCRIPT = String.raw`import { createServer, createConnection } from "node:net";

const [bindHost, portArg, serverHost] = process.argv.slice(2);
const port = Number(portArg);
const WAIT_MS = 180000;
const PROBE_MS = 500;
const IDLE_MS = 300000;
const HEAD_TIMEOUT_MS = 2000;
const PING_PATH = "${WATCH_PING_PATH}";
const PING_MARKER = "${WATCH_PING_MARKER}";
const NOISE = "${NOISE_PREFIX}";

const say = (line) => { process.stdout.write(line + "\n"); };

function serverReady() {
  return new Promise((resolve) => {
    let settled = false;
    const done = (ok) => {
      if (settled) return;
      settled = true;
      socket.destroy();
      resolve(ok);
    };
    const socket = createConnection({ host: serverHost, port }, () => {
      socket.write("GET /health HTTP/1.0\r\n\r\n");
    });
    socket.setTimeout(1000, () => done(false));
    socket.on("error", () => done(false));
    socket.on("data", (chunk) => {
      const first = chunk.toString("latin1").split("\r\n", 1)[0] || "";
      done(first !== "" && first.indexOf(" 503 ") === -1);
    });
    socket.on("close", () => done(false));
  });
}

const server = createServer((socket) => {
  const chunks = [];
  let size = 0;
  let routed = false;
  const timer = setTimeout(() => { socket.destroy(); }, HEAD_TIMEOUT_MS);
  socket.on("error", () => socket.destroy());
  socket.on("close", () => clearTimeout(timer));
  socket.on("data", (chunk) => {
    if (routed) return;
    chunks.push(chunk);
    size += chunk.length;
    const head = Buffer.concat(chunks, size);
    if (head.indexOf("\r\n\r\n") === -1 && head.indexOf("\n\n") === -1 && size < 65536) return;
    routed = true;
    clearTimeout(timer);
    route(socket, head);
  });
});

server.on("error", (err) => {
  process.stderr.write((err && err.code === "EADDRINUSE" ? "端口已被占用" : String(err)) + "\n");
  process.exit(1);
});

server.listen(port, bindHost);

function route(socket, head) {
  const text = head.toString("latin1");
  const first = (text.split("\n", 1)[0] || "").replace(/\r$/, "").trim();
  const parts = first.split(/\s+/);
  const method = (parts[0] || "").toUpperCase();
  const path = (parts[1] || "").split("?", 1)[0];
  if (method === "GET" && path === PING_PATH) {
    say(NOISE + first);
    socket.end(
      "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\n" +
      "Content-Length: " + PING_MARKER.length + "\r\n\r\n" + PING_MARKER,
    );
    return;
  }
  if (method !== "POST" && method !== "PUT" && method !== "PATCH" && method !== "DELETE" && method !== "OPTIONS") {
    say(NOISE + first);
    socket.destroy();
    return;
  }
  let ua = "";
  const match = text.match(/^user-agent:[ \t]*(.*)$/im);
  if (match) ua = match[1].trim();
  say(first + " | ua=" + ua);
  server.close();
  forward(socket, head);
}

function forward(socket, head) {
  const chunks = [head];
  let clientGone = false;
  let relaying = false;
  socket.on("error", () => {});
  socket.on("close", () => { clientGone = true; });
  socket.on("data", (chunk) => { chunks.push(chunk); });
  const deadline = Date.now() + WAIT_MS;
  const poll = setInterval(() => {
    if (clientGone || Date.now() >= deadline) {
      clearInterval(poll);
      socket.destroy();
      process.exitCode = 0;
      return;
    }
    serverReady().then((ready) => {
      if (!ready || clientGone || relaying) return;
      relaying = true;
      clearInterval(poll);
      relay(socket, Buffer.concat(chunks));
    });
  }, PROBE_MS);
}

function relay(client, body) {
  const upstream = createConnection({ host: serverHost, port });
  let last = Date.now();
  const touch = () => { last = Date.now(); };
  let connectGuard;
  let idle;
  const finish = () => {
    clearTimeout(connectGuard);
    clearInterval(idle);
    client.destroy();
    upstream.destroy();
  };
  // llama-server 绑定成功后 connect 是毫秒级；连不上（防火墙丢包）5 秒放弃，客户端重试
  connectGuard = setTimeout(finish, 5000);
  idle = setInterval(() => {
    if (Date.now() - last > IDLE_MS) finish();
  }, 10000);
  client.on("error", finish);
  upstream.on("error", finish);
  client.on("close", finish);
  upstream.on("close", finish);
  upstream.on("connect", () => {
    clearTimeout(connectGuard);
    upstream.write(body);
    client.on("data", touch);
    upstream.on("data", touch);
    client.pipe(upstream);
    upstream.pipe(client);
  });
}
`;

export { NODE_SCRIPT };
