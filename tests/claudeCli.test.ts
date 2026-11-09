import { describe, expect, test } from 'bun:test';
import { parseClaudeCliStatus } from '../src/services/claudeCli';
import { isManagedRecord } from '../src/services/managedRecords';
import { providerCategoryMatchesRecord } from '../src/pages/ApiAccessPage';
import { configuredModelSources, modelIsEnabled, type AvailableModelSource } from '../src/services/availableModels';

describe('Claude CLI connection boundary', () => {
  test('parses disconnected and connected states and drops unknown fields', () => {
    expect(parseClaudeCliStatus({ installed: false, account: null, models: [], disabledModels: [] }))
      .toEqual({ installed: false, account: null, models: [], disabledModels: [] });
    expect(parseClaudeCliStatus({ installed: true, account: 'annie@greendale.edu', models: ['claude-cli/opus', 'claude-cli/haiku'], disabledModels: ['claude-cli/haiku'], token: 'private' }))
      .toEqual({ installed: true, account: 'annie@greendale.edu', models: ['claude-cli/opus', 'claude-cli/haiku'], disabledModels: ['claude-cli/haiku'] });
  });

  test('rejects malformed IPC', () => {
    for (const value of [null, {}, { installed: 'yes', account: null, models: [], disabledModels: [] },
      { installed: true, account: 7, models: [], disabledModels: [] },
      { installed: true, account: null, disabledModels: [] },
      { installed: true, account: null, models: [12], disabledModels: [] },
      { installed: true, account: null, models: [] },
      { installed: true, account: null, models: [], disabledModels: [12] }]) {
      expect(() => parseClaudeCliStatus(value)).toThrow();
    }
  });
});

describe('Claude CLI core route', () => {
  const managed = { 'base-url': 'http://127.0.0.1:4321/llamaproxy-claude-cli', models: [{ name: 'opus', alias: 'claude-cli/opus' }] };

  test('is owned by LlamaProxy, unlike a user entry that only looks similar', () => {
    expect(isManagedRecord(managed)).toBe(true);
    for (const record of [{ 'base-url': 'https://api.anthropic.com' },
      { 'base-url': 'http://127.0.0.1:4321/llamaproxy-claude-cli-other' },
      { 'base-url': 'http://example.com/llamaproxy-claude-cli' }]) {
      expect(isManagedRecord(record)).toBe(false);
    }
  });

  test('stays out of manual API access editing and configured model sources', () => {
    expect(providerCategoryMatchesRecord('claude-api-key', managed)).toBe(false);
    expect(configuredModelSources({ 'claude-api-key': [managed] })).toEqual([]);
  });

  test('lists its models as their own source, with the ones switched off disabled', () => {
    const source: AvailableModelSource = { kind: 'managed', id: 'claude-cli', label: 'Claude CLI',
      models: [{ name: 'claude-cli/opus' }, { name: 'claude-cli/haiku' }], disabledModels: ['claude-cli/haiku'] };
    expect(source.models.map((model) => modelIsEnabled(source, model))).toEqual([true, false]);
  });
});
