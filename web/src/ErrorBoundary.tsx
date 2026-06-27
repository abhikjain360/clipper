import { Component, type ReactNode } from "react";

interface ErrorBoundaryProps {
    children: ReactNode;
    // Render-prop fallback so each caller can theme the error UI to its slot.
    // Receives the caught error and a `reset` that re-attempts the subtree.
    fallback: (error: Error, reset: () => void) => ReactNode;
    // Optional hook so a caller can surface the message in its own error banner.
    onError?: (error: Error) => void;
}

interface ErrorBoundaryState {
    error: Error | null;
}

// Catches render/commit-phase throws from its subtree so a single failing
// component (e.g. a lazily-loaded editor chunk that fails to mount) renders a
// local fallback instead of unmounting the whole React tree to a blank screen.
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
    state: ErrorBoundaryState = { error: null };

    static getDerivedStateFromError(error: Error): ErrorBoundaryState {
        return { error };
    }

    componentDidCatch(error: Error): void {
        this.props.onError?.(error);
    }

    reset = (): void => {
        this.setState({ error: null });
    };

    render(): ReactNode {
        const { error } = this.state;
        if (error) {
            return this.props.fallback(error, this.reset);
        }
        return this.props.children;
    }
}
