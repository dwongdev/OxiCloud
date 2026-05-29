import { expect } from '@playwright/test';
import { test, goToLoginPage, loginAsAdmin, TEST_ADMIN } from './helpers';

test('has OxiCloud title', async ({ page }) => {
  await page.goto('/');
  await expect(page).toHaveTitle(/OxiCloud/);
});

test('language selector > choose EN > reach login page', async ({ page }) => {
  await page.goto('/');

  await expect(page.locator('#language-panel')).toBeVisible();
  await expect(page.getByText('Select your language to continue')).toBeVisible();
  await expect(page.locator('#lang-picker-name')).toHaveText('English');

  await expect(page.locator('#language-panel')).toHaveScreenshot('language-selector.png');

  await page.locator('#language-continue').click();

  await expect(page.locator('#login-panel')).toBeVisible();
  await expect(page.locator('#language-panel')).toBeHidden();

  await expect(page.locator('#login-panel')).toHaveScreenshot('login-panel.png');
});

test('login with wrong password is rejected', async ({ page }) => {
  await goToLoginPage(page);

  await page.locator('#login-username').fill(TEST_ADMIN.username);
  await page.locator('#login-password').fill('definitely-wrong-password');
  await page.locator('#login-panel button[type="submit"]').click();

  const loginError = page.locator('#login-error');
  await expect(loginError).toBeVisible();
  await expect(loginError).toContainText('Authentication error (403): Forbidden');

  await expect(page.locator('#login-panel')).toBeVisible();
  await expect(page.locator('#login-panel')).toHaveScreenshot('login-panel-error.png');
});

test.describe('authenticated as admin', () => {
  test.beforeEach(async ({ page }) => {
    await loginAsAdmin(page);
  });

  test('home page shows empty files list', async ({ page }) => {
    await expect(page.locator('.files-container')).toBeVisible();
    await expect(page.locator('#user-menu-wrapper')).toBeVisible();
    await expect(page).toHaveScreenshot('home-files.png', {
      // ignore this div (max value may change)
      // FIXME: ensure same max capacity from server during test
      animations: 'disabled',
      mask: [page.locator('.storage-bar'), page.locator('.storage-info') ]
    });
    await expect(page.locator('#files-container-error')).toBeVisible();
    await expect(page.locator('#files-container-error')).toContainText("No files in this folder");
  });

  test('theme can be changed to dark', async ({ page }) => {
    await page.locator('#user-avatar-btn').click();
    await expect(page.locator('#user-menu')).toBeVisible();

    // The appearance row is now a 3-option segmented control
    // (Light / Like OS / Dark). Each option carries a `data-mode` attribute.
    await page.locator('.theme-segmented__opt[data-mode="dark"]').click();

    // html element must carry data-color-scheme="dark" (new attribute).
    await expect(page.locator('html')).toHaveAttribute('data-color-scheme', 'dark');

    // localStorage must persist the choice.
    const theme = await page.evaluate(() => localStorage.getItem('oxicloud_theme'));
    expect(theme).toBe('dark');

    await expect(page).toHaveScreenshot('home-files-darktheme.png', {
      // ignore this div (max value may change)
      // FIXME: ensure same max capacity from server during test
      animations: 'disabled',
      mask: [page.locator('.storage-bar'), page.locator('.storage-info') ]
    });

    // Switch back to light via the Light option.
    await page.locator('.theme-segmented__opt[data-mode="light"]').click();
    await expect(page.locator('html')).toHaveAttribute('data-color-scheme', 'light');

    const themeAfter = await page.evaluate(() => localStorage.getItem('oxicloud_theme'));
    expect(themeAfter).toBe('light');

    await expect(page).toHaveScreenshot('home-files-lightheme.png', {
      // ignore this div (max value may change)
      // FIXME: ensure same max capacity from server during test
      animations: 'disabled',
      mask: [page.locator('.storage-bar'), page.locator('.storage-info') ]
    });

  });
});
