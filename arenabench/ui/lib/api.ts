// Thin fetch wrapper over the arena's same-origin JSON API. The server
// answers errors as {"error": "..."} with a non-2xx status.

export async function api<T = unknown>(path: string, options?: RequestInit): Promise<T> {
  const response = await fetch(path, options);
  const body = await response.json().catch(() => ({}));
  if (!response.ok) {
    const message =
      (body as { error?: string }).error || `${response.status} ${response.statusText}`;
    throw new Error(message);
  }
  return body as T;
}

export function postJson<T = unknown>(path: string, payload: unknown): Promise<T> {
  return api<T>(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
}
