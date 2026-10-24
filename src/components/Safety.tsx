import { ReactNode } from "react";
import { SessionInfo } from "../lib/api";
import { Modal } from "./ui";

/** The red strip across the top of a production connection's workspace. */
export function ProductionBar({ info }: { info: SessionInfo }) {
  return (
    <div className="production-bar" role="status">
      <b>PRODUCTION</b>
      <span>{info.name}</span>
      <div className="spacer" />
      <span>{info.readOnly ? "Read-only" : "Changes ask for confirmation"}</span>
    </div>
  );
}

/** Small PROD / READ-ONLY labels for connection lists and headers. */
export function EnvBadges(props: { production?: boolean; readOnly?: boolean; short?: boolean }) {
  return (
    <>
      {props.production && (
        <span className="env-badge production" title="Production connection: writes ask for confirmation">
          {props.short ? "PROD" : "PRODUCTION"}
        </span>
      )}
      {props.readOnly && (
        <span className="env-badge read-only" title="Writes are refused on this connection">
          READ-ONLY
        </span>
      )}
    </>
  );
}

/**
 * Asks before running statements: writes on production, or UPDATE/DELETE
 * without WHERE. Cancel has the focus, so Enter or Escape backs out.
 */
export function ConfirmStatements(props: {
  title: ReactNode;
  children: ReactNode;
  statements: string[];
  confirmLabel: string;
  busy?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Modal
      title={props.title}
      wide
      onClose={props.onCancel}
      footer={
        <>
          <button onClick={() => navigator.clipboard.writeText(props.statements.join("\n"))}>Copy</button>
          <div className="spacer" />
          <button onClick={props.onCancel} autoFocus>Cancel</button>
          <button className="primary danger-fill" onClick={props.onConfirm} disabled={props.busy}>
            {props.confirmLabel}
          </button>
        </>
      }
    >
      <div style={{ marginBottom: 10 }}>{props.children}</div>
      <pre className="sql-preview">{props.statements.join("\n")}</pre>
    </Modal>
  );
}
