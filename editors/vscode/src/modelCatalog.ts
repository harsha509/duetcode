/**
 * Live model discovery for the settings webview's "Refresh" button.
 *
 * Claude uses the Anthropic Models API, which requires an API key — the claude
 * CLI exposes no model-listing command, so CLI-only setups have no live list
 * (they track the sonnet/opus/haiku aliases plus `claude update` instead).
 * Gemini uses the Generative Language API with the stored Gemini key.
 */

const ANTHROPIC_MODELS_URL = 'https://api.anthropic.com/v1/models?limit=1000';
const GEMINI_MODELS_URL =
  'https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000';
const ANTHROPIC_VERSION = '2023-06-01';

/** Selectable Claude model ids from the Anthropic Models API (`GET /v1/models`). */
export async function fetchClaudeModels(apiKey: string): Promise<string[]> {
  const res = await fetch(ANTHROPIC_MODELS_URL, {
    headers: { 'x-api-key': apiKey, 'anthropic-version': ANTHROPIC_VERSION },
  });
  if (!res.ok) {
    throw new Error(`Anthropic Models API returned ${res.status}`);
  }
  const body = (await res.json()) as { data?: Array<{ id?: unknown }> };
  return (body.data ?? [])
    .map((m) => m.id)
    .filter((id): id is string => typeof id === 'string');
}

/** Selectable Gemini model ids (those supporting `generateContent`), stripped
 *  of the `models/` prefix so they match what the gemini adapter expects. */
export async function fetchGeminiModels(apiKey: string): Promise<string[]> {
  const res = await fetch(GEMINI_MODELS_URL, {
    headers: { 'x-goog-api-key': apiKey },
  });
  if (!res.ok) {
    throw new Error(`Gemini Models API returned ${res.status}`);
  }
  const body = (await res.json()) as {
    models?: Array<{ name?: unknown; supportedGenerationMethods?: unknown }>;
  };
  return (body.models ?? [])
    .filter(
      (m) =>
        Array.isArray(m.supportedGenerationMethods) &&
        m.supportedGenerationMethods.includes('generateContent'),
    )
    .map((m) => (typeof m.name === 'string' ? m.name.replace(/^models\//, '') : undefined))
    .filter((id): id is string => typeof id === 'string');
}
