import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useConfirmation } from './ConfirmationDialog';
import { Bot, Copy, ExternalLink, LoaderCircle, LogIn, RefreshCw } from 'lucide-react';
import { useI18n } from '../i18n';
import { copilotCommand, type CopilotCommand, type CopilotStatus } from '../services/copilot';

export function CopilotConnection({ browser = 'default' }: {
  browser?: string;
}) {
  const { t } = useI18n();
  const { askConfirmation, confirmationDialog } = useConfirmation();
  const [status, setStatus] = useState<CopilotStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const generation = useRef(0);
  const mounted = useRef(false);

  useEffect(() => {
    mounted.current = true;
    const epoch = generation.current;
    void copilotCommand('get_copilot_status').then((value) => {
      if (mounted.current && generation.current === epoch) setStatus(value);
    }).catch((error: unknown) => {
      if (mounted.current && generation.current === epoch) setError(String(error));
    });
    return () => { mounted.current = false; ++generation.current; };
  }, []);

  const deviceCode = status?.pending?.userCode;
  useEffect(() => {
    if (!deviceCode || busy) return;
    let active = true;
    let timer: ReturnType<typeof setTimeout>;
    const epoch = generation.current;
    const poll = async () => {
      try {
        const next = await copilotCommand('poll_copilot_login');
        if (!active || generation.current !== epoch) return;
        setStatus(next);
        setError('');
        if (next.pending === null) return;
      } catch (error) {
        if (!active || generation.current !== epoch) return;
        setError(String(error));
        try {
          const next = await copilotCommand('get_copilot_status');
          if (!active || generation.current !== epoch) return;
          setStatus(next);
          if (next.pending === null) return;
        } catch (statusError) {
          if (active) setError(`${String(error)}; ${String(statusError)}`);
        }
      }
      if (active) timer = setTimeout(() => void poll(), 5000);
    };
    timer = setTimeout(() => void poll(), 5000);
    return () => { active = false; clearTimeout(timer); };
  }, [deviceCode, busy]);

  const run = useCallback(async (command: CopilotCommand) => {
    const epoch = ++generation.current;
    setBusy(true);
    setError('');
    try {
      const next = await copilotCommand(command);
      if (mounted.current && generation.current === epoch) setStatus(next);
    } catch (error) {
      if (mounted.current && generation.current === epoch) setError(String(error));
    } finally {
      if (mounted.current && generation.current === epoch) setBusy(false);
    }
  }, []);

  const openLink = async () => {
    if (!status?.pending) return;
    try {
      await invoke('open_oauth_url', { url: status.pending.url, browser: browser === 'none' ? 'default' : browser });
    } catch (error) { setError(String(error)); }
  };

  const copyCode = async () => {
    if (!status?.pending) return;
    try { await navigator.clipboard.writeText(status.pending.userCode); }
    catch (error) { setError(String(error)); }
  };

  const disconnect = async () => {
    try {
      if (await askConfirmation({ title: t('copilot.title'), message: t('copilot.disconnectConfirm'),
        confirmText: t('copilot.disconnect'), variant: 'danger' })) {
        await run('disconnect_copilot');
      }
    } catch (error) { setError(String(error)); }
  };

  return (
    <section className="panel oauth-card">
      {confirmationDialog}
      <div className="provider-title-row">
        <Bot size={32} aria-hidden="true" />
        <div>
          <h2>{t('copilot.title')}</h2>
          {status?.login ? <span className="state-pill success">{status.login}</span> : null}
        </div>
      </div>
      <div className="oauth-card-body">
        <p className="oauth-hint">{t('copilot.description')}</p>
        {status?.login ? <p role="status">{t('copilot.connected', { count: status.models.length })}</p> : null}
        {status?.pending ? (
          <div className="oauth-auth-url-box" aria-live="polite">
            <div className="oauth-auth-url-label">{t('copilot.deviceCode')}</div>
            <div className="oauth-auth-url-value"><strong>{status.pending.userCode}</strong></div>
            <p className="oauth-hint">{t('copilot.deviceHint')}</p>
            <div className="oauth-auth-url-actions">
              <button type="button" className="secondary-button compact-button" onClick={() => void copyCode()}>
                <Copy size={15} aria-hidden="true" />{t('copilot.copyCode')}
              </button>
              <button type="button" className="secondary-button compact-button" onClick={() => void openLink()}>
                <ExternalLink size={15} aria-hidden="true" />{t('oauth.openLink')}
              </button>
            </div>
          </div>
        ) : null}
        {error ? <div className="oauth-inline-status error" role="alert">{error}</div> : null}
      </div>
      <div className="button-row management-card-actions">
        {status?.pending ? (
          <button type="button" className="secondary-button" disabled={busy} onClick={() => void run('cancel_copilot_login')}>
            {t('common.cancel')}
          </button>
        ) : (
          <button type="button" className="primary-button" disabled={busy} onClick={() => void run('start_copilot_login')}>
            {busy ? <LoaderCircle size={16} className="spin" aria-hidden="true" /> : <LogIn size={16} aria-hidden="true" />}
            {status?.login ? t('copilot.replaceAccount') : t('oauth.startLogin')}
          </button>
        )}
        {status?.login ? <>
          <button type="button" className="secondary-button" disabled={busy || Boolean(status.pending)} onClick={() => void run('refresh_copilot_models')}>
            <RefreshCw size={16} aria-hidden="true" />{t('copilot.refreshModels')}
          </button>
          <button type="button" className="secondary-button" disabled={busy} onClick={() => void disconnect()}>
            {t('copilot.disconnect')}
          </button>
        </> : null}
      </div>
    </section>
  );
}
