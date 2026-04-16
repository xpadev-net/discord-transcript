import { Route, Routes } from "react-router-dom";
import { MeetingPage } from "./pages/MeetingPage";

export function App() {
  return (
    <Routes>
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
  );
}
