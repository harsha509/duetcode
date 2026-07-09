import * as vscode from 'vscode';

export const SECRET_ANTHROPIC = 'dt.anthropicKey';
export const SECRET_GEMINI = 'dt.geminiKey';

/** Env injected into `dt serve` from securely stored keys. */
export async function secretEnv(ctx: vscode.ExtensionContext): Promise<Record<string, string>> {
  const env: Record<string, string> = {};
  const anthropic = await ctx.secrets.get(SECRET_ANTHROPIC);
  const gemini = await ctx.secrets.get(SECRET_GEMINI);
  if (anthropic) {
    env.ANTHROPIC_API_KEY = anthropic;
  }
  if (gemini) {
    env.GEMINI_API_KEY = gemini;
  }
  return env;
}
