import { Component, type ErrorInfo, type ReactNode } from "react";

interface Props {
  children: ReactNode;
  title?: string;
}

interface State {
  hasError: boolean;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { hasError: false };

  static getDerivedStateFromError(): State {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error(error, info.componentStack);
  }

  private handleRetry = (): void => {
    window.location.reload();
  };

  render() {
    if (this.state.hasError) {
      return (
        <div className="panel-error" role="alert">
          <div>
            {this.props.title ??
              "\u8868\u793a\u30a8\u30e9\u30fc\u304c\u767a\u751f\u3057\u307e\u3057\u305f"}
          </div>
          <button type="button" onClick={this.handleRetry}>
            {"\u518d\u8aad\u307f\u8fbc\u307f"}
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
