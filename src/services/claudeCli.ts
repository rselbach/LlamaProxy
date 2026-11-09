import { invoke } from '@tauri-apps/api/core';

export type ClaudeCliStatus = {
  installed: boolean;
  account: string | null;
  models: string[];
  disabledModels: string[];
};

export type ClaudeCliCommand = 'get_claude_cli_status' | 'connect_claude_cli' | 'disconnect_claude_cli';

export function parseClaudeCliStatus(value: unknown): ClaudeCliStatus {
  if (typeof value !== 'object' || value === null
    || !('installed' in value) || typeof value.installed !== 'boolean'
    || !('account' in value) || !(value.account === null || typeof value.account === 'string')
    || !('models' in value) || !Array.isArray(value.models)
    || !value.models.every((model): model is string => typeof model === 'string')
    || !('disabledModels' in value) || !Array.isArray(value.disabledModels)
    || !value.disabledModels.every((model): model is string => typeof model === 'string')) {
    throw new Error('Invalid Claude CLI status');
  }
  return { installed: value.installed, account: value.account, models: value.models, disabledModels: value.disabledModels };
}

export async function claudeCliCommand(command: ClaudeCliCommand): Promise<ClaudeCliStatus> {
  return parseClaudeCliStatus(await invoke<unknown>(command));
}

export async function setClaudeCliModelEnabled(model: string, enabled: boolean): Promise<ClaudeCliStatus> {
  return parseClaudeCliStatus(await invoke<unknown>('set_claude_cli_model_enabled', { model, enabled }));
}
