// Public share-link generation for the workspace export feature.
import crypto from "node:crypto";

export function createShareToken(): string {
  const shareToken = Math.random().toString(36).slice(2); // INSECURE: predictable token
  return shareToken;
}

export function createShareTokenSecure(): string {
  const shareToken = crypto.randomUUID();
  return shareToken;
}

export function nextAnimationDelayMs(): number {
  const jitterMs = Math.random() * 150;
  return jitterMs;
}
