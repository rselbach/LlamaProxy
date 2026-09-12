import { describe, expect, it } from 'bun:test';
import { readFileSync, readdirSync } from 'node:fs';
import { join, relative } from 'node:path';

const root = new URL('../src-tauri/src/', import.meta.url).pathname;
const han = /\p{Script=Han}/u;

function rustFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.name === 'tests' || entry.name === 'tests.rs') return [];
    return entry.isDirectory() ? rustFiles(path) : entry.name.endsWith('.rs') ? [path] : [];
  });
}

// Only these wire phase identifiers and legacy classifiers may contain Han in production.
const phaseLiterals: Record<string, string[]> = {
  'main.rs': ['空闲', '准备安装 {version}', '准备安装最新版', '安装完成', '已取消', '安装失败', '取消'],
  'core_runtime.rs': ['检查版本', '解压中', '校验内置内核', '解压内置内核', '准备下载', '下载中', '下载失败，正在切换到 {}'],
};

function productionSource(path: string, source: string): string {
  // Inline test modules and two cfg(test)-only helpers contain intentional Unicode data.
  source = source.replace(/#\[cfg\(test\)\]\s*mod \w+\s*\{[\s\S]*$/, '');
  if (path === 'codex_catalog.rs') {
    source = source.replace(/#\[cfg\(test\)\]\s*fn parse_combined_sources_for_test[\s\S]*?\n\}/, '');
  }
  if (path === 'agents/backups.rs') {
    source = source.replace(/#\[cfg\(test\)\]\s*\{[\s\S]*?\n    \}/, '');
  }
  // Native tray dictionaries are deliberately locale-aware, not untranslated diagnostics.
  if (path === 'main.rs') source = source.replace(/fn locale_text[^\n]*\{[\s\S]*?\n\}/, '');
  if (path === 'tray.rs') {
    source = source.replace(/locale_text\(\s*[^,]+,\s*"[^"]*",\s*"[^"]*",?\s*\)/g, '');
    source = source.replace(/(?:"zh-TW"|"ja"|_) => format!\("[^"]*\{summary\}"\),/g, '');
  }
  // Match comments as tokens so URL strings are not mistaken for line comments.
  source = source.replace(/"(?:[^"\\]|\\.)*"|\/\/[^\n]*|\/\*[\s\S]*?\*\//g,
    (token) => token.startsWith('/') ? '' : token);
  for (const literal of phaseLiterals[path] ?? []) source = source.replaceAll(`"${literal}"`, '"phase"');
  return source;
}

describe('native English diagnostics', () => {
  it('allows only native locales, phase protocol, comments and test fixtures to retain Han', () => {
    const untranslated = rustFiles(root).flatMap((file) => {
      const path = relative(root, file);
      return productionSource(path, readFileSync(file, 'utf8')).split('\n')
        .filter((line) => han.test(line)).map((line) => `${path}: ${line.trim()}`);
    });
    expect(untranslated).toEqual([]);
  });

  it('would reject the original management error and collector status, but preserves locale resources', () => {
    expect(han.test(productionSource('management_api.rs', 'Err("不支持的管理 API 请求方法")'))).toBe(true);
    expect(han.test(productionSource('usage.rs', 'message: "等待内核启动"'))).toBe(true);
    expect(han.test(productionSource('tray.rs', 'locale_text(locale, "启动内核", "Start Core")'))).toBe(false);
    expect(han.test(productionSource('core_runtime.rs', 'state.progress(window, "检查版本", 0, None, true);'))).toBe(false);
  });
});
