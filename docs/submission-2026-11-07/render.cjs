// submission.html を A4 の PDF と、確認用の PNG に書き出す。1ページに収まらない場合は失敗する。
const { chromium } = require('playwright');
const path = require('path');
(async () => {
  const dir = process.argv[2];
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 794, height: 1123 }, deviceScaleFactor: 2 });
  await page.goto('file://' + path.resolve(dir, 'submission.html'));
  await page.evaluate(() => document.fonts.ready);
  const over = await page.evaluate(() => { const p = document.querySelector('.page'); return { scroll: p.scrollHeight, client: p.clientHeight, clipped: [...p.querySelectorAll('*')].filter(e => e.scrollWidth > e.clientWidth + 1 && getComputedStyle(e).overflow !== 'visible').length }; });
  console.log(JSON.stringify(over));
  await page.pdf({ path: path.resolve(dir, 'submission.pdf'), preferCSSPageSize: true, printBackground: true });
  await page.screenshot({ path: path.resolve(dir, 'submission-preview.png'), fullPage: true });
  await browser.close();
  if (over.scroll > over.client + 1) { console.error('1ページに収まっていない'); process.exit(1); }
})();
