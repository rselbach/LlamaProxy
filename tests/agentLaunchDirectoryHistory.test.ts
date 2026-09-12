import { describe, expect, it } from 'bun:test';
import {
  MAX_AGENT_LAUNCH_DIRECTORY_HISTORY,
  parseAgentLaunchDirectoryHistory,
  rememberAgentLaunchDirectory,
} from '../src/services/agentLaunchDirectoryHistory';

describe('agent launch directory history', () => {
  it('keeps only valid known-client entries', () => {
    const history = parseAgentLaunchDirectoryHistory(JSON.stringify({
      codex: [' C:\\work\\one ', '', 'C:\\work\\one', 42],
      unknown: ['C:\\private'],
    }), ['codex', 'claude-code']);
    expect(history).toEqual({ codex: ['C:\\work\\one'] });
  });

  it('moves the successful directory to the front and limits history size', () => {
    const existing = Array.from(
      { length: MAX_AGENT_LAUNCH_DIRECTORY_HISTORY },
      (_, index) => `/work/${index}`,
    );
    const added = rememberAgentLaunchDirectory({ codex: existing }, 'codex', '/work/new');
    expect(added.codex[0]).toBe('/work/new');
    expect(added.codex).toHaveLength(MAX_AGENT_LAUNCH_DIRECTORY_HISTORY);

    const reused = rememberAgentLaunchDirectory(added, 'codex', '/work/2');
    expect(reused.codex[0]).toBe('/work/2');
    expect(reused.codex.filter((directory) => directory === '/work/2')).toHaveLength(1);
  });

  it('falls back to an empty history for malformed storage', () => {
    expect(parseAgentLaunchDirectoryHistory('{broken', ['codex'])).toEqual({});
  });
});
