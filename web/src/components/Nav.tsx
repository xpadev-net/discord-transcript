import { Link, useLocation } from "react-router-dom";

export function Nav() {
  const location = useLocation();
  const isDashboard = location.pathname === "/" || location.pathname === "/meetings" || location.pathname.startsWith("/meetings/");

  return (
    <nav className="nav">
      <div className="nav-brand">
        <Link to="/">discord transcript</Link>
      </div>
      <div className="nav-links">
        <Link 
          to="/" 
          className={location.pathname === "/" ? "active" : ""}
        >
          会議一覧
        </Link>
        <Link 
          to="/settings" 
          className={location.pathname === "/settings" ? "active" : ""}
        >
          設定
        </Link>
      </div>
    </nav>
  );
}
