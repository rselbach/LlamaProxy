import { createRoot } from 'react-dom/client';
import { mockIPC } from '@tauri-apps/api/mocks';
import { I18nProvider } from '../../src/i18n';
import { ModelsPage } from '../../src/pages/ModelsPage';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
localStorage.setItem('easy-cli-proxy-api.locale', params.get('locale') ?? 'en');
document.documentElement.dataset.theme = params.get('theme') ?? 'light';

let config: Record<string, unknown> = {
  'openai-compatibility': [{
    name: 'Greendale AI Lab',
    'base-url': 'https://ai.greendale.example/v1',
    'api-key-entries': [{ 'api-key': 'fixture-key', 'auth-index': '7' }],
    models: [{ name: 'troy-chat', displayName: 'Troy Chat' }],
  }],
  'claude-api-key': [{
    'api-key': 'fixture-claude-key',
    models: [
      { name: 'claude-abed', alias: 'abed-film' },
      { name: 'claude-annie' },
    ],
    'excluded-models': ['abed-film'],
  }],
};
let copilotDisabled = ['copilot/paintball-preview'];
let oauthExcluded = { codex: ['gpt-image-*'] };

mockIPC(async (command, args) => {
  if (command === 'set_app_locale') return null;
  if (command === 'get_copilot_status') return {
    login: 'troy-barnes',
    models: ['copilot/gpt-5.4', 'copilot/paintball-preview'],
    disabledModels: copilotDisabled,
    pending: null,
  };
  if (command === 'set_copilot_model_enabled') {
    const model = String((args as { model: string }).model);
    const enabled = Boolean((args as { enabled: boolean }).enabled);
    copilotDisabled = enabled ? copilotDisabled.filter((item) => item !== model) : [...copilotDisabled, model];
    return { login: 'troy-barnes', models: ['copilot/gpt-5.4', 'copilot/paintball-preview'], disabledModels: copilotDisabled, pending: null };
  }
  if (command !== 'management_request') throw new Error(`Unhandled fixture command: ${command}`);
  const request = (args as { request: { method: string; path: string; body?: unknown } }).request;
  if (request.method === 'GET' && request.path === '/config') return config;
  if (request.method === 'GET' && request.path === '/auth-files') return { files: [{ name: 'codex.json', type: 'codex' }] };
  if (request.method === 'GET' && request.path === '/oauth-excluded-models') return { 'oauth-excluded-models': oauthExcluded };
  if (request.method === 'GET' && request.path === '/model-definitions/codex') return { models: [{ id: 'gpt-5.4', display_name: 'GPT 5.4' }, { id: 'gpt-image-2' }] };
  if (request.method === 'GET' && request.path.startsWith('/model-definitions/')) return { models: [] };
  if (request.method === 'POST' && request.path === '/api-call') return { status_code: 200, body: { data: [{ id: 'troy-chat' }, { id: 'pierce-legacy' }] } };
  if (request.method === 'PUT') {
    config = { ...config, [request.path.slice(1)]: request.body };
    return { status: 'ok' };
  }
  if (request.method === 'PATCH' && request.path === '/oauth-excluded-models') {
    const body = request.body as { provider: string; models: string[] };
    oauthExcluded = { ...oauthExcluded, [body.provider]: body.models };
    return { status: 'ok' };
  }
  throw new Error(`Unhandled fixture request: ${request.method} ${request.path}`);
});

const root = document.getElementById('root');
if (!root) throw new Error('Fixture root is missing');
createRoot(root).render(<I18nProvider><ModelsPage /></I18nProvider>);
