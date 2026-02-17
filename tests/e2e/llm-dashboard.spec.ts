import { test, expect, Page } from '@playwright/test';
import { readFileSync } from 'fs';
import { join } from 'path';

function getState() {
  return JSON.parse(readFileSync(join(__dirname, '.e2e-state.json'), 'utf-8'));
}

async function openLlmPanel(page: Page) {
  // Set session cookie so auth passes
  const state = getState();
  const url = new URL(state.baseUrl);
  await page.context().addCookies([{
    name: 'bloop_session',
    value: state.sessionToken,
    domain: url.hostname,
    path: '/',
  }]);

  await page.goto('/');
  // Dismiss welcome modal by marking onboarded in localStorage
  await page.evaluate(() => localStorage.setItem('bloop_onboarded', '1'));
  // Reload so the modal doesn't appear
  await page.reload();
  // Wait for the page to load and the LLM button to be visible
  await page.waitForSelector('#llmBtn', { state: 'visible' });
  await page.click('#llmBtn');
  // Wait for the LLM panel to become visible
  await page.waitForSelector('#llmPanel:not(.hidden)', { state: 'attached' });
}

async function switchTab(page: Page, tabName: string) {
  const tab = page.locator(`#llmPanel .insights-tab`).filter({ hasText: tabName });
  await tab.click();
  // Wait for loading to finish
  await page.waitForFunction(
    () => !document.querySelector('#llmContent .insights-loading'),
    { timeout: 10_000 },
  );
}

test.describe('LLM Dashboard', () => {
  test('tab layout — expected tabs in panel', async ({ page }) => {
    await openLlmPanel(page);
    const tabs = page.locator('#llmPanel .insights-tab');
    const count = await tabs.count();
    expect(count).toBeGreaterThanOrEqual(9);
    const names = await tabs.allTextContents();
    // These 9 tabs are always present
    for (const expected of ['Overview', 'Usage', 'Latency', 'Models', 'Traces', 'Search', 'Prompts', 'Scores', 'RAG']) {
      expect(names).toContain(expected);
    }
  });

  test('Overview tab — stat cards render', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Overview');
    // Wait for stat cards to appear (there should be 6: Traces, Spans, Tokens, Cost, Errors, Error Rate)
    const content = page.locator('#llmContent');
    await expect(content).not.toContainText('Failed to load');
    // The overview renders stat cards — look for key labels
    await expect(content).toContainText('Traces');
    await expect(content).toContainText('Spans');
    await expect(content).toContainText('Tokens');
    await expect(content).toContainText('Cost');
    await expect(content).toContainText('Errors');
    await expect(content).toContainText('Error Rate');
  });

  test('Usage tab — table renders', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Usage');
    const content = page.locator('#llmContent');
    const table = content.locator('table');
    await expect(table).toBeVisible();
    await expect(table).toContainText('Time');
    await expect(table).toContainText('Model');
    await expect(table).toContainText('Spans');
    await expect(table).toContainText('Tokens');
    await expect(table).toContainText('Cost');
    // Table has no <tbody> — data rows follow the header row
    const rows = table.locator('tr');
    const count = await rows.count();
    expect(count).toBeGreaterThan(1); // at least header + 1 data row
  });

  test('Latency tab — table renders', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Latency');
    const content = page.locator('#llmContent');
    const table = content.locator('table');
    await expect(table).toBeVisible();
    await expect(table).toContainText('Model');
    await expect(table).toContainText('p50');
    await expect(table).toContainText('p90');
    await expect(table).toContainText('p99');
    await expect(table).toContainText('TTFT');
    await expect(table).toContainText('Calls');
    // Table has no <tbody> — data rows follow the header row
    const rows = table.locator('tr');
    const count = await rows.count();
    expect(count).toBeGreaterThan(1);
    await expect(content).toContainText('ms');
  });

  test('Models tab — cards render', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Models');
    const content = page.locator('#llmContent');
    await expect(content).toContainText('gpt-4o');
    await expect(content).toContainText('calls');
    await expect(content).toContainText('tokens');
  });

  test('Scores tab — score cards render', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Scores');
    const content = page.locator('#llmContent');
    await expect(content).toContainText('quality');
    await expect(content).toContainText('P10');
    await expect(content).toContainText('P50');
    await expect(content).toContainText('P90');
    await expect(content).toContainText('scores');
  });

  test('Traces tab — trace list renders', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Traces');
    const content = page.locator('#llmContent');
    // Should show total traces count
    await expect(content).toContainText('total traces');
    // Should show at least one trace row
    await expect(content).toContainText('spans');
  });

  test('Traces tab — trace detail with span tree', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Traces');
    const content = page.locator('#llmContent');
    // Click the first trace row (should be a clickable div)
    const traceRow = content.locator('div[style*="cursor:pointer"]').first();
    await traceRow.click();
    // Wait for trace detail to load
    await expect(content.locator('button', { hasText: 'Back to traces' })).toBeVisible();
    // Should show spans
    await expect(content).toContainText('Spans');
  });

  test('Prompts tab — table renders', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Prompts');
    const content = page.locator('#llmContent');
    // Should show a table with prompt names
    const table = content.locator('table');
    await expect(table).toBeVisible();
    await expect(table).toContainText('Prompt Name');
    // Should have our seeded prompt "summarizer"
    await expect(content).toContainText('summarizer');
  });

  test('Search tab — query and results', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Search');
    const content = page.locator('#llmContent');
    // Should show search input
    const input = content.locator('input[type="text"]');
    await expect(input).toBeVisible();
    // Type a query matching our seeded data
    await input.fill('quantum');
    await input.press('Enter');
    // Wait for results to load
    await page.waitForFunction(
      () => !document.querySelector('#llmContent .insights-loading'),
      { timeout: 10_000 },
    );
    // Should show results or empty state
    const hasResults = await content.locator('div[style*="cursor:pointer"]').count();
    if (hasResults > 0) {
      // Found matching traces
      await expect(content).toContainText('matching traces');
    } else {
      // Empty state is also valid
      await expect(content).toContainText('No traces found');
    }
  });

  test('RAG tab — renders stat cards or empty state', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'RAG');
    const content = page.locator('#llmContent');
    await expect(content).not.toContainText('Failed to load');
    // Should show either RAG data or the empty state
    const text = await content.textContent();
    const hasData = text?.includes('Total Retrievals');
    const isEmpty = text?.includes('No RAG data');
    expect(hasData || isEmpty).toBeTruthy();
  });

  test('Search tab — empty query shows placeholder', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Search');
    const content = page.locator('#llmContent');
    await expect(content).toContainText('Enter a query to search traces');
  });
});
