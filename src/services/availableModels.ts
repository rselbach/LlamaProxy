import { isManagedCopilotRecord } from './copilot';
import { isRecord, normalizeAuthIndex, readBoolean, readString, responseList } from './managementApi';
import { modelMatchesRule, normalizeOAuthExcludedRules, oauthModelsFromPayload } from './oauthModels';
import { modelsFromRecord, type ModelOption } from './modelService';
import { isRuntimeOnlyAuthFile } from './authFiles';

export const providerSections = [
  'codex-api-key',
  'openai-compatibility',
  'claude-api-key',
  'gemini-api-key',
] as const;

export type ProviderSection = (typeof providerSections)[number];

export type ConfigModelSource = {
  kind: 'config';
  id: string;
  section: ProviderSection;
  index: number;
  label: string;
  models: ModelOption[];
  enabledModels: string[];
  modelRecords: Record<string, unknown>[];
  defaultModelRecords: Record<string, unknown>[];
  defaultModels: string[];
  defaultCatalogKnown: boolean;
  excludedRules: string[];
  disabled: boolean;
  explicitAllowlist: boolean;
  connection: { baseUrl: string; apiKey: string; authIndex?: string; headers: Record<string, string> };
};

export type OAuthModelSource = {
  kind: 'oauth';
  id: string;
  provider: string;
  label: string;
  models: ModelOption[];
  excludedRules: string[];
};

export type CopilotModelSource = {
  kind: 'copilot';
  id: 'copilot';
  label: string;
  models: ModelOption[];
  disabledModels: string[];
};

export type AvailableModelSource = ConfigModelSource | OAuthModelSource | CopilotModelSource;

export type NativeDefaultCatalogs = Partial<Record<ProviderSection, Record<string, unknown>[]>>;

const CATALOG_STORAGE_KEY = 'llamaproxy.model-catalogs.v1';

export const readSavedModelCatalogs = (storage: Pick<Storage, 'getItem'> = localStorage): Record<string, Record<string, unknown>[]> => {
  try {
    const parsed: unknown = JSON.parse(storage.getItem(CATALOG_STORAGE_KEY) ?? '{}');
    if (!isRecord(parsed)) return {};
    return Object.fromEntries(Object.entries(parsed).map(([id, value]) => [
      id,
      Array.isArray(value) ? value.filter(isRecord) : [],
    ]));
  } catch {
    return {};
  }
};

export const saveModelCatalog = (
  source: ConfigModelSource,
  storage: Pick<Storage, 'getItem' | 'setItem'> = localStorage,
) => {
  const catalogs = readSavedModelCatalogs(storage);
  storage.setItem(CATALOG_STORAGE_KEY, JSON.stringify({ ...catalogs, [source.id]: source.modelRecords }));
};

export const mergeModelCatalogRecords = (
  discovered: Record<string, unknown>[],
  saved: Record<string, unknown>[],
) => {
  const savedNames = new Set(saved.map((model) =>
    readString(model, 'name', 'id', 'model', 'value').toLowerCase()).filter(Boolean));
  const merged = new Map([
    ...saved,
    ...discovered.filter((model) => !savedNames.has(
      readString(model, 'name', 'id', 'model', 'value').toLowerCase(),
    )),
  ].flatMap((model) => {
    const name = readString(model, 'name', 'id', 'model', 'value');
    return name ? [[modelRecordKey(model), model] as const] : [];
  }));
  return [...merged.values()];
};

export const nativeDefaultCatalogsFromDefinitions = (
  definitions: Record<string, unknown>,
): NativeDefaultCatalogs => Object.fromEntries(([
  ['codex-api-key', 'codex'],
  ['claude-api-key', 'claude'],
  ['gemini-api-key', 'gemini'],
] as const).flatMap(([section, provider]) => {
  const payload = definitions[provider];
  if (!isRecord(payload) || !Array.isArray(payload.models)) return [];
  return [[section, payload.models.filter(isRecord).flatMap((model) => {
    const name = readString(model, 'id', 'name');
    if (!name) return [];
    const record: Record<string, unknown> = { ...model, name };
    delete record.id;
    if (typeof record.display_name === 'string' && record['display-name'] === undefined) {
      record['display-name'] = record.display_name;
      delete record.display_name;
    }
    return [record];
  })] as const];
}));

const sectionLabel = (section: ProviderSection) => ({
  'codex-api-key': 'Codex API',
  'openai-compatibility': 'OpenAI compatible',
  'claude-api-key': 'Claude API',
  'gemini-api-key': 'Gemini API',
})[section];

