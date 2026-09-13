import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { RefreshCw, Search } from 'lucide-react';
import { useI18n } from '../i18n';
import { copilotCommand, setCopilotModelEnabled } from '../services/copilot';
import {
  configRecordWithModelEnabled,
  configuredModelSources,
  modelIsEnabled,
  mergeModelCatalogRecords,
  nativeDefaultCatalogsFromDefinitions,
  oauthModelSources,
  readSavedModelCatalogs,
  routedModelName,
  saveModelCatalog,
  type AvailableModelSource,
} from '../services/availableModels';
import { isRecord, managementApi, responseList } from '../services/managementApi';
import { modelMatchesRule, normalizeOAuthExcludedRules } from '../services/oauthModels';
import { fetchModels, type ModelProvider } from '../services/modelService';

const oauthProviders = ['codex', 'claude', 'gemini', 'antigravity', 'kimi', 'xai'];

const removeResponseFields = (value: Record<string, unknown>) => {
  const next = { ...value };
  delete next['auth-index'];
  delete next.authIndex;
  if (Array.isArray(next['api-key-entries'])) {
    next['api-key-entries'] = next['api-key-entries'].map((entry) => {
      if (!isRecord(entry)) return entry;
      const clean = { ...entry };
      delete clean['auth-index'];
      delete clean.authIndex;
      return clean;
    });
  }
  return next;
};

