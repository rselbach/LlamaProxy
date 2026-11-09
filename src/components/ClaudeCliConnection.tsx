import { useCallback, useEffect, useRef, useState } from 'react';
import { AlertTriangle, LoaderCircle, LogIn, Terminal } from 'lucide-react';
import { useI18n } from '../i18n';
import { claudeCliCommand, type ClaudeCliCommand, type ClaudeCliStatus } from '../services/claudeCli';

export function ClaudeCliConnection() {
  const { t } = useI18n();
  const [status, setStatus] = useState<ClaudeCliStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const mounted = useRef(false);

  const run = useCallback(async (command: ClaudeCliCommand) => {
    setBusy(true);
    setError('');
    try {
      const next = await claudeCliCommand(command);
      if (mounted.current) setStatus(next);
    } catch (error) {
      if (mounted.current) setError(String(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void run('get_claude_cli_status');
    return () => { mounted.current = false; };
  }, [run]);

  return (
    <section className="panel oauth-card">
      <div className="provider-title-row">
        <Terminal size={32} aria-hidden="true" />
        <div>
          <h2>{t('claudeCli.title')}</h2>
          {status?.account ? <span className="state-pill success">{status.account}</span> : null}
        </div>
      </div>
      <div className="oauth-card-body">
        <p className="oauth-hint">{t('claudeCli.description')}</p>
        <p className="oauth-hint"><AlertTriangle size={14} aria-hidden="true" /> {t('claudeCli.risk')}</p>
        {status?.account ? <p role="status">{t('claudeCli.connected', { count: status.models.length })}</p> : null}
        {status && !status.installed ? <p role="status">{t('claudeCli.notInstalled')}</p> : null}
        {error ? <div className="oauth-inline-status error" role="alert">{error}</div> : null}
      </div>
      <div className="button-row management-card-actions">
        {status?.account ? (
          <button type="button" className="secondary-button" disabled={busy} onClick={() => void run('disconnect_claude_cli')}>
            {t('claudeCli.disconnect')}
          </button>
        ) : (
          <button type="button" className="primary-button" disabled={busy || !status?.installed} onClick={() => void run('connect_claude_cli')}>
            {busy ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : <LogIn size={16} aria-hidden="true" />}
            {t('claudeCli.connect')}
          </button>
        )}
      </div>
    </section>
  );
}
