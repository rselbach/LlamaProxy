export const shouldShowOAuthLoginStatus = (status: string) => status === 'success';

export const createOAuthLoginSuccessCache = <T extends string>() => {
  const providers = new Set<T>();

  return {
    mark(provider: T) {
      providers.add(provider);
    },
    snapshot() {
      return Array.from(providers);
    },
  };
};
