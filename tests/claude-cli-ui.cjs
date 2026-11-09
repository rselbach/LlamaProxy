// Run Vite on port 1420, then node tests/claude-cli-ui.cjs. IPC is mocked; Claude Code never runs.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    const open = async query => {
      await page.goto(`http://localhost:1420/tests/fixtures/claude-cli.html?${query}`);
      await page.getByRole('heading', { name: 'Claude CLI' }).waitFor();
    };
    const button = name => page.getByRole('button', { name, exact: true });

    await open('');
    await page.getByText('your account may be at risk', { exact: false }).waitFor();
    await button('Connect').click();
    await page.getByText('annie@greendale.edu', { exact: true }).waitFor();
    await page.getByText('2 Claude CLI models available.', { exact: true }).waitFor();
    if (process.env.SCREENSHOT) await page.screenshot({ path: process.env.SCREENSHOT });
    await button('Disconnect').click();
    await button('Connect').waitFor();
    assert.equal(await page.getByText('annie@greendale.edu', { exact: true }).count(), 0);

    await open('missing&theme=dark');
    await page.getByText('Claude Code was not found.', { exact: false }).waitFor();
    assert.equal(await button('Connect').isDisabled(), true);

    await open('signed-out');
    await button('Connect').click();
    await page.getByRole('alert').getByText('Claude Code is not signed in', { exact: false }).waitFor();
    await button('Connect').waitFor();

    for (const [locale, connect] of [['ja', '接続'], ['zh-CN', '连接'], ['zh-TW', '連接']]) {
      await open(`locale=${locale}`);
      await button(connect).waitFor();
    }
    assert.deepEqual(errors, []);
    console.log('Claude CLI connection UI passed');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exit(1); });