const modelRecordKey = (model: Record<string, unknown>) => [
  readString(model, 'name', 'id', 'model', 'value').toLowerCase(),
  readString(model, 'alias').toLowerCase(),
].join('\0');

const providerSourceId = (
  section: ProviderSection,
  name: string,
  baseUrl: string,
  record: Record<string, unknown>,
) => {
  const apiKeys = section === 'openai-compatibility' && Array.isArray(record['api-key-entries'])
    ? record['api-key-entries'].filter(isRecord).map((entry) => readString(entry, 'api-key', 'apiKey'))
    : [readString(record, 'api-key', 'apiKey')];
  const identity = [section, name, baseUrl, ...apiKeys].join('\0');
  let hash = 2166136261;
  for (let index = 0; index < identity.length; index += 1) {
    hash ^= identity.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${section}:${(hash >>> 0).toString(36)}`;
};

export const configuredModelSources = (
  payload: unknown,
  savedCatalogs: Record<string, Record<string, unknown>[]> = {},
  defaultCatalogs: NativeDefaultCatalogs = {},
): ConfigModelSource[] => providerSections.flatMap((section) => {
  const records = responseList(payload, section);
  return records.flatMap((record, index) => {
    if (isManagedCopilotRecord(record)) return [];
    const name = readString(record, 'name');
    const baseUrl = readString(record, 'base-url', 'baseUrl');
    const id = providerSourceId(section, name, baseUrl, record);
    const configuredRecords = Array.isArray(record.models) ? record.models.filter(isRecord) : [];
    const configured = modelsFromRecord(configuredRecords);
    const defaultCatalogKnown = Object.prototype.hasOwnProperty.call(defaultCatalogs, section);
    const defaultModelRecords = defaultCatalogs[section] ?? [];
    const defaultModels = modelsFromRecord(defaultModelRecords).map((model) => model.name.toLowerCase());
    const modelRecords = new Map<string, Record<string, unknown>>();
    [...(savedCatalogs[id] ?? []), ...defaultModelRecords, ...configuredRecords].forEach((model) => {
      const modelName = readString(model, 'name', 'id', 'model', 'value');
      if (modelName) modelRecords.set(modelRecordKey(model), model);
    });
    const models = modelsFromRecord([...configuredRecords, ...modelRecords.values()]);
    const excludedRules = Array.isArray(record['excluded-models'])
      ? normalizeOAuthExcludedRules(record['excluded-models'].map(String))
      : [];
    return [{
      kind: 'config' as const,
      id,
      section,
      index,
      label: name || sectionLabel(section),
      models: models.sort((left, right) => left.name.localeCompare(right.name)),
      enabledModels: configured.map((model) => model.name.toLowerCase()),
      modelRecords: [...modelRecords.values()],
      defaultModelRecords,
      defaultModels,
      defaultCatalogKnown,
      excludedRules,
      disabled: section === 'openai-compatibility'
        ? readBoolean(record, 'disabled')
        : excludedRules.includes('*'),
      explicitAllowlist: configuredRecords.length > 0,
      connection: {
        baseUrl,
        apiKey: section === 'openai-compatibility'
          ? readString(Array.isArray(record['api-key-entries']) ? record['api-key-entries'][0] : null, 'api-key', 'apiKey')
          : readString(record, 'api-key', 'apiKey'),
        authIndex: normalizeAuthIndex(record['auth-index'] ?? (Array.isArray(record['api-key-entries']) && isRecord(record['api-key-entries'][0]) ? record['api-key-entries'][0]['auth-index'] : undefined)) || undefined,
        headers: isRecord(record.headers)
          ? Object.fromEntries(Object.entries(record.headers).map(([key, value]) => [key, String(value)]))
          : {},
      },
    }];
  });
});

export const oauthModelSources = (
  authFilesPayload: unknown,
  definitions: Record<string, unknown>,
  excludedPayload: unknown,
): OAuthModelSource[] => {
  const files = responseList(authFilesPayload, 'files');
  const providers = new Set(files
    .filter((file) => !readBoolean(file, 'disabled') && !isRuntimeOnlyAuthFile(file))
    .map((file) => readString(file, 'provider', 'type').toLowerCase())
    .map((provider) => provider === 'openai' ? 'codex' : provider === 'anthropic' ? 'claude' : provider)
    .filter(Boolean));
  const excluded = isRecord(excludedPayload) && isRecord(excludedPayload['oauth-excluded-models'])
    ? excludedPayload['oauth-excluded-models']
    : excludedPayload;
  return [...providers].flatMap((provider) => {
    const models = oauthModelsFromPayload(definitions[provider])
      .map((model) => ({ name: model.id, alias: model.displayName }));
    const rules = isRecord(excluded) && Array.isArray(excluded[provider])
      ? normalizeOAuthExcludedRules(excluded[provider].map(String))
      : [];
    return [{ kind: 'oauth' as const, id: `oauth:${provider}`, provider, label: `${provider} OAuth`, models, excludedRules: rules }];
  });
};

export const modelIsEnabled = (source: AvailableModelSource, model: ModelOption): boolean => {
  if (source.kind === 'copilot') {
    return !source.disabledModels.some((name) => name.toLowerCase() === model.name.toLowerCase());
  }
  if (source.kind === 'config' && source.section === 'openai-compatibility') {
    return source.enabledModels.includes(model.name.toLowerCase())
      && !source.disabled;
  }
  if (source.kind === 'config' && source.disabled) return false;
  if (source.kind === 'config' && source.section !== 'openai-compatibility'
    && !source.explicitAllowlist && !source.defaultCatalogKnown) return false;
  if (source.kind === 'config' && source.explicitAllowlist
    && !source.enabledModels.includes(model.name.toLowerCase())) return false;
  if (source.kind === 'config' && !source.explicitAllowlist
    && source.defaultCatalogKnown
    && !source.defaultModels.includes(model.name.toLowerCase())) return false;
  const routedName = routedModelName(source, model);
  return !source.excludedRules.some((rule) => modelMatchesRule(routedName, rule));
};

export const routedModelName = (source: AvailableModelSource, model: ModelOption): string => {
  const configuredAlias = source.kind === 'config'
    ? readString(source.modelRecords.find((record) =>
      readString(record, 'name', 'id', 'model', 'value').toLowerCase() === model.name.toLowerCase()
      && Boolean(readString(record, 'alias'))), 'alias')
    : '';
  return configuredAlias || model.name;
};

export const configRecordWithModelEnabled = (
  record: Record<string, unknown>,
  source: ConfigModelSource,
  modelName: string,
  enabled: boolean,
): Record<string, unknown> => {
  if (source.section === 'openai-compatibility') {
    const configured = Array.isArray(record.models) ? record.models.filter(isRecord) : [];
    const target = modelName.toLowerCase();
    if (enabled) {
      if (configured.some((model) => readString(model, 'name', 'id', 'model', 'value').toLowerCase() === target)) {
        return { ...record, models: configured };
      }
      const restored = source.modelRecords.filter((model) =>
        readString(model, 'name', 'id', 'model', 'value').toLowerCase() === target);
      return { ...record, models: [...configured, ...restored] };
    }
    return { ...record, models: configured.filter((model) =>
      readString(model, 'name', 'id', 'model', 'value').toLowerCase() !== target) };
  }
  const rules = source.excludedRules;
  const targetRecords = source.modelRecords.filter((candidate) =>
    readString(candidate, 'name', 'id', 'model', 'value').toLowerCase() === modelName.toLowerCase());
  const routedNames = [...new Set(targetRecords.map((candidate) =>
    readString(candidate, 'alias') || modelName))];
  if (routedNames.length === 0) routedNames.push(modelName);
  const nextRules = enabled
    ? rules.filter((rule) => rule.includes('*') || !routedNames.some((name) => rule.toLowerCase() === name.toLowerCase()))
    : normalizeOAuthExcludedRules([...rules, ...routedNames]);
  const next = { ...record };
  const enablingExtraDefaultSource = enabled
    && !source.explicitAllowlist
    && source.defaultCatalogKnown
    && !source.defaultModels.includes(modelName.toLowerCase());
  if (enabled && (source.explicitAllowlist || enablingExtraDefaultSource)) {
    const configured = Array.isArray(record.models) ? record.models.filter(isRecord) : [];
    const selected = [
      ...(enablingExtraDefaultSource ? source.defaultModelRecords : []),
      ...configured,
    ];
    if (!configured.some((candidate) =>
      readString(candidate, 'name', 'id', 'model', 'value').toLowerCase() === modelName.toLowerCase())) {
      selected.push(...targetRecords);
    }
    next.models = selected;
  }
  if (nextRules.length > 0) next['excluded-models'] = nextRules;
  else delete next['excluded-models'];
  return next;
};
