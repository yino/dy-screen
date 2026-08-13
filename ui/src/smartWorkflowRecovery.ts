import type {
  AiSmartWorkflow,
  AiSmartWorkflowDetail,
  AiSmartWorkflowEvent,
  ClientApi,
} from "./types";

export interface SmartWorkflowSnapshot {
  workflows: AiSmartWorkflow[];
  details: Map<number, AiSmartWorkflowDetail>;
}

export interface SmartWorkflowRecovery {
  initial: SmartWorkflowSnapshot;
  dispose(): void;
}

function shouldRefresh(
  event: AiSmartWorkflowEvent,
  detail: AiSmartWorkflowDetail | undefined,
): boolean {
  if (!detail) return true;
  const current = detail.workflow;
  return (
    event.workflowGeneration > current.generation ||
    (event.workflowGeneration === current.generation && event.sequence > current.eventSequence)
  );
}

async function loadSnapshot(api: ClientApi): Promise<SmartWorkflowSnapshot> {
  const workflows = await api.listSmartWorkflows();
  const details = new Map<number, AiSmartWorkflowDetail>();
  await Promise.all(
    workflows.map(async (workflow) => {
      details.set(workflow.id, await api.getSmartWorkflow(workflow.id));
    }),
  );
  return { workflows, details };
}

/**
 * Events are only refresh hints. Query before subscribing, subscribe, then query once more
 * to close the race between the first snapshot and listener registration.
 */
export async function recoverSmartWorkflows(
  api: ClientApi,
  onSnapshot: (snapshot: SmartWorkflowSnapshot) => void,
): Promise<SmartWorkflowRecovery> {
  let disposed = false;
  let snapshot = await loadSnapshot(api);
  if (!disposed) onSnapshot(snapshot);

  const refreshOne = async (workflowId: number) => {
    if (disposed) return;
    const detail = await api.getSmartWorkflow(workflowId);
    if (disposed) return;
    const current = snapshot.details.get(workflowId)?.workflow;
    if (
      current &&
      (detail.workflow.generation < current.generation ||
        (detail.workflow.generation === current.generation &&
          detail.workflow.eventSequence < current.eventSequence))
    ) {
      return;
    }
    const details = new Map(snapshot.details);
    details.set(workflowId, detail);
    const workflows = Array.from(details.values())
      .map((item) => item.workflow)
      .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt) || right.id - left.id);
    snapshot = { workflows, details };
    onSnapshot(snapshot);
  };

  const unlisten = await api.subscribeSmartWorkflows((event) => {
    if (disposed || !shouldRefresh(event, snapshot.details.get(event.workflowId))) return;
    void refreshOne(event.workflowId);
  });

  const afterSubscribe = await loadSnapshot(api);
  if (!disposed) {
    for (const [workflowId, detail] of afterSubscribe.details) {
      const current = snapshot.details.get(workflowId)?.workflow;
      if (
        !current ||
        detail.workflow.generation > current.generation ||
        (detail.workflow.generation === current.generation &&
          detail.workflow.eventSequence >= current.eventSequence)
      ) {
        snapshot.details.set(workflowId, detail);
      }
    }
    snapshot = {
      workflows: Array.from(snapshot.details.values())
        .map((item) => item.workflow)
        .sort((left, right) => right.updatedAt.localeCompare(left.updatedAt) || right.id - left.id),
      details: new Map(snapshot.details),
    };
    onSnapshot(snapshot);
  }

  return {
    initial: snapshot,
    dispose() {
      disposed = true;
      unlisten();
    },
  };
}
