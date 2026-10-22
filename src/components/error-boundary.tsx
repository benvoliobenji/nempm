import { Component, type ReactNode } from "react";

/** Last line of defence: a render error shows a way out instead of a blank window. */
export class ErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error) {
    console.error("UI error:", error);
  }

  render() {
    if (!this.state.error) return this.props.children;
    return (
      <div className="grid h-full place-items-center bg-background p-8">
        <div className="max-w-md text-center">
          <h1 className="text-lg font-semibold">Something went wrong</h1>
          <p className="mt-2 text-[13px] text-muted-foreground">
            The window hit an unexpected error. Nothing on your drive was changed by it. Reload to continue.
          </p>
          <pre className="selectable mt-4 max-h-40 overflow-auto rounded-lg bg-foreground/[0.06] p-3 text-left text-[11px]">
            {this.state.error.message}
          </pre>
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="mt-4 h-8 rounded-md bg-primary px-4 text-[13px] font-medium text-primary-foreground"
          >
            Reload
          </button>
        </div>
      </div>
    );
  }
}
