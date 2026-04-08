import { Routes, Route } from "react-router-dom";
import { MeetingPage } from "./pages/MeetingPage";

export function App() {
  return (
    <Routes>
      <Route path="/meetings/:meetingId" element={<MeetingPage />} />
    </Routes>
  );
}
