// Run Vite on port 1420, then node tests/copilot-ui.cjs. IPC is mocked; no GitHub access occurs.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');

(async () => {
  const browser = await chromium.launch({ channel: 'chrome', headless: true });
  try {
    const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
    const errors = [];
    page.on('pageerror', error => errors.push(String(error)));
    const open = async query => {
      await page.goto(`http://localhost:1420/tests/fixtures/copilot.html?${query}`);
      await page.getByRole('heading', { name: 'GitHub Copilot' }).waitFor();
    };
    const button = name => page.getByRole('button', { name, exact: true });
    const start = async () => {
      await page.locator('.oauth-card').getByRole('button', { name: 'Start Sign-In', exact: true }).click();
      await page.getByText('TROY-ABED', { exact: true }).waitFor();
    };
    await open('');
    await start();
    await button('Open Link').click();
    await page.waitForFunction(() => window.copilotFixture.calls.some(call => call.command === 'open_oauth_url'));
    const opened = await page.evaluate(() => window.copilotFixture.calls.find(call => call.command === 'open_oauth_url').args);
    assert.equal(opened.url, 'https://github.com/login/device');
    await page.getByText('1 Copilot models available.', { exact: true }).waitFor();
    await button('Refresh models').click();
    await page.getByText('2 Copilot models available.', { exact: true }).waitFor();
    await button('Disconnect').click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Cancel', exact: true }).click();
    assert.equal(await page.evaluate(() => window.copilotFixture.calls.filter(call => call.command === 'disconnect_copilot').length), 0);
    await button('Disconnect').click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Disconnect', exact: true }).click();
    await button('Start Sign-In').waitFor();
    assert.equal(await page.getByText('troy-barnes', { exact: true }).count(), 0);

    await open('defer-poll&theme=dark');
    await start();
    await page.waitForFunction(() => window.copilotFixture.completePoll !== null);
    await button('Cancel').click();
    await button('Start Sign-In').waitFor();
    await page.evaluate(() => window.copilotFixture.completePoll());
    await page.waitForTimeout(100);
    assert.equal(await page.getByText('troy-barnes', { exact: true }).count(), 0);

    await open('denied');
    await start();
    await page.getByRole('alert').filter({ hasText: 'GitHub authorization was denied' }).waitFor();
    await button('Start Sign-In').waitFor();
    await open('easy&defer-initial');
    await button('Interactive Guide').click();
    await page.locator('button.simple-mode-choice').filter({ hasText: 'OAuth Sign-In' }).click();
    const nextGuideStep = page.locator('.guide-interactive-card').getByRole('button', { name: 'Next', exact: true });
    await nextGuideStep.click();
    assert.equal(await nextGuideStep.isDisabled(), true);
    await start();
    await page.getByText('1 Copilot models available.', { exact: true }).waitFor();
    await page.waitForFunction(() => !document.querySelector('.guide-interactive-card .primary-button').disabled);
    await page.evaluate(() => window.copilotFixture.completeInitial());
    await page.waitForTimeout(100);
    assert.equal(await nextGuideStep.isDisabled(), false);
    await button('Disconnect').click();
    await page.getByRole('alertdialog').getByRole('button', { name: 'Disconnect', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('.guide-interactive-card .primary-button').disabled);
    assert.deepEqual(errors, []);
    console.log('Copilot UI: device login, URL opening, polling, refresh, confirmed disconnect, cancellation, denial, and Beginner Mode guide passed.');
  } finally {
    await browser.close();
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
