import { Route, Routes } from "react-router-dom";
import { Nav } from "./components/Nav";
import { DashboardPage } from "./pages/DashboardPage";
import { MeetingPage } from "./pages/MeetingPage";
import { SettingsPage } from "./pages/SettingsPage";

export function App() {
  return (
    <>
      <Nav />
      <Routes>
        <Route path="/" element={<DashboardPage />} />
        <Route path="/settings" element={<SettingsPage />} />
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
