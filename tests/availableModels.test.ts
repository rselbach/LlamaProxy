import { describe, expect, it } from 'bun:test';
import {
  configRecordWithModelEnabled,
  configuredModelSources,
  modelIsEnabled,
  mergeModelCatalogRecords,
  nativeDefaultCatalogsFromDefinitions,
  oauthModelSources,
  readSavedModelCatalogs,
  saveModelCatalog,
} from '../src/services/availableModels';

describe('available model sources', () => {
  it('keeps disabled OpenAI models discoverable and preserves their complete record', () => {
    const config = {
      'openai-compatibility': [{
        name: 'Greendale Relay',
        'base-url': 'https://greendale.example/v1',
        models: [{ name: 'troy-chat', alias: 'Troy', thinking: { levels: ['high'] }, custom: true }],
      }],
    };
    const storageValues = new Map<string, string>();
    const storage = {
      getItem: (key: string) => storageValues.get(key) ?? null,
      setItem: (key: string, value: string) => { storageValues.set(key, value); },
    };
    const source = configuredModelSources(config)[0];
    saveModelCatalog(source, storage);
    const disabled = configRecordWithModelEnabled(
      config['openai-compatibility'][0], source, 'troy-chat', false,
    );
    expect(disabled.models).toEqual([]);

    const reloaded = configuredModelSources(
      { 'openai-compatibility': [disabled] }, readSavedModelCatalogs(storage),
    )[0];
    expect(reloaded.models.map((model) => model.name)).toEqual(['troy-chat']);
    expect(modelIsEnabled(reloaded, reloaded.models[0])).toBe(false);
    expect(configRecordWithModelEnabled(disabled, reloaded, 'troy-chat', true).models).toEqual([
      { name: 'troy-chat', alias: 'Troy', thinking: { levels: ['high'] }, custom: true },
    ]);
  });

  it('keeps provider identities stable across reorder and distinct across keys', () => {
    const first = { name: 'Greendale', 'base-url': 'https://example.test/v1', 'api-key-entries': [{ 'api-key': 'troy-key' }], models: [] };
    const second = { name: 'Greendale', 'base-url': 'https://example.test/v1', 'api-key-entries': [{ 'api-key': 'abed-key' }], models: [] };
    const original = configuredModelSources({ 'openai-compatibility': [first, second] });
    const reordered = configuredModelSources({ 'openai-compatibility': [second, first] });
    expect(original[0].id).not.toBe(original[1].id);
    expect(reordered.map((source) => source.id)).toEqual([original[1].id, original[0].id]);
  });

  it('keeps saved disabled metadata when discovery returns a thinner record', () => {
    expect(mergeModelCatalogRecords(
      [{ name: 'troy-chat', displayName: 'Troy Chat' }, { name: 'new-model' }],
      [{ name: 'troy-chat', alias: 'troy', thinking: { levels: ['high'] }, custom: true }],
    )).toEqual([
      { name: 'troy-chat', alias: 'troy', thinking: { levels: ['high'] }, custom: true },
      { name: 'new-model' },
    ]);
  });

  it('restores every saved OpenAI alias after disable, discovery, and reload', () => {
    const records = [
      { name: 'same-upstream', alias: 'same-high', thinking: { effort: 'high' } },
      { name: 'same-upstream', alias: 'same-low', thinking: { effort: 'low' } },
    ];
    const config = {
      'openai-compatibility': [{
        name: 'Greendale Relay',
        'base-url': 'https://greendale.example/v1',
        'api-key-entries': [{ 'api-key': 'greendale-key' }],
        models: records,
      }],
    };
    const source = configuredModelSources(config)[0];
    const disabled = configRecordWithModelEnabled(
      config['openai-compatibility'][0], source, 'same-upstream', false,
    );
    expect(disabled.models).toEqual([]);
    const merged = mergeModelCatalogRecords(
      [{ name: 'same-upstream', displayName: 'Same Upstream' }],
      source.modelRecords,
    );
    expect(merged).toEqual(records);
    const reloaded = configuredModelSources(
      { 'openai-compatibility': [disabled] },
      { [source.id]: merged },
    )[0];
    expect(configRecordWithModelEnabled(disabled, reloaded, 'same-upstream', true).models)
      .toEqual(records);
  });

  it('uses routed aliases for native provider exclusions and preserves wildcard rules', () => {
    const record = {
      models: [{ name: 'upstream-model', alias: 'community-chat' }],
      'excluded-models': ['preview-*'],
    };
    const source = configuredModelSources({ 'claude-api-key': [record] })[0];
    const disabled = configRecordWithModelEnabled(record, source, 'upstream-model', false);
    expect(disabled['excluded-models']).toEqual(['preview-*', 'community-chat']);
    expect(disabled.models).toEqual([{ name: 'upstream-model', alias: 'community-chat' }]);
    const disabledSource = configuredModelSources(
      { 'claude-api-key': [disabled] },
      { [source.id]: source.modelRecords },
    )[0];
    expect(modelIsEnabled(disabledSource, disabledSource.models[0])).toBe(false);
    expect(configRecordWithModelEnabled(disabled, disabledSource, 'upstream-model', true)['excluded-models'])
      .toEqual(['preview-*']);
  });

  it('preserves duplicate-name aliases when toggling an unrelated native model', () => {
    const models = [
      { name: 'same-upstream', alias: 'same-high', thinking: { effort: 'high' } },
      { name: 'same-upstream', alias: 'same-low', thinking: { effort: 'low' } },
      { name: 'other-model', custom: true },
    ];
    const record = { models };
    const source = configuredModelSources({ 'codex-api-key': [record] })[0];
    const disabled = configRecordWithModelEnabled(record, source, 'other-model', false);
    expect(disabled.models).toEqual(models);
    expect(disabled['excluded-models']).toEqual(['other-model']);
    expect(source.modelRecords.filter((model) => model.name === 'same-upstream')).toHaveLength(2);
  });

  it('re-enables a native default model without creating a one-model allowlist', () => {
    const record = { 'excluded-models': ['default-model'] };
    const defaults = nativeDefaultCatalogsFromDefinitions({
      codex: { models: [{ id: 'default-model' }] },
    });
    const source = configuredModelSources(
      { 'codex-api-key': [record] }, {}, defaults,
    )[0];
    expect(modelIsEnabled(source, source.models[0])).toBe(false);
    const enabled = configRecordWithModelEnabled(record, source, 'default-model', true);
    expect(enabled['excluded-models']).toBeUndefined();
    expect(enabled.models).toBeUndefined();
  });

  it('shows native defaults as enabled, discovered extras as off, and preserves defaults when enabling an extra', () => {
    const defaults = nativeDefaultCatalogsFromDefinitions({
      codex: { models: [
        { id: 'gpt-default-a', display_name: 'Default A' },
        { id: 'gpt-default-b' },
      ] },
    });
    const initial = configuredModelSources(
      { 'codex-api-key': [{ 'api-key': 'greendale-key' }] },
      {},
      defaults,
    )[0];
    const source = {
      ...initial,
      models: [...initial.models, { name: 'gpt-upstream-extra' }],
      modelRecords: [...initial.modelRecords, { name: 'gpt-upstream-extra', custom: true }],
    };
    expect(modelIsEnabled(source, source.models[0])).toBe(true);
    expect(modelIsEnabled(source, { name: 'gpt-upstream-extra' })).toBe(false);

    const enabled = configRecordWithModelEnabled(
      { 'api-key': 'greendale-key' }, source, 'gpt-upstream-extra', true,
    );
    expect(enabled.models).toEqual([
      { name: 'gpt-default-a', 'display-name': 'Default A' },
      { name: 'gpt-default-b' },
      { name: 'gpt-upstream-extra', custom: true },
    ]);
  });

  it('reports native models off while the default catalog is unknown', () => {
    const source = configuredModelSources(
      { 'codex-api-key': [{ 'api-key': 'greendale-key' }] },
      {},
      {},
    )[0];
    const discovered = { name: 'gpt-upstream' };
    expect(modelIsEnabled({ ...source, models: [discovered] }, discovered)).toBe(false);
  });

  it('only creates OAuth sources for persisted credentials and ignores runtime API entries', () => {
    const sources = oauthModelSources({ files: [
      { name: 'codex.json', type: 'codex' },
      { name: 'runtime', type: 'claude', runtime_only: true },
    ] }, {
      codex: { models: [{ id: 'gpt-troy', display_name: 'GPT Troy' }] },
      claude: { models: [{ id: 'claude-abed' }] },
    }, { 'oauth-excluded-models': { codex: ['gpt-*'] } });
    expect(sources).toHaveLength(1);
    expect(sources[0]).toMatchObject({ provider: 'codex', excludedRules: ['gpt-*'] });
    expect(modelIsEnabled(sources[0], sources[0].models[0])).toBe(false);
  });
});
