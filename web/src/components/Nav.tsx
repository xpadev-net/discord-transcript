import { NavLink } from "react-router-dom";

export function Nav() {
  return (
    <nav className="app-nav" aria-label="Primary">
      <div className="app-nav-content">
        <NavLink
          to="/"
          end
          className={({ isActive }) =>
            `app-nav-link${isActive ? " active" : ""}`
          }
        >
          {"\u4f1a\u8b70\u4e00\u89a7"}
        </NavLink>
        <span className="app-nav-link disabled" aria-disabled="true">
          {"\u8a2d\u5b9a"}
        </span>
      </div>
    </nav>
  );
}
