export type AgentLaunchDirectoryHistory = Record<string, string[]>;

export const MAX_AGENT_LAUNCH_DIRECTORY_HISTORY = 8;

const validDirectory = (value: unknown): value is string => (
  typeof value === 'string'
  && Boolean(value.trim())
  && value.length <= 4096
  && !Array.from(value).some((character) => /[\u0000-\u001f\u007f]/u.test(character))
);

export const parseAgentLaunchDirectoryHistory = (
  payload: string | null,
  clients: readonly string[],
): AgentLaunchDirectoryHistory => {
  if (!payload) return {};
  try {
    const parsed = JSON.parse(payload) as unknown;
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    const source = parsed as Record<string, unknown>;
    return clients.reduce<AgentLaunchDirectoryHistory>((history, client) => {
      const entries = source[client];
      if (!Array.isArray(entries)) return history;
      const normalized = entries
        .filter(validDirectory)
        .map((directory) => directory.trim())
        .filter((directory, index, directories) => directories.indexOf(directory) === index)
        .slice(0, MAX_AGENT_LAUNCH_DIRECTORY_HISTORY);
      if (normalized.length) history[client] = normalized;
      return history;
    }, {});
  } catch {
    return {};
  }
};

export const rememberAgentLaunchDirectory = (
  history: AgentLaunchDirectoryHistory,
  client: string,
  directory: string,
): AgentLaunchDirectoryHistory => {
  const normalized = directory.trim();
  if (!validDirectory(normalized)) return history;
  const previous = history[client] ?? [];
  return {
    ...history,
    [client]: [normalized, ...previous.filter((item) => item !== normalized)]
      .slice(0, MAX_AGENT_LAUNCH_DIRECTORY_HISTORY),
  };
};
