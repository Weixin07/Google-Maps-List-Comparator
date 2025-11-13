import type { FoundationHealth } from "./types/foundation";

export type ChecklistStatus = "pending" | "done";

export interface ChecklistItem {
  id: string;
  label: string;
  note: string;
  status: ChecklistStatus;
}

export interface ChecklistContext {
  foundationHealth: FoundationHealth | null;
  isDevMode: boolean;
}

export function resolveChecklist(
  template: ChecklistItem[],
  context: ChecklistContext,
): ChecklistItem[] {
  return template.map((item) => {
    switch (item.id) {
      case "sqlcipher":
        return withStatus(
          item,
          context.foundationHealth?.has_encryption_key ? "done" : item.status,
        );
      case "telemetry":
        return withStatus(item, context.foundationHealth ? "done" : item.status);
      case "ci":
        return withStatus(item, context.isDevMode ? "pending" : item.status);
      default:
        return item;
    }
  });
}

function withStatus(item: ChecklistItem, status: ChecklistStatus): ChecklistItem {
  if (item.status === status) {
    return item;
  }
  return {
    ...item,
    status,
  };
}
