import { NavLink } from "react-router-dom";

interface NavProps {
  isAdmin: boolean;
}

export function Nav({ isAdmin }: NavProps) {
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
        {isAdmin ? (
          <NavLink
            to="/settings"
            className={({ isActive }) =>
              `app-nav-link${isActive ? " active" : ""}`
            }
          >
            {"\u8a2d\u5b9a"}
          </NavLink>
        ) : null}
      </div>
    </nav>
  );
}
