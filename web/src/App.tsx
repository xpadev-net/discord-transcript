import { useEffect, useState } from "react";
import { Route, Routes } from "react-router-dom";
import { ForbiddenState } from "./components/ForbiddenState";
import { Nav } from "./components/Nav";
import { fetchMe } from "./lib/api";
import type { MeResponse } from "./lib/types";
import { DashboardPage } from "./pages/DashboardPage";
import { MeetingPage } from "./pages/MeetingPage";
import { SettingsPage } from "./pages/SettingsPage";

export function App() {
  const [me, setMe] = useState<MeResponse | null>(null);
  const [loadingMe, setLoadingMe] = useState(true);
  const [sessionForbidden, setSessionForbidden] = useState(false);
  const [sessionError, setSessionError] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    setLoadingMe(true);
    setMe(null);
    setSessionForbidden(false);
    setSessionError(false);

    fetchMe(controller.signal)
      .then((response) => {
        if (!controller.signal.aborted) {
          setMe(response);
        }
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) {
          return;
        }
        if (err instanceof Error && err.message === "forbidden") {
          setSessionForbidden(true);
        } else {
          setSessionError(true);
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoadingMe(false);
        }
      });

    return () => controller.abort();
  }, []);

  const isAdmin = me?.is_admin === true;

  return (
    <>
      <Nav isAdmin={isAdmin} />
      <Routes>
        <Route path="/" element={<DashboardPage />} />
        <Route
          path="/settings"
          element={
            <SettingsRoute
              isAdmin={isAdmin}
              loading={loadingMe}
              forbidden={sessionForbidden}
              error={sessionError}
            />
          }
        />
        <Route path="/meetings/:meetingId" element={<MeetingPage />} />
        <Route
          path="*"
          element={
            <div className="empty-state">
              {
                "\u4f1a\u8b70\u3092\u9078\u629e\u3057\u3066\u304f\u3060\u3055\u3044"
              }
            </div>
          }
        />
      </Routes>
    </>
  );
}

interface SettingsRouteProps {
  isAdmin: boolean;
  loading: boolean;
  forbidden: boolean;
  error: boolean;
}

function SettingsRoute({
  isAdmin,
  loading,
  forbidden,
  error,
}: SettingsRouteProps) {
  if (loading) {
    return (
      <main className="settings-page">
        <output className="loading settings-panel-message">
          <span className="loading-spinner" />
          {"\u8aad\u307f\u8fbc\u307f\u4e2d"}
        </output>
      </main>
    );
  }

  if (error) {
    return (
      <main className="settings-page">
        <div className="panel-error settings-panel-message" role="alert">
          {
            "\u6a29\u9650\u60c5\u5831\u3092\u78ba\u8a8d\u3067\u304d\u307e\u305b\u3093\u3067\u3057\u305f"
          }
        </div>
      </main>
    );
  }

  if (forbidden || !isAdmin) {
    return (
      <main className="settings-page">
        <ForbiddenState />
      </main>
    );
  }

  return <SettingsPage />;
}
