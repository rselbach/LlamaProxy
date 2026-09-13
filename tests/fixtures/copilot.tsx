import { createRoot } from 'react-dom/client';
import { mockIPC } from '@tauri-apps/api/mocks';
import { CopilotConnection } from '../../src/components/CopilotConnection';
import type { CopilotStatus } from '../../src/services/copilot';
import { I18nProvider } from '../../src/i18n';
import '../../src/styles.css';

declare global {
  interface Window {
    copilotFixture: {
      calls: { command: string; args: unknown }[];
      completePoll: (() => void) | null;
    };
  }
}

const params = new URLSearchParams(location.search);
localStorage.setItem('easy-cli-proxy-api.locale', params.get('locale') ?? 'en');
document.documentElement.dataset.theme = params.get('theme') ?? 'light';
window.copilotFixture = { calls: [], completePoll: null };
let status: CopilotStatus = { login: null, models: [], pending: null };
const connected: CopilotStatus = { login: 'troy-barnes', models: ['copilot/greendale'], pending: null };
mockIPC(async (command, args) => {
  window.copilotFixture.calls.push({ command, args });
  switch (command) {
    case 'set_app_locale':
    case 'open_oauth_url': return null;
    case 'get_copilot_status':
      return status;
    case 'start_copilot_login':
      status = { ...status, pending: { userCode: 'TROY-ABED', url: 'https://github.com/login/device', expiresIn: 900 } };
      return status;
    case 'poll_copilot_login':
      if (params.has('defer-poll')) {
        await new Promise<void>((resolve) => { window.copilotFixture.completePoll = resolve; });
        return connected;
      }
      if (params.has('denied')) {
        status = { ...status, pending: null };
        throw new Error('GitHub authorization was denied');
      }
      status = connected;
      return status;
    case 'cancel_copilot_login': status = { ...status, pending: null }; return status;
    case 'refresh_copilot_models':
      status = { ...connected, models: ['copilot/greendale', 'copilot/troy-and-abed'] }; return status;
    case 'disconnect_copilot': status = { login: null, models: [], pending: null }; return status;
    default: throw new Error(`Unhandled fixture command: ${command}`);
  }
});
const root = document.getElementById('root');
if (!root) throw new Error('Fixture root is missing');
createRoot(root).render(
  <I18nProvider>
    <main className="page management-page"><div className="oauth-grid"><CopilotConnection /></div></main>
  </I18nProvider>,
);
