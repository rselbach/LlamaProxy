// Run Vite on port 1420, then node tests/models-ui.cjs. IPC is mocked; no provider is contacted.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    await page.goto('http://localhost:1420/tests/fixtures/models.html');
    const row = model => page.locator('tr').filter({ has: page.getByText(model, { exact: true }) });
    const enabled = model => row(model).getByRole('checkbox').isChecked();
    const flip = model => row(model).locator('.switch-control').click();

    for (const [model, provider] of [['claude-cli/claude-opus-5-5', 'Claude CLI'], ['claude-cli/haiku', 'Claude CLI'],
      ['copilot/gpt-5.4', 'GitHub Copilot'], ['copilot/paintball-preview', 'GitHub Copilot']]) {
      await row(model).getByText(provider, { exact: true }).waitFor();
    }
    assert.equal(await enabled('claude-cli/claude-opus-5-5'), true);
    assert.equal(await enabled('claude-cli/haiku'), false);
    assert.equal(await enabled('copilot/gpt-5.4'), true);
    assert.equal(await enabled('copilot/paintball-preview'), false);

    await flip('claude-cli/haiku');
    await page.waitForFunction(() => [...document.querySelectorAll('tr')]
      .some(item => item.textContent.includes('claude-cli/haiku') && item.querySelector('input').checked));
    await flip('copilot/gpt-5.4');
    await page.waitForFunction(() => [...document.querySelectorAll('tr')]
      .some(item => item.textContent.includes('copilot/gpt-5.4') && !item.querySelector('input').checked));
    assert.equal(await enabled('claude-cli/claude-opus-5-5'), true);
    if (process.env.SCREENSHOT) await page.screenshot({ path: process.env.SCREENSHOT });
    assert.deepEqual(errors, []);
    console.log('Models UI: Claude CLI and Copilot models are listed and switch independently.');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exit(1); });