export function ModelsPage() {
  const { t } = useI18n();
  const [sources, setSources] = useState<AvailableModelSource[]>([]);
  const [filter, setFilter] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState('');
  const [error, setError] = useState('');
  const defaultCatalogsRef = useRef(nativeDefaultCatalogsFromDefinitions({}));

  const load = useCallback(async (discover = true) => {
    if (discover) setLoading(true);
    setError('');
    try {
      const [config, authFiles, exclusions] = await Promise.all([
        managementApi.get('/config'),
        managementApi.get('/auth-files'),
        managementApi.get('/oauth-excluded-models'),
      ]);
      const copilotResult = await copilotCommand('get_copilot_status')
        .then((status) => ({ status, error: '' }))
        .catch((copilotError: unknown) => ({ status: null, error: String(copilotError) }));
      const definitionErrors: Record<string, string> = {};
      const definitions = Object.fromEntries(await Promise.all(oauthProviders.map(async (provider) => {
        try {
          return [provider, await managementApi.get(`/model-definitions/${encodeURIComponent(provider)}`)];
        } catch (definitionError) {
          definitionErrors[provider] = String(definitionError);
          return [provider, null];
        }
      })));
      const savedCatalogs = readSavedModelCatalogs();
      const defaultCatalogs = nativeDefaultCatalogsFromDefinitions(definitions);
      defaultCatalogsRef.current = defaultCatalogs;
      const initialConfigured = configuredModelSources(config, savedCatalogs, defaultCatalogs);
      const discoveryErrors: string[] = [];
      if (discover) await Promise.all(initialConfigured.map(async (source) => {
        const provider: ModelProvider = source.section === 'gemini-api-key'
          ? 'gemini'
          : source.section === 'claude-api-key'
            ? 'claude'
            : source.section === 'codex-api-key'
              ? 'codex'
              : 'openai';
        try {
          const discovered = await fetchModels(
            provider,
            source.connection.baseUrl,
            source.connection.apiKey,
            source.connection.authIndex,
            source.connection.headers,
          );
          const discoveredRecords = discovered.map((model) => ({
            name: model.name,
            ...(model.alias ? { displayName: model.alias } : {}),
          }));
          savedCatalogs[source.id] = mergeModelCatalogRecords(
            discoveredRecords,
            savedCatalogs[source.id] ?? [],
          );
        } catch (discoveryError) {
          discoveryErrors.push(`${source.label}: ${String(discoveryError)}`);
        }
      }));
      const configured = configuredModelSources(config, savedCatalogs, defaultCatalogs);
      configured.forEach((source) => saveModelCatalog(source));
      const oauth = oauthModelSources(authFiles, definitions, exclusions);
      const copilot = copilotResult.status;
      const copilotSource: AvailableModelSource[] = copilot?.login ? [{
        kind: 'copilot',
        id: 'copilot',
        label: 'GitHub Copilot',
        models: copilot.models.map((name) => ({ name })),
        disabledModels: copilot.disabledModels,
      }] : [];
      setSources([...oauth, ...copilotSource, ...configured]);
      const providerErrors = [
        ...oauth.flatMap((source) => definitionErrors[source.provider]
          ? [`${source.label}: ${definitionErrors[source.provider]}`]
          : []),
        ...discoveryErrors,
        ...configured.flatMap((source) => {
          const provider = source.section === 'codex-api-key'
            ? 'codex'
            : source.section === 'claude-api-key'
              ? 'claude'
              : source.section === 'gemini-api-key'
                ? 'gemini'
                : '';
          return provider && definitionErrors[provider]
            ? [`${source.label}: ${definitionErrors[provider]}`]
            : [];
        }),
        ...(copilotResult.error ? [`GitHub Copilot: ${copilotResult.error}`] : []),
      ];
      if (providerErrors.length > 0) setError(providerErrors.join('\n'));
    } catch (loadError) {
      setError(String(loadError));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { void load(); }, [load]);

  const visibleRows = useMemo(() => {
    const query = filter.trim().toLowerCase();
    return sources.flatMap((source) => source.models.flatMap((model) => {
      const visible = !query
        || (
        model.name.toLowerCase().includes(query)
        || model.alias?.toLowerCase().includes(query)
        || source.label.toLowerCase().includes(query)
        );
      return visible ? [{ source, model }] : [];
    })).sort((left, right) => (
      left.model.name.localeCompare(right.model.name)
      || left.source.label.localeCompare(right.source.label)
    ));
  }, [filter, sources]);

  const toggle = async (source: AvailableModelSource, modelName: string, enabled: boolean) => {
    const action = `${source.id}:${modelName}`;
    setBusy(action);
    setError('');
    try {
      if (source.kind === 'copilot') {
        await setCopilotModelEnabled(modelName, enabled);
      } else if (source.kind === 'oauth') {
        const rules = enabled
          ? source.excludedRules.filter((rule) => rule.includes('*') || rule !== modelName.toLowerCase())
          : normalizeOAuthExcludedRules([...source.excludedRules, modelName]);
        if (rules.length > 0) {
          await managementApi.patch('/oauth-excluded-models', { provider: source.provider, models: rules });
        } else {
          await managementApi.delete('/oauth-excluded-models', { query: { provider: source.provider } });
        }
      } else {
        const config = await managementApi.get('/config');
        const records = responseList(config, source.section);
        const matches = configuredModelSources(
          config,
          readSavedModelCatalogs(),
          defaultCatalogsRef.current,
        )
          .filter((candidate) => candidate.id === source.id);
        if (matches.length !== 1) throw new Error(t('models.error.stale'));
        const latestSource = matches[0];
        const record = records[latestSource.index];
        if (!record) throw new Error(t('models.error.stale'));
        const next = configRecordWithModelEnabled(record, latestSource, modelName, enabled);
        await managementApi.put(`/${source.section}`, records.map((item, index) =>
          removeResponseFields(index === latestSource.index ? next : item)));
      }
      await load(false);
    } catch (toggleError) {
      setError(String(toggleError));
    } finally {
      setBusy('');
    }
  };

  const total = sources.reduce((count, source) => count + source.models.length, 0);
  return (
    <section className="page management-page models-page">
      <header className="management-header">
        <div><h1>{t('models.title')}</h1><p>{t('models.description')}</p></div>
        <div className="management-heading-actions">
          <span className="muted-summary">{t('models.count', { count: total })}</span>
          <button type="button" className="secondary-button compact-button" disabled={loading || Boolean(busy)} onClick={() => void load(true)}>
            <RefreshCw size={16} aria-hidden="true" />{t('common.refresh')}
          </button>
        </div>
      </header>
      <label className="models-search"><Search size={16} aria-hidden="true" /><input value={filter} onChange={(event) => setFilter(event.target.value)} placeholder={t('models.search')} aria-label={t('models.search')} /></label>
      {error ? <div className="management-alert error" role="alert">{error}</div> : null}
      {loading ? <div className="panel empty-state">{t('common.loading')}</div> : null}
      {!loading && visibleRows.length === 0 ? <div className="panel empty-state">{t('models.empty')}</div> : null}
      {visibleRows.length > 0 ? (
        <div className="panel models-table-panel">
          <table className="models-table">
            <thead><tr><th scope="col">{t('models.column.model')}</th><th scope="col">{t('models.column.provider')}</th><th scope="col">{t('models.column.enabled')}</th></tr></thead>
            <tbody>
              {visibleRows.map(({ source, model }) => {
                const enabled = modelIsEnabled(source, model);
                const wildcard = source.kind !== 'copilot' && source.excludedRules.some((rule) =>
                  rule.includes('*') && modelMatchesRule(routedModelName(source, model), rule));
                const providerDisabled = source.kind === 'config' && source.disabled;
                const defaultsUnknown = source.kind === 'config'
                  && source.section !== 'openai-compatibility'
                  && !source.explicitAllowlist
                  && !source.defaultCatalogKnown;
                const action = `${source.id}:${model.name}`;
                const title = wildcard ? t('models.wildcardDisabled') : providerDisabled ? t('models.providerDisabled') : defaultsUnknown ? t('models.defaultsUnknown') : undefined;
                return <tr key={`${source.id}:${source.kind === 'config' ? source.index : ''}:${model.name}`} title={title}>
                  <td><span className="models-model-name"><strong>{model.alias || model.name}</strong>{model.alias ? <code>{model.name}</code> : null}</span></td>
                  <td><span className="models-provider-name"><strong>{source.label}</strong></span></td>
                  <td><label className="switch-control"><input type="checkbox" checked={enabled} disabled={Boolean(busy) || wildcard || providerDisabled || defaultsUnknown} onChange={(event) => void toggle(source, model.name, event.target.checked)} aria-label={t('models.toggle', { model: model.name })} /><span className="switch-track" />{busy === action ? <small>{t('common.saving')}</small> : null}</label></td>
                </tr>;
              })}
            </tbody>
          </table>
        </div>
      ) : null}
    </section>
  );
}
