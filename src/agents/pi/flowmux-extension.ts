import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const endpoint = __FLOWMUX_ENDPOINT__;
const agentId = __FLOWMUX_AGENT_ID__;

function assistantText(message: any): string | undefined {
  if (message?.role !== "assistant") return undefined;
  if (typeof message.content === "string") return message.content;
  if (!Array.isArray(message.content)) return undefined;
  const text = message.content
    .filter((part: any) => part?.type === "text" && typeof part.text === "string")
    .map((part: any) => part.text)
    .join("");
  return text || undefined;
}

async function report(event: string, ctx: any, extra: Record<string, unknown> = {}) {
  const usage = ctx.getContextUsage?.();
  const model = ctx.model ? `${ctx.model.provider}/${ctx.model.id}` : undefined;
  const body = {
    event,
    session_id: ctx.sessionManager?.getSessionId?.(),
    model_name: model,
    context_used: usage?.tokens ?? undefined,
    context_total: usage?.contextWindow ?? undefined,
    is_idle: ctx.isIdle?.(),
    ...extra,
  };
  try {
    await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json", "x-flowmux-agent-id": agentId },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(1_000),
    });
  } catch {
    // Flowmux may be restarting; status reporting must never affect Pi.
  }
}

export default function (pi: ExtensionAPI) {
  let generation = 0;
  let lastContext: any;
  // A session can outlive Flowmux. Periodically re-send its state after the
  // callback server is restarted, without keeping Pi alive at shutdown.
  const heartbeat = setInterval(() => {
    if (lastContext) void report("heartbeat", lastContext);
  }, 5_000) as ReturnType<typeof setInterval> & { unref?: () => void };
  heartbeat.unref?.();
  pi.on("session_start", async (_event, ctx) => {
    lastContext = ctx;
    await report("session_start", ctx);
  });
  pi.on("input", async (event, ctx) => {
    lastContext = ctx;
    await report("input", ctx, { first_prompt: event.text });
  });
  pi.on("agent_start", async (_event, ctx) => {
    lastContext = ctx;
    await report("agent_start", ctx, { generation: ++generation });
  });
  pi.on("message_update", async (_event, ctx) => {
    lastContext = ctx;
    await report("message_update", ctx);
  });
  pi.on("message_end", async (event, ctx) => {
    lastContext = ctx;
    const last_model_response = assistantText(event.message);
    if (last_model_response) await report("message_end", ctx, { last_model_response });
  });
  pi.on("model_select", async (event, ctx) => {
    lastContext = ctx;
    await report("model_select", ctx, { model_name: `${event.model.provider}/${event.model.id}` });
  });
  pi.on("agent_end", async (_event, ctx) => {
    lastContext = ctx;
    const endedGeneration = generation;
    setTimeout(() => void report("agent_end", ctx, { generation: endedGeneration }), 250);
  });
  pi.on("session_shutdown", async (event, ctx) => {
    lastContext = ctx;
    if (event.reason === "quit") await report("session_shutdown", ctx, { reason: event.reason });
  });
}
