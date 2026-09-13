import { invoke } from '@tauri-apps/api/core';

export type CopilotStatus = {
  login: string | null;
  models: string[];
  pending: { userCode: string; url: string; expiresIn: number } | null;
};

export type CopilotCommand = 'get_copilot_status' | 'start_copilot_login'
  | 'poll_copilot_login' | 'cancel_copilot_login' | 'refresh_copilot_models'
  | 'disconnect_copilot';

export function parseCopilotStatus(value: unknown): CopilotStatus {
  if (typeof value !== 'object' || value === null
    || !('login' in value) || !(value.login === null || typeof value.login === 'string')
    || !('models' in value) || !Array.isArray(value.models)
    || !value.models.every((model): model is string => typeof model === 'string')
    || !('pending' in value)) {
    throw new Error('Invalid Copilot status');
  }
  let pending: CopilotStatus['pending'] = null;
  if (value.pending !== null) {
    const login = value.pending;
    if (typeof login !== 'object' || login === null
      || !('userCode' in login) || typeof login.userCode !== 'string'
      || !('url' in login) || login.url !== 'https://github.com/login/device'
      || !('expiresIn' in login) || typeof login.expiresIn !== 'number'
      || !Number.isFinite(login.expiresIn) || login.expiresIn < 0) {
      throw new Error('Invalid Copilot device sign-in');
    }
    pending = { userCode: login.userCode, url: login.url, expiresIn: login.expiresIn };
  }
  return { login: value.login, models: value.models, pending };
}

export async function copilotCommand(command: CopilotCommand): Promise<CopilotStatus> {
  return parseCopilotStatus(await invoke<unknown>(command));
}

export function isManagedCopilotRecord(record: Record<string, unknown>): boolean {
  const baseUrl = record['base-url'];
  if (typeof baseUrl !== 'string') return false;
  try {
    const url = new URL(baseUrl);
    return url.protocol === 'http:' && url.hostname === '127.0.0.1'
      && url.pathname === '/llamaproxy-copilot';
  } catch {
    return false;
  }
}
