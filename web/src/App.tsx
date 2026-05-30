import { Route, Routes } from "react-router-dom";
import { Nav } from "./components/Nav";
import { MeetingPage } from "./pages/MeetingPage";
import { DashboardPage } from "./pages/DashboardPage";
import { SettingsPage } from "./pages/SettingsPage";

export function App() {
  return (
    <div className="app">
      <Nav />
      <main>
        <Routes>
          <Route path="/" element={<DashboardPage />} />
          <Route path="/meetings/:meetingId" element={<MeetingPage />} />
          <Route path="/settings" element={<SettingsPage />} />
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
      </main>
    </div>
  );
}
