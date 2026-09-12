import { describe, expect, test } from 'bun:test';
import {
  createOAuthLoginSuccessCache,
  shouldShowOAuthLoginStatus,
} from '../src/services/oauthLoginState';

describe('OAuth login display state', () => {
  test('only successful login states display a status badge', () => {
    expect(shouldShowOAuthLoginStatus('idle')).toBeFalse();
    expect(shouldShowOAuthLoginStatus('waiting')).toBeFalse();
    expect(shouldShowOAuthLoginStatus('error')).toBeFalse();
    expect(shouldShowOAuthLoginStatus('success')).toBeTrue();
  });

  test('successful providers remain cached across page remounts', () => {
    const cache = createOAuthLoginSuccessCache<'codex' | 'claude'>();
    cache.mark('codex');

    expect(cache.snapshot()).toEqual(['codex']);
    expect(cache.snapshot()).toEqual(['codex']);
  });
});
