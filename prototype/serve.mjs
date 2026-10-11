// Throwaway CueNote UI preview. Serves only the prototype, on loopback.
import http from 'node:http';
import { readFile } from 'node:fs/promises';

const port = Number(process.env.CUENOTE_PROTOTYPE_PORT || 4173);
const page = new URL('./cornell-editor-prototype.html', import.meta.url);
const server = http.createServer(async (request, response) => {
  const path = new URL(request.url, 'http://127.0.0.1').pathname;
  if (path === '/favicon.ico') { response.writeHead(204).end(); return; }
  if (!['/', '/cornell-editor-prototype.html'].includes(path)) {
    response.writeHead(404, { 'Content-Type': 'text/plain; charset=utf-8' }).end('未找到页面');
    return;
  }
  try {
    const html = await readFile(page);
    response.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff',
    }).end(html);
  } catch {
    response.writeHead(503, { 'Content-Type': 'text/plain; charset=utf-8' }).end('原型页面暂不可用');
  }
});
server.on('error', error => { console.error(error.message); process.exitCode = 1; });
server.listen(port, '127.0.0.1', () => {
  console.log(`CueNote 交互原型：http://127.0.0.1:${port}/?variant=A`);
  console.log('临时演示，刷新将重置内容。按 Ctrl+C 退出。');
});
