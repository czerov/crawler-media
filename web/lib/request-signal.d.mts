export interface RequestSignalLease {
  readonly signal: AbortSignal;
  release(): void;
  isLocalTimeout(): boolean;
}

export function linkRequestSignal(
  externalSignal?: AbortSignal | null,
  timeoutMs?: number
): RequestSignalLease;
