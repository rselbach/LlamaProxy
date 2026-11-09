const MANAGED_ADAPTER_PATHS = new Set(['/llamaproxy-copilot', '/llamaproxy-claude-cli']);

// Routes LlamaProxy writes for its own loopback adapters. Their port and key
// change on every launch, so they are never edited by hand.
export function isManagedRecord(record: Record<string, unknown>): boolean {
  const baseUrl = record['base-url'];
  if (typeof baseUrl !== 'string') return false;
  try {
    const url = new URL(baseUrl);
    return url.protocol === 'http:' && url.hostname === '127.0.0.1'
      && MANAGED_ADAPTER_PATHS.has(url.pathname);
  } catch {
    return false;
  }
}
