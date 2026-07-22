import { Component, type ErrorInfo, type ReactNode } from "react";

// A render/effect error in any child (most often the embedded xterm terminal) used to blank the
// entire app — React unmounts to the root when there's no boundary. This scopes such a crash to
// the detail panel: the board stays usable and the actual error is surfaced instead of a white
// screen. `resetKey` (the selected ticket id) re-mounts the boundary on ticket switch so a crash
// on one ticket doesn't stick when you open another.
type Props = { children: ReactNode; onClose?: () => void; resetKey?: unknown };
type State = { error: Error | null };

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("Detail panel crashed:", error, info.componentStack);
  }

  componentDidUpdate(prev: Props) {
    if (prev.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: null });
    }
  }

  render() {
    if (this.state.error) {
      return (
        <div className="panel-error">
          <div className="panel-error-head">Something went wrong displaying this ticket</div>
          <pre className="panel-error-msg">{String(this.state.error.message || this.state.error)}</pre>
          <div className="panel-error-actions">
            <button onClick={() => this.setState({ error: null })}>Try again</button>
            {this.props.onClose && <button onClick={this.props.onClose}>Close</button>}
          </div>
        </div>
      );
    }
    return this.props.children;
  }
}
