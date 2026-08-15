import { describe, expect, it, vi } from "vitest";
import { recoverSmartWorkflows } from "./smartWorkflowRecovery";
import type {
  AiSmartWorkflowDetail,
  AiSmartWorkflowEvent,
  ClientApi,
} from "./types";

function detail(generation: number, sequence: number): AiSmartWorkflowDetail {
  return {
    workflow: {
      id: 7,
      name: "智能成片",
      mode: "local",
      status: sequence >= 2 ? "review_ready" : "running",
      stage: sequence >= 2 ? "review" : "asr",
      generation,
      sourceSessionId: null,
      sourceSummary: "1 个本地视频",
      provider: "deepseek",
      modelId: "deepseek-chat",
      textScope: "selected_clip_subtitles",
      authorizationDigest: "digest",
      authorizedAt: "2026-08-14T00:00:00Z",
      configurationFingerprint: "config",
      activeDraftGeneration: sequence >= 2 ? 1 : null,
      liveCursorVideoId: null,
      liveStartVideoId: null,
      eventSequence: sequence,
      candidateCount: sequence >= 2 ? 1 : 0,
      selectedCount: sequence >= 2 ? 1 : 0,
      pendingBatchCount: sequence >= 2 ? 0 : 1,
      lastErrorCode: null,
      lastErrorMessage: null,
      createdAt: "2026-08-14T00:00:00Z",
      updatedAt: `2026-08-14T00:00:0${sequence}Z`,
    },
    batches: [],
    attempts: [],
    drafts: [],
    frozenInputCount: 1,
    processedInputCount: sequence >= 2 ? 1 : 0,
  };
}

describe("recoverSmartWorkflows", () => {
  it("先查快照再订阅并补查，过滤旧代次和旧序号事件", async () => {
    const order: string[] = [];
    let listener: ((event: AiSmartWorkflowEvent) => void) | undefined;
    let current = detail(1, 1);
    const api = {
      listSmartWorkflows: vi.fn(async () => {
        order.push("list");
        return [current.workflow];
      }),
      getSmartWorkflow: vi.fn(async () => {
        order.push("get");
        return current;
      }),
      subscribeSmartWorkflows: vi.fn(async (next) => {
        order.push("subscribe");
        listener = next;
        current = detail(1, 2);
        return () => order.push("unlisten");
      }),
    } as unknown as ClientApi;
    const snapshots: AiSmartWorkflowDetail[] = [];
    const recovery = await recoverSmartWorkflows(api, (snapshot) => {
      const value = snapshot.details.get(7);
      if (value) snapshots.push(value);
    });

    expect(order.slice(0, 5)).toEqual(["list", "get", "subscribe", "list", "get"]);
    expect(snapshots.at(-1)?.workflow.eventSequence).toBe(2);

    listener?.({
      workflowId: 7,
      workflowGeneration: 1,
      batchId: null,
      stage: "asr",
      sequence: 1,
      status: "running",
      errorCode: null,
      errorMessage: null,
    });
    listener?.({
      workflowId: 7,
      workflowGeneration: 0,
      batchId: null,
      stage: "asr",
      sequence: 99,
      status: "running",
      errorCode: null,
      errorMessage: null,
    });
    await Promise.resolve();
    expect(api.getSmartWorkflow).toHaveBeenCalledTimes(2);

    current = detail(2, 1);
    listener?.({
      workflowId: 7,
      workflowGeneration: 2,
      batchId: null,
      stage: "review",
      sequence: 1,
      status: "review_ready",
      errorCode: null,
      errorMessage: null,
    });
    await vi.waitFor(() => expect(api.getSmartWorkflow).toHaveBeenCalledTimes(3));
    expect(snapshots.at(-1)?.workflow.generation).toBe(2);

    recovery.dispose();
    expect(order.at(-1)).toBe("unlisten");
  });
});
