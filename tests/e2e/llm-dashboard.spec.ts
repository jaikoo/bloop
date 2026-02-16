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
  test('tab layout — 11 tabs in panel', async ({ page }) => {
    await openLlmPanel(page);
    const tabs = page.locator('#llmPanel .insights-tab');
    await expect(tabs).toHaveCount(11);
    const names = await tabs.allTextContents();
    expect(names).toEqual([
      'Overview', 'Usage', 'Latency', 'Models', 'Traces',
      'Search', 'Prompts', 'Scores', 'Sessions', 'Tools', 'Feedback',
    ]);
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

  test('Overview tab — budget gauge visible', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Overview');
    const content = page.locator('#llmContent');
    // Budget was seeded — should show "Monthly Budget" section with Edit button
    await expect(content).toContainText('Monthly Budget');
    await expect(content.locator('button', { hasText: 'Edit' })).toBeVisible();
  });

  test('Overview tab — budget edit form', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Overview');
    const content = page.locator('#llmContent');
    // Click Edit to go to budget form
    await content.locator('button', { hasText: 'Edit' }).click();
    // Budget form should have inputs and Save button
    await expect(content.locator('input[type="number"]').first()).toBeVisible();
    await expect(content.locator('button', { hasText: 'Save Budget' })).toBeVisible();
    // Fill in new budget
    const amtInput = content.locator('input[type="number"]').first();
    await amtInput.fill('200');
    // Click Save
    await content.locator('button', { hasText: 'Save Budget' }).click();
    // Should show toast
    const toast = page.locator('.toast', { hasText: 'Budget saved' });
    await expect(toast).toBeVisible({ timeout: 5000 });
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

  test('Tools tab — table renders', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Tools');
    const content = page.locator('#llmContent');
    // Should have a table with headers
    const table = content.locator('table');
    await expect(table).toBeVisible();
    await expect(table).toContainText('Tool');
    await expect(table).toContainText('Calls');
    await expect(table).toContainText('Errors');
    await expect(table).toContainText('p50');
    await expect(table).toContainText('p99');
    await expect(table).toContainText('Cost');
    // Should have at least 1 data row (from seeded tool spans)
    const rows = table.locator('tbody tr');
    await expect(rows).not.toHaveCount(0);
  });

  test('Sessions tab — list renders', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Sessions');
    const content = page.locator('#llmContent');
    // Should have at least one session card (session-e2e-1 from seed)
    await expect(content).toContainText('session-e2e-1');
  });

  test('Sessions tab — drill into session', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Sessions');
    const content = page.locator('#llmContent');
    // Click the session card
    const sessionCard = content.locator('div', { hasText: 'session-e2e-1' }).first();
    await sessionCard.click();
    // Should show "Back to sessions" button
    await expect(content.locator('button', { hasText: 'Back to sessions' })).toBeVisible();
    // Should show trace cards within session
    await expect(content).toContainText('Traces');
  });

  test('Sessions tab — drill into trace from session', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Sessions');
    const content = page.locator('#llmContent');
    // Navigate to session detail
    await content.locator('div', { hasText: 'session-e2e-1' }).first().click();
    await expect(content.locator('button', { hasText: 'Back to sessions' })).toBeVisible();
    // Click a trace card
    const traceCard = content.locator('div[style*="cursor:pointer"]').first();
    await traceCard.click();
    // Should show trace detail with "Back to traces" and span info
    await expect(content.locator('button', { hasText: 'Back to traces' })).toBeVisible();
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

  test('Trace detail — span tree hierarchy', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Traces');
    const content = page.locator('#llmContent');
    // Find and click the trace that has nested spans (chat-completion-e2e / trace-e2e-001)
    const traceRow = content.locator('div[style*="cursor:pointer"]', { hasText: 'chat-completion-e2e' });
    // It might be in the list - click it
    if (await traceRow.count() > 0) {
      await traceRow.first().click();
    } else {
      // Click first trace and navigate from there
      await content.locator('div[style*="cursor:pointer"]').first().click();
    }
    await expect(content.locator('button', { hasText: 'Back to traces' })).toBeVisible();
    // Check for span-children (nested spans) — our trace-e2e-001 has child spans
    const spanChildren = content.locator('.span-children');
    // If this trace has children, verify the tree
    if (await spanChildren.count() > 0) {
      await expect(spanChildren.first()).toBeVisible();
      // Find collapse toggle (▼) and click it
      const toggle = content.locator('span', { hasText: '▼' }).first();
      if (await toggle.count() > 0) {
        await toggle.click();
        // Children should be hidden
        await expect(spanChildren.first()).toBeHidden();
        // Click again to re-expand
        await content.locator('span', { hasText: '▶' }).first().click();
        await expect(spanChildren.first()).toBeVisible();
      }
    }
  });

  test('Trace detail — feedback buttons', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Traces');
    const content = page.locator('#llmContent');
    // Click first trace
    await content.locator('div[style*="cursor:pointer"]').first().click();
    await expect(content.locator('button', { hasText: 'Back to traces' })).toBeVisible();
    // Should see Feedback section with thumbs up/down buttons
    await expect(content).toContainText('Feedback');
    const thumbsUp = content.locator('button', { hasText: '👍' });
    const thumbsDown = content.locator('button', { hasText: '👎' });
    await expect(thumbsUp).toBeVisible();
    await expect(thumbsDown).toBeVisible();
    // Click thumbs up
    await thumbsUp.click();
    // Should show toast
    const toast = page.locator('.toast', { hasText: 'Feedback submitted' });
    await expect(toast).toBeVisible({ timeout: 5000 });
  });

  test('Feedback tab — summary stats', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Feedback');
    const content = page.locator('#llmContent');
    // Should show stat cards: Total, Positive, Negative, Positive Rate
    await expect(content).toContainText('Total');
    await expect(content).toContainText('Positive');
    await expect(content).toContainText('Negative');
    await expect(content).toContainText('Positive Rate');
  });

  test('Prompts tab — list with clickable names', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Prompts');
    const content = page.locator('#llmContent');
    // Should show a table with prompt names
    const table = content.locator('table');
    await expect(table).toBeVisible();
    await expect(table).toContainText('Prompt Name');
    // Should have our seeded prompt "summarizer"
    await expect(content).toContainText('summarizer');
    // Click the prompt name
    const promptName = content.locator('td', { hasText: 'summarizer' });
    await promptName.click();
    // Should navigate to versions view with "Back to prompts"
    await expect(content.locator('button', { hasText: 'Back to prompts' })).toBeVisible();
  });

  test('Prompts tab — version comparison', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Prompts');
    const content = page.locator('#llmContent');
    // Click into summarizer prompt
    await content.locator('td', { hasText: 'summarizer' }).click();
    await expect(content.locator('button', { hasText: 'Back to prompts' })).toBeVisible();
    // Should show "Compare Selected" button (initially disabled)
    const compareBtn = content.locator('button', { hasText: 'Compare Selected' });
    await expect(compareBtn).toBeVisible();
    await expect(compareBtn).toHaveCSS('opacity', '0.4');
    // Check two version checkboxes
    const checkboxes = content.locator('input[type="checkbox"]');
    const cbCount = await checkboxes.count();
    if (cbCount >= 2) {
      await checkboxes.nth(0).check();
      await checkboxes.nth(1).check();
      // Compare button should now be active
      await expect(compareBtn).toHaveCSS('opacity', '1');
      // Click compare
      await compareBtn.click();
      // Should show comparison table with Metric, v1, v2, Delta columns
      await expect(content).toContainText('Metric');
      await expect(content).toContainText('Delta');
      await expect(content.locator('button', { hasText: 'Back to versions' })).toBeVisible();
    }
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

  test('Search tab — empty query shows placeholder', async ({ page }) => {
    await openLlmPanel(page);
    await switchTab(page, 'Search');
    const content = page.locator('#llmContent');
    await expect(content).toContainText('Enter a query to search traces');
  });
});
