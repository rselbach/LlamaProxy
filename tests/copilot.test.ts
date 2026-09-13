import { describe, expect, test } from 'bun:test';
import { isManagedCopilotRecord, parseCopilotStatus } from '../src/services/copilot';
import { providerCategoryMatchesRecord, sectionRecordsFromConfig } from '../src/pages/ApiAccessPage';

describe('Copilot connection boundary', () => {
  test('parses disconnected, pending, and connected states without retaining secrets', () => {
    expect(parseCopilotStatus({ login: null, models: [], pending: null })).toEqual({ login: null, models: [], pending: null });
    const pending = { userCode: 'TROY-ABED', url: 'https://github.com/login/device', expiresIn: 900 };
    expect(parseCopilotStatus({ login: null, models: [], pending, accessToken: 'private-token' })).toEqual({ login: null, models: [], pending });
    expect(parseCopilotStatus({ login: 'troy-barnes', models: ['copilot/greendale'], pending: null }).login).toBe('troy-barnes');
  });

  test('rejects malformed IPC and arbitrary verification URLs', () => {
    for (const value of [null, {}, { login: false, models: [], pending: null },
      { login: null, models: [12], pending: null }, { login: null, models: [] },
      { login: null, models: [], pending: { userCode: 'TROY-ABED', url: 'https://example.com', expiresIn: 900 } },
      { login: null, models: [], pending: { userCode: 'TROY-ABED', url: 'https://github.com/login/device', expiresIn: NaN } }]) {
      expect(() => parseCopilotStatus(value)).toThrow();
    }
  });
});

describe('Copilot-owned core routes', () => {
  const managed = { name: 'GitHub Copilot', 'base-url': 'http://127.0.0.1:4321/llamaproxy-copilot' };

  test('identifies only LlamaProxy loopback routes, not similarly named user providers', () => {
    expect(isManagedCopilotRecord(managed)).toBe(true);
    for (const record of [{ name: 'GitHub Copilot' }, { 'base-url': 'https://api.githubcopilot.com' },
      { 'base-url': 'http://127.0.0.1:4321/llamaproxy-copilot-other' },
      { 'base-url': 'http://example.com/llamaproxy-copilot' }]) {
      expect(isManagedCopilotRecord(record)).toBe(false);
    }
  });

  test('hides managed entries from manual editing but preserves their original configuration slots', () => {
    for (const section of ['openai-compatibility', 'codex-api-key', 'claude-api-key'] as const) {
      const records = [managed, { 'base-url': 'https://example.com' }];
      expect(sectionRecordsFromConfig({ [section]: records }, section)).toEqual(records);
    }
    for (const category of ['openai-compatibility', 'codex-api-key', 'claude-api-key'] as const) {
      expect(providerCategoryMatchesRecord(category, managed)).toBe(false);
    }
  });
});
