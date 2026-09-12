import { afterEach, beforeEach, describe, expect, it } from 'bun:test';
import { clearMocks, mockIPC } from '@tauri-apps/api/mocks';
import { renderToStaticMarkup } from 'react-dom/server';
import { AppErrorBoundary } from '../src/AppErrorBoundary';
import { useCoreRuntime } from '../src/coreRuntime';
import { useCoreUpdate } from '../src/coreUpdate';
import { I18nProvider, translate, type AppLocale } from '../src/i18n';
import {
  isCoreInstallCancellation, localizeInstallMessage, localizeInstallPhase,
} from '../src/pages/VersionManagementPage';
import { checkProviderHealthProbe } from '../src/services/providerHealthCheck';

let originalWindow: PropertyDescriptor | undefined;
let locale: AppLocale;
beforeEach(() => {
  originalWindow = Object.getOwnPropertyDescriptor(globalThis, 'window');
  locale = 'en';
  Object.defineProperty(globalThis, 'window', {
    value: { localStorage: { getItem: () => locale }, navigator: { language: 'en' } },
    configurable: true, writable: true,
  });
});
afterEach(() => {
  clearMocks();
  if (originalWindow) Object.defineProperty(globalThis, 'window', originalWindow);
  else Reflect.deleteProperty(globalThis, 'window');
});
const t = (key: Parameters<typeof translate>[1], variables?: Parameters<typeof translate>[2]) =>
  translate(locale, key, variables);

describe('English presentation of native task state', () => {
  it('localizes every native install phase without changing its protocol identifier', () => {
    for (const phase of [
      '空闲', '检查版本', '校验内置内核', '解压内置内核', '准备下载', '下载中', '解压中',
      '准备内置内核', '安装完成', '安装失败', '已取消', '准备安装最新版', '准备安装 v1.2.3',
      '下载失败，正在切换到 GitHub', '下载失败，正在切换到 Custom mirror',
    ]) {
      expect(localizeInstallPhase(phase, t)).not.toMatch(/\p{Script=Han}/u);
    }
    expect(localizeInstallPhase('准备安装 v1.2.3', t)).toBe('Installing v1.2.3');
    locale = 'zh-CN';
    expect(localizeInstallPhase('下载中', t)).toBe(translate(locale, 'kernel.phase.downloading'));
    expect(localizeInstallPhase('下载失败，正在切换到 Custom mirror', t))
      .toContain(translate(locale, 'kernel.versions.source.custom'));
  });

  it('recognizes English and legacy cancellation diagnostics, but not install failures', () => {
    for (const message of ['Download cancelled', 'Download canceled', '已取消下载', 'The app is exiting; core operation cancelled']) {
      expect(isCoreInstallCancellation(message)).toBe(true);
    }
    expect(isCoreInstallCancellation('SHA-256 verification failed')).toBe(false);
    expect(isCoreInstallCancellation('Failed to write configuration')).toBe(false);
  });

  it('uses localized task success/cancellation messages while preserving detailed errors and user data', () => {
    const result = { version: 'v1.2.3', assetName: 'core.tar.gz', installDir: '/tmp/core', binaryPath: null };
    expect(localizeInstallMessage({ message: 'v1.2.3 安装完成', result }, t)).toBe('v1.2.3 installed');
    expect(localizeInstallMessage({ message: 'Download cancelled', result: null }, t)).toBe('Canceled');
    expect(localizeInstallMessage({ message: '已取消下载', result: null }, t)).toBe('Canceled');
    const error = 'Download cancelled; failed to automatically restore the original core running state: port busy';
    expect(localizeInstallMessage({ message: error, result: null }, t)).toBe(error);
    const upstream = 'Provider response: 用户数据';
    expect(localizeInstallMessage({ message: upstream, result: null }, t)).toBe(upstream);
    locale = 'zh-CN';
    expect(localizeInstallMessage({ message: 'v1.2.3 installation completed', result }, t))
      .toBe(translate(locale, 'kernel.install.completed', { version: 'v1.2.3' }));
  });
});

describe('health check timeout compatibility (mocked IPC)', () => {
  it('classifies English native and legacy Chinese timeout responses without rewriting them', async () => {
    for (const [message, timedOut] of [
      ['Health check request failed: operation timed out', true],
      ['deadline has elapsed', true],
      ['健康检测请求超时', true],
      ['Upstream returned HTTP 401', false],
    ] as const) {
      mockIPC(() => { throw new Error(message); });
      const result = await checkProviderHealthProbe('openai', 'https://example.com', 'test-model', 'test-key');
      expect(result).toMatchObject({ success: false, timedOut, error: message });
    }
  });
});

describe('AppErrorBoundary English fallback', () => {
  it('renders translated fallback text and keeps useful error details', () => {
    const renderError = (error: Error) => {
      const boundary = new AppErrorBoundary({ children: null });
      boundary.state = AppErrorBoundary.getDerivedStateFromError(error);
      return renderToStaticMarkup(<I18nProvider>{boundary.render()}</I18nProvider>);
    };
    const markup = renderError(new Error('Failed to read core configuration'));
    expect(markup).toContain(translate('en', 'error.render.title'));
    expect(markup).toContain('Failed to read core configuration');
    expect(markup).not.toMatch(/\p{Script=Han}/u);
    expect(renderError(new Error(''))).toContain(translate('en', 'error.unknown'));
    locale = 'zh-CN';
    expect(renderError(new Error(''))).toContain(translate(locale, 'error.render.title'));
  });

  it('reports missing runtime providers in English', () => {
    for (const hook of [useCoreRuntime, useCoreUpdate]) {
      function MissingProvider() { hook(); return null; }
      expect(() => renderToStaticMarkup(<MissingProvider />)).toThrow(/must be used inside/);
    }
  });
});
