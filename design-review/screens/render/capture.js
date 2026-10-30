// Renders every board in ../../canvas/canvas.json to PNG with headless Chrome.
// usage: node capture.js [board ...]   (needs playwright-core on NODE_PATH; CHROME=path to Chrome)
const http = require('http');
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright-core');

const here = __dirname;
const canvas = path.resolve(here, '../../canvas');
const screens = path.resolve(here, '..');
const chrome = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const renames = { 'P-Launcher-Light': 'P-Launcher', 'P-Launcher-Light-List': 'P-Launcher-List' };

function outFile(board, page) {
  if (page === 'proto') return `dark/${board}.png`;
  if (page === 'light') return `light/${renames[board] || board.replace('P-Light-', 'P-')}.png`;
  return `studies/${board}.png`;
}

const server = http.createServer((req, res) => {
  const url = decodeURIComponent(req.url.split('?')[0]);
  const base = path.basename(url);
  const file = url.startsWith('/_blob/') ? path.join(here, '_blob', base)
    : /^(support\.js|_render\.html)$/.test(base) ? path.join(here, base)
    : path.join(canvas, base);
  fs.readFile(file, (err, data) => {
    if (err) { res.writeHead(404); return res.end(); }
    const type = { '.js': 'text/javascript', '.html': 'text/html', '.png': 'image/png', '.json': 'application/json' }[path.extname(file)];
    res.writeHead(200, { 'content-type': type || 'application/octet-stream' });
    res.end(data);
  });
});

(async () => {
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  const port = server.address().port;
  const { boards } = JSON.parse(fs.readFileSync(path.join(canvas, 'canvas.json'), 'utf8'));
  const only = process.argv.slice(2);
  const browser = await chromium.launch({ executablePath: chrome, headless: true });
  let failed = 0;
  for (const [file, b] of Object.entries(boards)) {
    const board = file.replace(/\.dc\.html$/, '');
    if (only.length && !only.includes(board)) continue;
    const ctx = await browser.newContext({ viewport: { width: b.w, height: b.h }, colorScheme: 'dark' });
    const page = await ctx.newPage();
    await page.goto(`http://127.0.0.1:${port}/_render.html?b=${board}`);
    await page.waitForFunction(() => window.__done, null, { timeout: 20000 });
    const done = await page.evaluate(() => window.__done);
    await page.evaluate(() => document.fonts.ready);
    // Let entry animations settle; animated boards are captured at this moment.
    await page.waitForTimeout(2500);
    const out = path.join(screens, outFile(board, b.page));
    fs.mkdirSync(path.dirname(out), { recursive: true });
    await page.screenshot({ path: out });
    if (done !== 'ok') failed++;
    console.log(done === 'ok' ? 'ok  ' : 'FAIL', path.relative(screens, out), done === 'ok' ? '' : done);
    await ctx.close();
  }
  await browser.close();
  server.close();
  process.exit(failed ? 1 : 0);
})();
