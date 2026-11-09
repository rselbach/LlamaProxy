import { createRoot } from 'react-dom/client';
import { mockIPC } from '@tauri-apps/api/mocks';
import { ClaudeCliConnection } from '../../src/components/ClaudeCliConnection';
import type { ClaudeCliStatus } from '../../src/services/claudeCli';
import { I18nProvider } from '../../src/i18n';
import '../../src/styles.css';

const params = new URLSearchParams(location.search);
localStorage.setItem('easy-cli-proxy-api.locale', params.get('locale') ?? 'en');
document.documentElement.dataset.theme = params.get('theme') ?? 'light';
const disconnected: ClaudeCliStatus = { installed: !params.has('missing'), account: null, models: [], disabledModels: [] };
let status = disconnected;
mockIPC(async (command) => {
  switch (command) {
    case 'set_app_locale': return null;
    case 'get_claude_cli_status': return status;
    case 'connect_claude_cli':
      if (params.has('signed-out')) throw new Error('Claude Code is not signed in. Run `claude auth login` with your Claude subscription');
      status = { installed: true, account: 'annie@greendale.edu', models: ['claude-cli/opus', 'claude-cli/sonnet'], disabledModels: [] };
      return status;
    case 'disconnect_claude_cli': status = disconnected; return status;
    default: throw new Error(`Unhandled fixture command: ${command}`);
  }
});
const root = document.getElementById('root');
if (!root) throw new Error('Fixture root is missing');
createRoot(root).render(
  <I18nProvider>
    <main className="page management-page"><div className="oauth-grid"><ClaudeCliConnection /></div></main>
  </I18nProvider>,
);
